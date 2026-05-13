import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { PocketIc, PocketIcServer, createIdentity } from "@dfinity/pic";
import { idlFactory } from "./idl.mjs";

const wasm = resolve("target/pocketic/ic_sqlite_vfs.wasm");
const v1Wasm = resolve("target/pocketic/ic_sqlite_vfs_v1.wasm");
const failpointWasm = resolve("target/pocketic/ic_sqlite_vfs_failpoints.wasm");
const timeout = 600_000;

test("PocketIC persistence and failpoint regressions", { timeout }, async () => {
  const server = await PocketIcServer.start();
  const pic = await PocketIc.create(server.getUrl());
  try {
    await stableImageSurvivesUpgrade(pic);
    await schemaMigrationSurvivesSecondUpgrade(pic);
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
  assert.deepEqual(await actor.kv_get("survives"), { Ok: ["before-upgrade"] });
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

async function schemaMigrationSurvivesSecondUpgrade(pic) {
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm: v1Wasm });

  assert.deepEqual(await actor.kv_put("migrates", "v1"), { Ok: null });
  assert.deepEqual(await actor.kv_get("migrates"), { Ok: ["v1"] });
  const before = await actor.db_meta();
  assert.equal("Ok" in before, true);
  assert.equal(before.Ok.schema_version, 1n);

  await pic.upgradeCanister({ canisterId, wasm });
  const migrated = pic.createActor(idlFactory, canisterId);

  assert.deepEqual(await migrated.kv_get("migrates"), { Ok: ["v1"] });
  assert.deepEqual(await migrated.kv_set_note("migrates", "v2-note"), { Ok: null });
  assert.deepEqual(await migrated.kv_get_note("migrates"), { Ok: ["v2-note"] });
  assert.deepEqual(await migrated.db_integrity_check(), { Ok: "ok" });
  const afterMigration = await migrated.db_meta();
  assert.equal("Ok" in afterMigration, true);
  assert.equal(afterMigration.Ok.schema_version, 2n);

  await pic.upgradeCanister({ canisterId, wasm });
  const upgradedAgain = pic.createActor(idlFactory, canisterId);

  assert.deepEqual(await upgradedAgain.kv_get("migrates"), { Ok: ["v1"] });
  assert.deepEqual(await upgradedAgain.kv_get_note("migrates"), { Ok: ["v2-note"] });
  assert.deepEqual(await upgradedAgain.db_integrity_check(), { Ok: "ok" });
  const afterSecondUpgrade = await upgradedAgain.db_meta();
  assert.equal("Ok" in afterSecondUpgrade, true);
  assert.equal(afterSecondUpgrade.Ok.schema_version, 2n);
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

  assert.equal("Err" in denied, true);
  assert.match(denied.Err, /not a controller/);
  assert.equal("Err" in deniedRefresh, true);
  assert.match(deniedRefresh.Err, /not a controller/);
  assert.equal("Err" in deniedChunk, true);
  assert.match(deniedChunk.Err, /not a controller/);
}
