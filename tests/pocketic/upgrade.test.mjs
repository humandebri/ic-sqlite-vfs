import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { PocketIc, createIdentity } from "@dfinity/pic";
import { idlFactory } from "./idl.mjs";
import { startPocketIcServer } from "./server.mjs";

const wasm = resolve("target/pocketic/ic_sqlite_vfs.wasm");
const failpointWasm = resolve("target/pocketic/ic_sqlite_vfs_failpoints.wasm");
const timeout = 600_000;

test("PocketIC persistence and failpoint regressions", { timeout }, async () => {
  const server = await startPocketIcServer({ timeoutMs: timeout });
  const pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
  try {
    await stableImageSurvivesUpgrade(pic);
    await stableWriteTrapRollsBackFailedUpdate(pic);
    await chunkedImportRejectsWrongChecksum(pic);
    await managementMethodsRequireController(pic);
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

async function stableImageSurvivesUpgrade(pic) {
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });

  assert.deepEqual(await actor.kv_put("survives", "before-upgrade"), { Ok: null });
  assert.deepEqual(await actor.kv_put("other", "second"), { Ok: null });
  assert.deepEqual(await actor.kv_get("survives"), { Ok: ["before-upgrade"] });
  assert.deepEqual(await actor.kv_get_many(["other", "missing", "survives", "other"]), {
    Ok: [["second"], [], ["before-upgrade"], ["second"]],
  });
  const tooMany = await actor.kv_get_many(Array.from({ length: 1001 }, (_, index) => `k${index}`));
  assert.equal("Err" in tooMany, true);
  assert.match(tooMany.Err, /at most 1000 keys/);
  const before = await actor.db_meta();
  assert.equal("Ok" in before, true);
  assert.equal(before.Ok.checksum_stale, true);

  await pic.upgradeCanister({ canisterId, wasm });
  const upgraded = pic.createActor(idlFactory, canisterId);

  assert.deepEqual(await upgraded.kv_get("survives"), { Ok: ["before-upgrade"] });
  assert.deepEqual(await upgraded.db_integrity_check(), { Ok: "ok" });
  const after = await upgraded.db_meta();
  assert.equal("Ok" in after, true);
  assert.equal(after.Ok.checksum, before.Ok.checksum);
  assert.equal(after.Ok.checksum_stale, before.Ok.checksum_stale);
  assert.equal(after.Ok.checksum_refreshing, false);
}

async function stableWriteTrapRollsBackFailedUpdate(pic) {
  const { actor } = await pic.setupCanister({ idlFactory, wasm: failpointWasm });

  assert.deepEqual(await actor.kv_put("trap", "before"), { Ok: null });
  assert.deepEqual(await actor.db_test_trap_after_stable_write(1n), { Ok: null });
  await assert.rejects(
    async () => actor.kv_put("trap", "after"),
    /stable write failpoint|Canister.*trap|reject/i,
  );
  assert.deepEqual(await actor.db_test_clear_failpoints(), { Ok: null });

  assert.deepEqual(await actor.kv_get("trap"), { Ok: ["before"] });
  assert.deepEqual(await actor.db_integrity_check(), { Ok: "ok" });
}

async function chunkedImportRejectsWrongChecksum(pic) {
  const { actor } = await pic.setupCanister({ idlFactory, wasm });
  assert.deepEqual(await actor.kv_put("key", "value"), { Ok: null });
  const refreshed = await actor.db_refresh_checksum();
  assert.equal("Ok" in refreshed, true);

  const meta = await actor.db_meta();
  assert.equal("Ok" in meta, true);
  assert.equal(meta.Ok.checksum, refreshed.Ok);
  assert.equal(meta.Ok.checksum_stale, false);
  const exported = await actor.db_export_chunk(0n, meta.Ok.db_size);
  assert.equal("Ok" in exported, true);

  assert.deepEqual(await actor.db_begin_import(meta.Ok.db_size, refreshed.Ok + 1n), { Ok: null });
  assert.deepEqual(await actor.db_import_chunk(0n, exported.Ok), { Ok: null });
  const finish = await actor.db_finish_import();

  assert.equal("Err" in finish, true);
  assert.match(finish.Err, /checksum mismatch/);
}

async function managementMethodsRequireController(pic) {
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });
  assert.equal("Ok" in await actor.db_meta(), true);
  assert.equal("Ok" in await actor.db_refresh_checksum(), true);
  const chunk = await actor.db_refresh_checksum_chunk(64n);
  assert.equal("Ok" in chunk, true);

  const attacker = pic.createActor(idlFactory, canisterId);
  attacker.setIdentity(createIdentity("not-a-controller"));
  const denied = await attacker.db_export_chunk(0n, 1n);
  const deniedRefresh = await attacker.db_refresh_checksum();
  const deniedChunk = await attacker.db_refresh_checksum_chunk(64n);
  assert.deepEqual(await actor.db_begin_import(1n, 0n), { Ok: null });
  const deniedCancel = await attacker.db_cancel_import();

  assert.equal("Err" in denied, true);
  assert.match(denied.Err, /not a controller/);
  assert.equal("Err" in deniedRefresh, true);
  assert.match(deniedRefresh.Err, /not a controller/);
  assert.equal("Err" in deniedChunk, true);
  assert.match(deniedChunk.Err, /not a controller/);
  assert.equal("Err" in deniedCancel, true);
  assert.match(deniedCancel.Err, /not a controller/);
  assert.deepEqual(await actor.db_cancel_import(), { Ok: null });
}
