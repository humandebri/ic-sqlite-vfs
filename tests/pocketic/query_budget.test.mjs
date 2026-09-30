import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { IDL } from "@dfinity/candid";
import { PocketIc, createIdentity } from "@dfinity/pic";
import { startPocketIcServer } from "./server.mjs";

const idlFactory = () => {
  const report = IDL.Record({
    error: IDL.Opt(IDL.Text),
    instructions: IDL.Opt(IDL.Nat64),
    prepare_instructions: IDL.Opt(IDL.Nat64),
    bind_instructions: IDL.Opt(IDL.Nat64),
    execute_instructions: IDL.Opt(IDL.Nat64),
    vm_steps: IDL.Nat32,
    result_rows: IDL.Nat64,
    result_bytes: IDL.Nat64,
    recovery: IDL.Int64,
    stable_read_bytes: IDL.Opt(IDL.Nat64),
  });
  return IDL.Service({
    kv_put: IDL.Func([IDL.Text, IDL.Text], [IDL.Variant({ Ok: IDL.Null, Err: IDL.Text })], []),
    db_test_query_overhead: IDL.Func([IDL.Nat32], [IDL.Variant({ Ok: IDL.Record({
      plain_instructions: IDL.Nat64,
      profiled_instructions: IDL.Nat64,
      budgeted_instructions: IDL.Nat64,
    }), Err: IDL.Text })], []),
    db_test_update_budget: IDL.Func(
      [IDL.Nat32, IDL.Nat32, IDL.Opt(IDL.Nat64), IDL.Nat64, IDL.Nat64, IDL.Nat64],
      [IDL.Variant({ Ok: IDL.Record({
        error: IDL.Opt(IDL.Text), changed_rows: IDL.Opt(IDL.Nat64),
        instructions: IDL.Opt(IDL.Nat64), commit_instructions: IDL.Opt(IDL.Nat64),
        peak_dirty_pages: IDL.Nat64, overlay_peak_bytes: IDL.Nat64,
        committed_bytes: IDL.Nat64, committed_over_soft_limit: IDL.Bool,
        stored_rows: IDL.Int64,
      }), Err: IDL.Text })], [],
    ),
    db_test_query_bind_budget: IDL.Func([], [IDL.Variant({ Ok: report, Err: IDL.Text })], []),
    db_test_query_budget: IDL.Func(
      [IDL.Nat32, IDL.Opt(IDL.Nat64), IDL.Nat64, IDL.Nat64, IDL.Bool],
      [IDL.Variant({ Ok: report, Err: IDL.Text })], [],
    ),
  });
};

test("IC instruction budgets interrupt SQL and leave the cached connection usable", { timeout: 120_000 }, async () => {
  const server = await startPocketIcServer({ timeoutMs: 60_000 });
  let pic;
  try {
    pic = await PocketIc.create(server.getUrl());
    const identity = createIdentity("query-budget-controller");
    const fixture = await pic.setupCanister({
      idlFactory,
      wasm: resolve(process.env.QUERY_BUDGET_WASM ?? "target/pocketic/ic_sqlite_vfs_failpoints.wasm"),
      sender: identity.getPrincipal(),
    });
    const actor = fixture.actor;
    actor.setIdentity(identity);
    assert.deepEqual(await actor.kv_put("sentinel", "unchanged"), { Ok: null });
    async function run(iterations, budget, rows = 10n, bytes = 1024n, aggregate = true) {
      const result = await actor.db_test_query_budget(iterations, budget, rows, bytes, aggregate);
      assert.ok("Ok" in result, result.Err);
      assert.equal(result.Ok.recovery, 1n);
      return result.Ok;
    }
    const cold = await run(100_000, []);
    const warm = await run(100_000, []);
    assert.deepEqual(cold.error, []);
    assert.deepEqual(warm.error, []);
    assert.ok(warm.instructions[0] > 0n);
    assert.ok(warm.vm_steps > 0);
    assert.equal(warm.result_rows, 1n);
    assert.equal(warm.result_bytes, 8n);
    assert.ok(warm.prepare_instructions[0] > 0n);
    assert.ok(warm.execute_instructions[0] > 0n);
    const limited = await run(100_000, [1_000_000n]);
    assert.match(limited.error[0], /budget exceeded: Instructions/);
    assert.ok(limited.instructions[0] < warm.instructions[0]);
    assert.ok(limited.vm_steps > 0);
    const zero = await run(100_000, [0n]);
    assert.match(zero.error[0], /budget exceeded: Instructions/);
    const rowLimited = await run(100, [], 2n, 1024n, false);
    assert.match(rowLimited.error[0], /budget exceeded: Rows/);
    assert.equal(rowLimited.result_rows, 2n);
    const byteLimited = await run(100, [], 100n, 7n, false);
    assert.match(byteLimited.error[0], /budget exceeded: ResultBytes/);
    assert.equal(byteLimited.result_rows, 0n);
    assert.deepEqual((await run(100, [100_000_000n])).error, []);
    const bindLimited = await actor.db_test_query_bind_budget();
    assert.ok("Ok" in bindLimited, bindLimited.Err);
    assert.match(bindLimited.Ok.error[0], /budget exceeded: Instructions/);
    assert.equal(bindLimited.Ok.recovery, 1n);
    // One parameter copy and cleanup can overshoot; the old loop consumed 67M.
    assert.ok(bindLimited.Ok.instructions[0] < 12_000_000n,
      "parameter binding ignored the soft instruction budget");
    console.log(JSON.stringify({ bindInterrupted: bindLimited.Ok.instructions[0].toString(),
      bindInstructions: bindLimited.Ok.bind_instructions[0].toString() }));
    async function update(iterations, blobBytes, instructions = [], reserve = 1_000_000n,
      pages = 256n, bytes = 1_048_576n) {
      const result = await actor.db_test_update_budget(iterations, blobBytes, instructions, reserve, pages, bytes);
      assert.ok("Ok" in result, result.Err);
      return result.Ok;
    }
    const instructionUpdate = await update(100_000, 0, [2_000_000n]);
    assert.match(instructionUpdate.error[0], /update budget exceeded: Instructions/);
    assert.equal(instructionUpdate.stored_rows, 0n);
    assert.equal(instructionUpdate.committed_bytes, 0n);
    const pageUpdate = await update(100, 20_000, [], 1_000_000n, 1n);
    assert.match(pageUpdate.error[0], /update budget exceeded: DirtyPages/);
    assert.equal(pageUpdate.stored_rows, 0n);
    assert.ok(pageUpdate.peak_dirty_pages <= 1n);
    const byteUpdate = await update(100, 20_000, [], 1_000_000n, 256n, 4095n);
    assert.match(byteUpdate.error[0], /update budget exceeded: ChangedBytes/);
    assert.equal(byteUpdate.stored_rows, 0n);
    const committedUpdate = await update(1000, 128, [100_000_000n], 20_000_000n);
    assert.deepEqual(committedUpdate.error, []);
    assert.deepEqual(committedUpdate.changed_rows, [1000n]);
    assert.equal(committedUpdate.stored_rows, 1000n);
    assert.ok(committedUpdate.committed_bytes > 0n);
    assert.ok(committedUpdate.commit_instructions[0] > 0n);
    assert.equal(committedUpdate.committed_over_soft_limit, false);
    // Calibrate after warming the same table shape. A deliberately tiny reserve
    // exercises the contract that a published write must not become an error.
    const warmCommit = await update(1000, 128, [100_000_000n], 20_000_000n);
    assert.deepEqual(warmCommit.error, []);
    const publishedOverrun = await update(1000, 128, [warmCommit.instructions[0] - 1000n], 1n);
    assert.deepEqual(publishedOverrun.error, []);
    assert.equal(publishedOverrun.stored_rows, 1000n);
    assert.equal(publishedOverrun.committed_over_soft_limit, true);
    console.log(JSON.stringify({ interruptedUpdate: instructionUpdate, committedUpdate, publishedOverrun },
      (_, value) => typeof value === "bigint" ? value.toString() : value));
    const overhead = await actor.db_test_query_overhead(10_000);
    assert.ok("Ok" in overhead, overhead.Err);
    assert.ok(overhead.Ok.plain_instructions > 0n);
    assert.ok(overhead.Ok.profiled_instructions > 0n);
    assert.ok(overhead.Ok.budgeted_instructions > 0n);
    assert.ok(overhead.Ok.profiled_instructions * 100n < overhead.Ok.plain_instructions * 125n,
      "profiled row collection overhead exceeded 25%");
    assert.ok(overhead.Ok.budgeted_instructions * 100n < overhead.Ok.plain_instructions * 150n,
      "budgeted row collection overhead exceeded 50%");
    console.log(JSON.stringify(overhead.Ok, (_, value) => typeof value === "bigint" ? value.toString() : value));
    console.log(JSON.stringify({ cold: cold.instructions[0].toString(), warm: warm.instructions[0].toString(), interrupted: limited.instructions[0].toString(), vfsEnabled: cold.stable_read_bytes.length > 0 }));
  } finally {
    try { if (pic) await pic.tearDown(); } finally { await server.stop(); }
  }
});
