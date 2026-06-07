import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { PocketIc } from "@dfinity/pic";
import { IDL, idlFactory } from "./idl.mjs";
import { startPocketIcServer } from "./server.mjs";

const currentWasm = resolve("target/pocketic/ic_sqlite_vfs.wasm");
const compat022Wasm = resolve("target/pocketic/ic_sqlite_vfs_compat_0_2_2.wasm");
const compat100Wasm = resolve("target/pocketic/ic_sqlite_vfs_compat_1_0_0.wasm");
const timeout = 600_000;
const chunkSize = 64n * 1024n;
const smallChunkSize = chunkSize - 17n;

const result = (ok) => IDL.Variant({ Ok: ok, Err: IDL.Text });

const dbMetaRecord = (IDL) =>
  IDL.Record({
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    schema_version: IDL.Nat64,
    last_tx_id: IDL.Nat64,
    flags: IDL.Nat64,
    checksum: IDL.Nat64,
    checksum_stale: IDL.Bool,
    checksum_refreshing: IDL.Bool,
    checksum_refresh_offset: IDL.Nat64,
    importing: IDL.Bool,
    import_written_until: IDL.Nat64,
    layout_version: IDL.Nat64,
    page_count: IDL.Nat64,
    page_table_bytes: IDL.Nat64,
    active_bytes: IDL.Nat64,
    allocated_bytes: IDL.Nat64,
    orphan_bytes_estimate: IDL.Nat64,
    orphan_ratio_basis_points: IDL.Nat64,
    compact_recommended: IDL.Bool,
  });

const checksumRefreshRecord = (IDL) =>
  IDL.Record({
    complete: IDL.Bool,
    checksum: IDL.Nat64,
    scanned_bytes: IDL.Nat64,
    db_size: IDL.Nat64,
  });

const compat022IdlFactory = ({ IDL }) => {
  const DbMeta = dbMetaRecord(IDL);
  return IDL.Service({
    kv_put: IDL.Func([IDL.Text, IDL.Text], [result(IDL.Null)], []),
    kv_get: IDL.Func([IDL.Text], [result(IDL.Opt(IDL.Text))], ["query"]),
    kv_count: IDL.Func([], [result(IDL.Nat64)], ["query"]),
    db_meta: IDL.Func([], [result(DbMeta)], ["query"]),
    db_integrity_check: IDL.Func([], [result(IDL.Text)], ["query"]),
    db_refresh_checksum: IDL.Func([], [result(IDL.Nat64)], []),
    db_export_chunk: IDL.Func([IDL.Nat64, IDL.Nat64], [result(IDL.Vec(IDL.Nat8))], ["query"]),
  });
};

const compat100IdlFactory = ({ IDL }) => {
  const DbMeta = dbMetaRecord(IDL);
  const ChecksumRefresh = checksumRefreshRecord(IDL);
  return IDL.Service({
    kv_put: IDL.Func([IDL.Text, IDL.Text], [result(IDL.Null)], []),
    kv_get: IDL.Func([IDL.Text], [result(IDL.Opt(IDL.Text))], ["query"]),
    kv_get_many: IDL.Func([IDL.Vec(IDL.Text)], [result(IDL.Vec(IDL.Opt(IDL.Text)))], ["query"]),
    kv_set_note: IDL.Func([IDL.Text, IDL.Text], [result(IDL.Null)], []),
    kv_get_note: IDL.Func([IDL.Text], [result(IDL.Opt(IDL.Text))], ["query"]),
    kv_count: IDL.Func([], [result(IDL.Nat64)], ["query"]),
    db_meta: IDL.Func([], [result(DbMeta)], ["query"]),
    db_integrity_check: IDL.Func([], [result(IDL.Text)], ["query"]),
    db_checksum: IDL.Func([], [result(IDL.Nat64)], ["query"]),
    db_refresh_checksum: IDL.Func([], [result(IDL.Nat64)], []),
    db_refresh_checksum_chunk: IDL.Func([IDL.Nat64], [result(ChecksumRefresh)], []),
    db_export_chunk: IDL.Func([IDL.Nat64, IDL.Nat64], [result(IDL.Vec(IDL.Nat8))], ["query"]),
  });
};

test("PocketIC 0.2.2 image upgrades and imports into current canister", { timeout }, async () => {
  const name = "crossVersion022";
  const server = await startPocketIcServer({ timeoutMs: 60_000 });
  let pic;
  let bodyError;
  let cleanupError;
  try {
    step(name, "create PocketIC client");
    pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
    step(name, "setup 0.2.2 canister");
    const { actor: oldActor, canisterId } = await pic.setupCanister({
      idlFactory: compat022IdlFactory,
      wasm: compat022Wasm,
    });

    step(name, "write old image");
    assert.deepEqual(await oldActor.kv_put("alpha", "from-0.2.2"), { Ok: null });
    assert.deepEqual(await oldActor.kv_put("beta", "second"), { Ok: null });
    for (let index = 0; index < 20; index += 1) {
      const key = `bulk-${index.toString().padStart(3, "0")}`;
      const value = `${key}:${"x".repeat(4096)}`;
      assert.deepEqual(await oldActor.kv_put(key, value), { Ok: null });
    }
    assert.deepEqual(await oldActor.kv_count(), { Ok: 22n });
    assert.deepEqual(await oldActor.db_integrity_check(), { Ok: "ok" });

    step(name, "export old image");
    const checksum = await expectOk("old checksum", oldActor.db_refresh_checksum());
    const oldMeta = await expectOk("old meta", oldActor.db_meta());
    assert.equal(oldMeta.schema_version, 1n);
    assert.equal(oldMeta.checksum, checksum);
    assert.equal(oldMeta.checksum_stale, false);
    assert(oldMeta.db_size > chunkSize, "old image did not cross the 64KiB chunk boundary");
    const exported = await exportImage(oldActor, oldMeta.db_size, smallChunkSize);

    step(name, "upgrade old image to current canister");
    await pic.upgradeCanister({ canisterId, wasm: currentWasm });
    const upgraded = pic.createActor(idlFactory, canisterId);
    assert.deepEqual(await upgraded.kv_get("alpha"), { Ok: ["from-0.2.2"] });
    assert.deepEqual(await upgraded.kv_get("beta"), { Ok: ["second"] });
    assert.deepEqual(await upgraded.kv_get("bulk-019"), {
      Ok: [`bulk-019:${"x".repeat(4096)}`],
    });
    assert.deepEqual(await upgraded.db_integrity_check(), { Ok: "ok" });
    const upgradedMeta = await expectOk("upgraded meta", upgraded.db_meta());
    assert.equal(upgradedMeta.schema_version, 2n);
    assert.deepEqual(await upgraded.kv_set_note("alpha", "migrated"), { Ok: null });
    assert.deepEqual(await upgraded.kv_get_note("alpha"), { Ok: ["migrated"] });

    step(name, "import old image into current canister");
    const { actor: destination, canisterId: destinationId } = await pic.setupCanister({
      idlFactory,
      wasm: currentWasm,
    });
    await importImage(destination, exported, oldMeta.db_size, checksum);
    assert.deepEqual(await destination.kv_get("alpha"), { Ok: ["from-0.2.2"] });
    assert.deepEqual(await destination.kv_get("bulk-019"), {
      Ok: [`bulk-019:${"x".repeat(4096)}`],
    });
    assert.deepEqual(await destination.db_integrity_check(), { Ok: "ok" });

    step(name, "run current migrations after import");
    await pic.upgradeCanister({ canisterId: destinationId, wasm: currentWasm });
    const migratedImport = pic.createActor(idlFactory, destinationId);
    const importMeta = await expectOk("imported meta", migratedImport.db_meta());
    assert.equal(importMeta.schema_version, 2n);
    assert.deepEqual(await migratedImport.kv_set_note("beta", "import-migrated"), { Ok: null });
    assert.deepEqual(await migratedImport.kv_get_note("beta"), { Ok: ["import-migrated"] });

    step(name, "reject wrong checksum without replacing destination");
    const { actor: checksumTarget } = await pic.setupCanister({
      idlFactory,
      wasm: currentWasm,
    });
    assert.deepEqual(await checksumTarget.kv_put("existing", "kept"), { Ok: null });
    await importImageExpectFinishError(checksumTarget, exported, oldMeta.db_size, checksum + 1n);
    assert.deepEqual(await checksumTarget.kv_get("existing"), { Ok: ["kept"] });
    assert.deepEqual(await checksumTarget.db_integrity_check(), { Ok: "ok" });

    step(name, "cancel partial import and keep active image");
    const { actor: cancelTarget } = await pic.setupCanister({
      idlFactory,
      wasm: currentWasm,
    });
    assert.deepEqual(await cancelTarget.kv_put("before-cancel", "kept"), { Ok: null });
    assert.deepEqual(await cancelTarget.db_begin_import(oldMeta.db_size, checksum), { Ok: null });
    assert.deepEqual(await cancelTarget.db_import_chunk(0n, exported[0]), { Ok: null });
    await expectErr(
      "read during import",
      cancelTarget.kv_get("before-cancel"),
      /import|unable to open database file/,
    );
    assert.deepEqual(await cancelTarget.db_cancel_import(), { Ok: null });
    assert.deepEqual(await cancelTarget.kv_get("before-cancel"), { Ok: ["kept"] });

    step(name, "import empty 0.2.2 image");
    const { actor: emptyOld } = await pic.setupCanister({
      idlFactory: compat022IdlFactory,
      wasm: compat022Wasm,
    });
    const emptyChecksum = await expectOk("empty old checksum", emptyOld.db_refresh_checksum());
    const emptyMeta = await expectOk("empty old meta", emptyOld.db_meta());
    assert.equal(emptyMeta.schema_version, 1n);
    assert.deepEqual(await emptyOld.kv_count(), { Ok: 0n });
    const emptyImage = await exportImage(emptyOld, emptyMeta.db_size, chunkSize);
    const { actor: emptyDestination, canisterId: emptyDestinationId } = await pic.setupCanister({
      idlFactory,
      wasm: currentWasm,
    });
    await importImage(emptyDestination, emptyImage, emptyMeta.db_size, emptyChecksum);
    assert.deepEqual(await emptyDestination.kv_count(), { Ok: 0n });
    await pic.upgradeCanister({ canisterId: emptyDestinationId, wasm: currentWasm });
    const emptyMigrated = pic.createActor(idlFactory, emptyDestinationId);
    const emptyImportMeta = await expectOk("empty imported meta", emptyMigrated.db_meta());
    assert.equal(emptyImportMeta.schema_version, 2n);
    assert.deepEqual(await emptyMigrated.db_integrity_check(), { Ok: "ok" });
  } catch (error) {
    bodyError = error;
  } finally {
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
  }

  if (cleanupError) {
    console.error(`[pocketic:cross-version] ${name}: cleanup failed`, cleanupError);
  }
  if (bodyError) {
    throw bodyError;
  }
  if (cleanupError) {
    throw cleanupError;
  }
});

test("PocketIC 1.0.0 image upgrades and imports into current canister", { timeout }, async () => {
  const name = "crossVersion100";
  const server = await startPocketIcServer({ timeoutMs: 60_000 });
  let pic;
  let bodyError;
  let cleanupError;
  try {
    step(name, "create PocketIC client");
    pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
    step(name, "setup 1.0.0 canister");
    const { actor: oldActor, canisterId } = await pic.setupCanister({
      idlFactory: compat100IdlFactory,
      wasm: compat100Wasm,
    });

    step(name, "write 1.0.0 image");
    assert.deepEqual(await oldActor.kv_put("alpha", "from-1.0.0"), { Ok: null });
    assert.deepEqual(await oldActor.kv_put("beta", "second"), { Ok: null });
    assert.deepEqual(await oldActor.kv_set_note("alpha", "released"), { Ok: null });
    for (let index = 0; index < 20; index += 1) {
      const key = `bulk-${index.toString().padStart(3, "0")}`;
      const value = `${key}:${"y".repeat(4096)}`;
      assert.deepEqual(await oldActor.kv_put(key, value), { Ok: null });
    }
    assert.deepEqual(await oldActor.kv_count(), { Ok: 22n });
    assert.deepEqual(await oldActor.kv_get_many(["alpha", "beta", "missing"]), {
      Ok: [["from-1.0.0"], ["second"], []],
    });
    assert.deepEqual(await oldActor.kv_get_note("alpha"), { Ok: ["released"] });
    assert.deepEqual(await oldActor.db_integrity_check(), { Ok: "ok" });

    step(name, "export 1.0.0 image");
    const refresh = await refreshChecksumByChunks(oldActor, smallChunkSize);
    assert.equal(refresh.complete, true);
    assert.equal(refresh.scanned_bytes, refresh.db_size);
    assert.deepEqual(await oldActor.db_checksum(), { Ok: refresh.checksum });
    const oldMeta = await expectOk("1.0.0 meta", oldActor.db_meta());
    assert.equal(oldMeta.schema_version, 2n);
    assert.equal(oldMeta.checksum, refresh.checksum);
    assert.equal(oldMeta.checksum_stale, false);
    assert(oldMeta.db_size > chunkSize, "1.0.0 image did not cross the 64KiB chunk boundary");
    const exported = await exportImage(oldActor, oldMeta.db_size, smallChunkSize);

    step(name, "upgrade 1.0.0 image to current canister");
    await pic.upgradeCanister({ canisterId, wasm: currentWasm });
    const upgraded = pic.createActor(idlFactory, canisterId);
    assert.deepEqual(await upgraded.kv_get("alpha"), { Ok: ["from-1.0.0"] });
    assert.deepEqual(await upgraded.kv_get_note("alpha"), { Ok: ["released"] });
    assert.deepEqual(await upgraded.kv_get_many(["alpha", "bulk-019", "missing"]), {
      Ok: [["from-1.0.0"], [`bulk-019:${"y".repeat(4096)}`], []],
    });
    assert.deepEqual(await upgraded.db_integrity_check(), { Ok: "ok" });
    const upgradedMeta = await expectOk("upgraded 1.0.0 meta", upgraded.db_meta());
    assert.equal(upgradedMeta.schema_version, 2n);

    step(name, "import 1.0.0 image into current canister");
    const { actor: destination, canisterId: destinationId } = await pic.setupCanister({
      idlFactory,
      wasm: currentWasm,
    });
    await importImage(destination, exported, oldMeta.db_size, refresh.checksum);
    assert.deepEqual(await destination.kv_get("alpha"), { Ok: ["from-1.0.0"] });
    assert.deepEqual(await destination.kv_get_note("alpha"), { Ok: ["released"] });
    assert.deepEqual(await destination.db_integrity_check(), { Ok: "ok" });
    const importMeta = await expectOk("imported 1.0.0 meta", destination.db_meta());
    assert.equal(importMeta.schema_version, 2n);

    step(name, "rerun current migrations after 1.0.0 import");
    await pic.upgradeCanister({ canisterId: destinationId, wasm: currentWasm });
    const migratedImport = pic.createActor(idlFactory, destinationId);
    const migratedMeta = await expectOk("migrated 1.0.0 meta", migratedImport.db_meta());
    assert.equal(migratedMeta.schema_version, 2n);
    assert.deepEqual(await migratedImport.kv_get_many(["alpha", "bulk-019", "missing"]), {
      Ok: [["from-1.0.0"], [`bulk-019:${"y".repeat(4096)}`], []],
    });
    assert.deepEqual(await migratedImport.kv_get_note("alpha"), { Ok: ["released"] });
  } catch (error) {
    bodyError = error;
  } finally {
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
  }

  if (cleanupError) {
    console.error(`[pocketic:cross-version] ${name}: cleanup failed`, cleanupError);
  }
  if (bodyError) {
    throw bodyError;
  }
  if (cleanupError) {
    throw cleanupError;
  }
});

function step(name, message) {
  console.error(`[pocketic:cross-version] ${name}: ${message}`);
}

async function exportImage(actor, size, maxChunkSize) {
  const chunks = [];
  for (let offset = 0n; offset < size; offset += maxChunkSize) {
    const len = min(maxChunkSize, size - offset);
    chunks.push(await expectOk(`export chunk ${offset}`, actor.db_export_chunk(offset, len)));
  }
  return chunks;
}

async function importImage(actor, chunks, size, checksum) {
  assert.deepEqual(await actor.db_begin_import(size, checksum), { Ok: null });
  let offset = 0n;
  for (const chunk of chunks) {
    assert.deepEqual(await actor.db_import_chunk(offset, chunk), { Ok: null });
    offset += BigInt(chunk.length);
  }
  assert.equal(offset, size);
  assert.deepEqual(await actor.db_finish_import(), { Ok: null });
}

async function refreshChecksumByChunks(actor, maxChunkSize) {
  const maxChecksumRefreshChunks = 1024;
  let previousScanned = -1n;
  let iterations = 0;
  let refresh;
  do {
    assert(iterations < maxChecksumRefreshChunks, "checksum refresh took too many chunks");
    refresh = await expectOk("refresh checksum chunk", actor.db_refresh_checksum_chunk(maxChunkSize));
    iterations += 1;
    assert(
      refresh.scanned_bytes > previousScanned,
      `checksum refresh made no progress: ${refresh.scanned_bytes}`,
    );
    previousScanned = refresh.scanned_bytes;
  } while (!refresh.complete);
  return refresh;
}

async function importImageExpectFinishError(actor, chunks, size, checksum) {
  assert.deepEqual(await actor.db_begin_import(size, checksum), { Ok: null });
  let offset = 0n;
  for (const chunk of chunks) {
    assert.deepEqual(await actor.db_import_chunk(offset, chunk), { Ok: null });
    offset += BigInt(chunk.length);
  }
  assert.equal(offset, size);
  await expectErr("finish wrong checksum", actor.db_finish_import(), /checksum mismatch/);
}

async function expectOk(name, promise) {
  const result = await promise;
  assert.equal("Ok" in result, true, `${name} failed: ${result.Err}`);
  return result.Ok;
}

async function expectErr(name, promise, pattern) {
  const result = await promise;
  assert.equal("Err" in result, true, `${name} unexpectedly succeeded`);
  assert.match(result.Err, pattern);
}

function min(left, right) {
  return left < right ? left : right;
}
