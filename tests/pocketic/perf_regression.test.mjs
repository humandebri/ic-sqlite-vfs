import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { IDL } from "@dfinity/candid";
import { PocketIc } from "@dfinity/pic";
import { startPocketIcServer } from "./server.mjs";

const wasm = resolve("target/pocketic/ic_sqlite_vfs_kv_bench.wasm");
const timeout = 600_000;
const maxLimitCaseInstructions = 10_000_000_000n;

const result = (ok) => IDL.Variant({ Ok: ok, Err: IDL.Text });

const idlFactory = ({ IDL }) => {
  const BenchReport = IDL.Record({
    rows: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
  });
  return IDL.Service({
    bench_reset: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_read: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_write: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_large_blob: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_many_rows: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_unbounded_order_by: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_join: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_growth: IDL.Func([IDL.Nat32, IDL.Nat32], [result(BenchReport)], []),
  });
};

test("PocketIC instruction and limit-case regression checks", { timeout }, async () => {
  const server = await startPocketIcServer({ timeoutMs: timeout });
  const pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
  try {
    const { actor } = await pic.setupCanister({ idlFactory, wasm });

    const reset = await callReport("bench_reset", actor.bench_reset(1_000));
    assertWithinBaseline("bench_reset", reset, 18_297_989n, 25n, 10n);

    const read = await callReport("bench_read", actor.bench_read(1_000));
    assertWithinBaseline("bench_read", read, 24_336_584n, 30n, 10n);

    const write = await callReport("bench_write", actor.bench_write(1_000));
    assertWithinBaseline("bench_write", write, 20_397_159n, 25n, 10n);

    await assertLimitCase("bench_large_blob_64k", actor.bench_large_blob(64 * 1024));
    await assertLimitCase("bench_large_blob_256k", actor.bench_large_blob(256 * 1024));

    await callReport("bench_reset_5000", actor.bench_reset(5_000));
    await assertLimitCase("bench_many_rows_1000", actor.bench_many_rows(1_000));
    await assertLimitCase("bench_many_rows_5000", actor.bench_many_rows(5_000));

    await assertLimitCase("bench_unbounded_order_by_5000", actor.bench_unbounded_order_by(5_000));
    await assertLimitCase("bench_join_2000", actor.bench_join(2_000));
    const growth1k = await assertLimitCase("bench_growth_1000_20", actor.bench_growth(1_000, 20));
    const growth5k = await assertLimitCase("bench_growth_5000_20", actor.bench_growth(5_000, 20));
    assertWithinBaseline("bench_growth_1000_20", growth1k, 26_623_396n, 25n, 10n);
    assertWithinBaseline("bench_growth_5000_20", growth5k, 26_620_438n, 25n, 10n);
    assert(
      growth5k.instructions <= growth1k.instructions * 2n,
      `bench_growth should not scale with DB size: 1k=${formatReport(growth1k)}, 5k=${formatReport(growth5k)}`,
    );
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

async function assertLimitCase(name, promise) {
  const report = await callReport(name, promise);
  assert(
    report.instructions <= maxLimitCaseInstructions,
    `${name} exceeded limit-case instruction cap: ${formatReport(report)}`,
  );
  return report;
}

async function callReport(name, promise) {
  const result = await promise;
  assert.equal("Ok" in result, true, `${name} failed: ${result.Err}`);
  const report = result.Ok;
  console.log(`${name}: ${formatReport(report)}`);
  assert(report.instructions > 0n, `${name} reported zero instructions`);
  assert(report.stable_pages > 0n, `${name} reported zero stable pages`);
  assert(report.stable_bytes >= report.stable_pages * 65_536n, `${name} stable bytes mismatch`);
  return report;
}

function assertWithinBaseline(name, report, baseline, multiplier, divisor) {
  const limit = (baseline * multiplier) / divisor;
  assert(
    report.instructions <= limit,
    `${name} instruction regression: baseline=${baseline}, limit=${limit}, ${formatReport(report)}`,
  );
}

function formatReport(report) {
  return [
    `rows=${report.rows}`,
    `instructions=${report.instructions}`,
    `checksum=${report.checksum}`,
    `db_size=${report.db_size}`,
    `stable_pages=${report.stable_pages}`,
    `stable_bytes=${report.stable_bytes}`,
  ].join(", ");
}
