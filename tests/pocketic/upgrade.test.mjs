import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { PocketIc, PocketIcServer, createIdentity } from "@dfinity/pic";
import { idlFactory } from "./idl.mjs";

const wasm = resolve("target/wasm32-unknown-unknown/debug/ic_sqlite_vfs.wasm");

test("stable SQLite image survives canister upgrade", { timeout: 120_000 }, async () => {
  const server = await PocketIcServer.start();
  const pic = await PocketIc.create(server.getUrl());
  try {
    const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });

    assert.deepEqual(await actor.kv_put("survives", "before-upgrade"), { Ok: null });
    assert.deepEqual(await actor.kv_get("survives"), { Ok: ["before-upgrade"] });
    const before = await actor.db_meta();
    assert.equal("Ok" in before, true);

    await pic.upgradeCanister({ canisterId, wasm });
    const upgraded = pic.createActor(idlFactory, canisterId);

    assert.deepEqual(await upgraded.kv_get("survives"), { Ok: ["before-upgrade"] });
    assert.deepEqual(await upgraded.db_integrity_check(), { Ok: "ok" });
    const after = await upgraded.db_meta();
    assert.equal("Ok" in after, true);
    assert.equal(after.Ok.checksum, before.Ok.checksum);
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

test("chunked export and import reject wrong checksum", { timeout: 120_000 }, async () => {
  const server = await PocketIcServer.start();
  const pic = await PocketIc.create(server.getUrl());
  try {
    const { actor } = await pic.setupCanister({ idlFactory, wasm });
    assert.deepEqual(await actor.kv_put("key", "value"), { Ok: null });

    const meta = await actor.db_meta();
    assert.equal("Ok" in meta, true);
    const checksum = await actor.db_checksum();
    assert.equal("Ok" in checksum, true);
    const exported = await actor.db_export_chunk(0n, meta.Ok.db_size);
    assert.equal("Ok" in exported, true);

    assert.deepEqual(await actor.db_begin_import(meta.Ok.db_size, checksum.Ok + 1n), { Ok: null });
    assert.deepEqual(await actor.db_import_chunk(0n, exported.Ok), { Ok: null });
    const finish = await actor.db_finish_import();

    assert.equal("Err" in finish, true);
    assert.match(finish.Err, /checksum mismatch/);
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

test("management database methods require a controller", { timeout: 120_000 }, async () => {
  const server = await PocketIcServer.start();
  const pic = await PocketIc.create(server.getUrl());
  try {
    const { actor, canisterId } = await pic.setupCanister({ idlFactory, wasm });
    assert.equal("Ok" in await actor.db_meta(), true);

    const attacker = pic.createActor(idlFactory, canisterId);
    attacker.setIdentity(createIdentity("not-a-controller"));
    const denied = await attacker.db_export_chunk(0n, 1n);

    assert.equal("Err" in denied, true);
    assert.match(denied.Err, /not a controller/);
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});
