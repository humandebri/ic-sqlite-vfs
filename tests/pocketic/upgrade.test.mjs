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
  step(name, "query input length guards");
  const maxKey = "k".repeat(256);
  const tooLongKey = "k".repeat(257);
  const maxValue = "v".repeat(64 * 1024);
  const tooLongValue = "v".repeat(64 * 1024 + 1);
  const maxNote = "n".repeat(4 * 1024);
  const tooLongNote = "n".repeat(4 * 1024 + 1);
  assert.deepEqual(await actor.kv_put(maxKey, maxValue), { Ok: null });
  assert.deepEqual(await actor.kv_get(maxKey), { Ok: [maxValue] });
  assert.deepEqual(await actor.kv_set_note(maxKey, maxNote), { Ok: null });
  assert.deepEqual(await actor.kv_get_note(maxKey), { Ok: [maxNote] });
  for (const result of [
    await actor.kv_put(tooLongKey, "value"),
    await actor.kv_put("too-long-value", tooLongValue),
    await actor.kv_get(tooLongKey),
    await actor.kv_get_many(["other", tooLongKey]),
    await actor.kv_set_note(tooLongKey, "note"),
    await actor.kv_set_note("survives", tooLongNote),
    await actor.kv_get_note(tooLongKey),
  ]) {
    assert.equal("Err" in result, true);
    assert.match(result.Err, /at most/);
  }
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
  step(name, "snapshot committed metadata");
  const beforeMeta = await actor.db_meta();
  assert.equal("Ok" in beforeMeta, true);
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
}

async function precompiledSqliteArchiveHasExpectedFeatures(pic, name) {
  step(name, "setup failpoint canister");
  const { actor } = await pic.setupCanister({ idlFactory, wasm: failpointWasm });

  step(name, "run sqlite feature probe");
  assert.deepEqual(await actor.db_test_sqlite_feature_probe(), { Ok: null });
}

async function managementMethodsRequireController(pic, name) {
  step(name, "setup canister");
  const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });
  step(name, "controller methods succeed");
  assert.deepEqual(await actor.kv_put("controller", "value"), { Ok: null });
  assert.deepEqual(await actor.kv_set_note("controller", "note"), { Ok: null });
  assert.deepEqual(await actor.kv_count(), { Ok: 1n });
  assert.equal("Ok" in await actor.db_meta(), true);
  const oneShotRefresh = await actor.db_refresh_checksum();
  assert.equal("Err" in oneShotRefresh, true);
  assert.match(oneShotRefresh.Err, /use db_refresh_checksum_chunk/);
  const chunk = await actor.db_refresh_checksum_chunk(64n);
  assert.equal("Ok" in chunk, true);
  step(name, "controller methods reject oversized work");
  await expectErr(
    "zero checksum refresh chunk",
    actor.db_refresh_checksum_chunk(0n),
    /greater than zero/,
  );
  await expectErr(
    "large checksum refresh chunk",
    actor.db_refresh_checksum_chunk(4n * 1024n * 1024n + 1n),
    /at most/,
  );

  step(name, "non-controller methods fail");
  const attacker = pic.createActor(idlFactory, canisterId);
  attacker.setIdentity(createIdentity("not-a-controller"));
  const deniedPut = await attacker.kv_put("attacker", "bad");
  const deniedNote = await attacker.kv_set_note("controller", "bad");
  const deniedCount = await attacker.kv_count();
  const deniedRefresh = await attacker.db_refresh_checksum();
  const deniedChunk = await attacker.db_refresh_checksum_chunk(64n);
  assert.deepEqual(await attacker.kv_get("controller"), { Ok: ["value"] });
  assert.deepEqual(await attacker.kv_get_many(["controller", "missing"]), {
    Ok: [["value"], []],
  });
  assert.deepEqual(await attacker.kv_get_note("controller"), { Ok: ["note"] });

  assert.equal("Err" in deniedPut, true);
  assert.match(deniedPut.Err, /not a controller/);
  assert.equal("Err" in deniedNote, true);
  assert.match(deniedNote.Err, /not a controller/);
  assert.equal("Err" in deniedCount, true);
  assert.match(deniedCount.Err, /not a controller/);
  assert.equal("Err" in deniedRefresh, true);
  assert.match(deniedRefresh.Err, /not a controller/);
  assert.equal("Err" in deniedChunk, true);
  assert.match(deniedChunk.Err, /not a controller/);
}

async function refreshChecksumByChunks(actor, maxChunkSize) {
  let refresh;
  do {
    refresh = await expectOk(
      "refresh checksum chunk",
      actor.db_refresh_checksum_chunk(maxChunkSize),
    );
  } while (!refresh.complete);
  return refresh;
}

async function expectErr(name, promise, pattern) {
  const result = await promise;
  assert.equal("Err" in result, true, `${name} unexpectedly succeeded`);
  assert.match(result.Err, pattern);
  return result.Err;
}

async function expectOk(name, promise) {
  const result = await promise;
  assert.equal("Ok" in result, true, `${name} failed: ${result.Err}`);
  return result.Ok;
}
