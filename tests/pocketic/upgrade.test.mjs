import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { PocketIc, createIdentity } from "@dfinity/pic";
import { idlFactory } from "./idl.mjs";
import { startPocketIcServer } from "./server.mjs";

const wasm = resolve("target/pocketic/ic_sqlite_vfs.wasm");
const failpointWasm = resolve("target/pocketic/ic_sqlite_vfs_failpoints.wasm");
const timeout = 600_000;
const serverStartTimeout = 120_000;

test("PocketIC stable image survives upgrade", { timeout }, async () => {
  await withPocketIc("stableImageSurvivesUpgrade", stableImageSurvivesUpgrade);
});

test("PocketIC precompiled SQLite archive exposes expected features", { timeout }, async () => {
  await withPocketIc(
    "precompiledSqliteArchiveHasExpectedFeatures",
    precompiledSqliteArchiveHasExpectedFeatures,
  );
});

test("PocketIC stable write trap rolls back failed update", { timeout }, async () => {
  await withPocketIc("stableWriteTrapRollsBackFailedUpdate", stableWriteTrapRollsBackFailedUpdate);
});

test("PocketIC chunked import rejects wrong checksum", { timeout }, async () => {
  await withPocketIc("chunkedImportRejectsWrongChecksum", chunkedImportRejectsWrongChecksum);
});

test("PocketIC management methods require controller", { timeout }, async () => {
  await withPocketIc("managementMethodsRequireController", managementMethodsRequireController);
});

async function withPocketIc(name, run) {
  step(name, "start server");
  const server = await startPocketIcServer({ timeoutMs: serverStartTimeout });
  let pic;
  let bodyError;
  let cleanupError;

  try {
    step(name, "create PocketIC client");
    pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
    await run(pic, name);
  } catch (error) {
    bodyError = error;
  }

  try {
    if (pic) {
      step(name, "tearDown PocketIC");
      await pic.tearDown();
    }
  } catch (error) {
    cleanupError = error;
  } finally {
    step(name, "stop server");
    try {
      await server.stop();
    } catch (error) {
      cleanupError ??= error;
    }
  }

  if (cleanupError) {
    console.error(`[pocketic:upgrade] ${name}: cleanup failed`, cleanupError);
  }
  if (bodyError) {
    throw bodyError;
  }
  if (cleanupError) {
    throw cleanupError;
  }
}

function step(name, message) {
  console.error(`[pocketic:upgrade] ${name}: ${message}`);
}

async function stableImageSurvivesUpgrade(pic, name) {
  step(name, "setup canister");
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });

  step(name, "write initial rows");
  assert.deepEqual(await actor.kv_put("survives", "before-upgrade"), { Ok: null });
  assert.deepEqual(await actor.kv_put("other", "second"), { Ok: null });
  step(name, "query initial rows");
  assert.deepEqual(await actor.kv_get("survives"), { Ok: ["before-upgrade"] });
  assert.deepEqual(await actor.kv_get_many(["other", "missing", "survives", "other"]), {
    Ok: [["second"], [], ["before-upgrade"], ["second"]],
  });
  step(name, "query too many keys guard");
  const tooMany = await actor.kv_get_many(Array.from({ length: 1001 }, (_, index) => `k${index}`));
  assert.equal("Err" in tooMany, true);
  assert.match(tooMany.Err, /at most 1000 keys/);
  step(name, "read metadata before upgrade");
  const before = await actor.db_meta();
  assert.equal("Ok" in before, true);
  assert.equal(before.Ok.checksum_stale, true);

  step(name, "upgrade canister");
  await pic.upgradeCanister({ canisterId, wasm });
  const upgraded = pic.createActor(idlFactory, canisterId);

  step(name, "verify upgraded image");
  assert.deepEqual(await upgraded.kv_get("survives"), { Ok: ["before-upgrade"] });
  assert.deepEqual(await upgraded.db_integrity_check(), { Ok: "ok" });
  const after = await upgraded.db_meta();
  assert.equal("Ok" in after, true);
  assert.equal(after.Ok.checksum, before.Ok.checksum);
  assert.equal(after.Ok.checksum_stale, before.Ok.checksum_stale);
  assert.equal(after.Ok.checksum_refreshing, false);
}

async function stableWriteTrapRollsBackFailedUpdate(pic, name) {
  step(name, "setup failpoint canister");
  const { actor } = await pic.setupCanister({ idlFactory, wasm: failpointWasm });

  step(name, "write baseline value");
  assert.deepEqual(await actor.kv_put("trap", "before"), { Ok: null });
  step(name, "snapshot committed image");
  const beforeMeta = await actor.db_meta();
  assert.equal("Ok" in beforeMeta, true);
  const beforeImage = await actor.db_export_chunk(0n, beforeMeta.Ok.db_size);
  assert.equal("Ok" in beforeImage, true);
  step(name, "enable stable write trap");
  assert.deepEqual(await actor.db_test_trap_after_stable_write(1n), { Ok: null });
  step(name, "trigger trapped update");
  await assert.rejects(
    async () => actor.kv_put("trap", "after"),
    /stable write failpoint|Canister.*trap|reject/i,
  );
  step(name, "clear failpoints");
  assert.deepEqual(await actor.db_test_clear_failpoints(), { Ok: null });

  step(name, "verify active image");
  assert.deepEqual(await actor.kv_get("trap"), { Ok: ["before"] });
  assert.deepEqual(await actor.db_integrity_check(), { Ok: "ok" });
  const afterMeta = await actor.db_meta();
  assert.deepEqual(afterMeta, beforeMeta);
  const afterImage = await actor.db_export_chunk(0n, beforeMeta.Ok.db_size);
  assert.deepEqual(afterImage, beforeImage);
}

async function precompiledSqliteArchiveHasExpectedFeatures(pic, name) {
  step(name, "setup failpoint canister");
  const { actor } = await pic.setupCanister({ idlFactory, wasm: failpointWasm });

  step(name, "run sqlite feature probe");
  assert.deepEqual(await actor.db_test_sqlite_feature_probe(), { Ok: null });
}

async function chunkedImportRejectsWrongChecksum(pic, name) {
  step(name, "setup canister");
  const { actor } = await pic.setupCanister({ idlFactory, wasm });
  step(name, "write source row");
  assert.deepEqual(await actor.kv_put("key", "value"), { Ok: null });
  step(name, "refresh checksum");
  const refreshed = await actor.db_refresh_checksum();
  assert.equal("Ok" in refreshed, true);

  step(name, "export current image");
  const meta = await actor.db_meta();
  assert.equal("Ok" in meta, true);
  assert.equal(meta.Ok.checksum, refreshed.Ok);
  assert.equal(meta.Ok.checksum_stale, false);
  const exported = await actor.db_export_chunk(0n, meta.Ok.db_size);
  assert.equal("Ok" in exported, true);

  step(name, "import with wrong checksum");
  assert.deepEqual(await actor.db_begin_import(meta.Ok.db_size, refreshed.Ok + 1n), { Ok: null });
  assert.deepEqual(await actor.db_import_chunk(0n, exported.Ok), { Ok: null });
  step(name, "finish import and expect mismatch");
  const finish = await actor.db_finish_import();

  assert.equal("Err" in finish, true);
  assert.match(finish.Err, /checksum mismatch/);
}

async function managementMethodsRequireController(pic, name) {
  step(name, "setup canister");
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });
  step(name, "controller methods succeed");
  assert.equal("Ok" in await actor.db_meta(), true);
  assert.equal("Ok" in await actor.db_refresh_checksum(), true);
  const chunk = await actor.db_refresh_checksum_chunk(64n);
  assert.equal("Ok" in chunk, true);

  step(name, "non-controller methods fail");
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
  step(name, "controller cancels import");
  assert.deepEqual(await actor.db_cancel_import(), { Ok: null });
}
