import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { IDL } from "@dfinity/candid";
import { PocketIc } from "@dfinity/pic";
import { startPocketIcServer } from "./server.mjs";

const wasm = resolve("target/pocketic/ic_rusqlite_kv_bench.wasm");
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
    bench_insert_only: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_append_insert: IDL.Func([IDL.Nat32, IDL.Nat32], [result(BenchReport)], []),
    bench_update_only: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_read: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_many_rows: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_get_many_in: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_write: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
  });
};

test("PocketIC wasi2ic + ic-rusqlite bulk read benchmark", { timeout }, async () => {
  const server = await startPocketIcServer({ timeoutMs: timeout });
  const pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
  try {
    const { actor } = await pic.setupCanister({ idlFactory, wasm });

    await callReport("ic_rusqlite_bench_reset_1000", actor.bench_reset(1_000));
    await assertLimitCase("ic_rusqlite_bench_read_1", actor.bench_read(1));
    await assertLimitCase("ic_rusqlite_bench_read_10", actor.bench_read(10));
    await assertLimitCase("ic_rusqlite_bench_read_100", actor.bench_read(100));
    await assertLimitCase("ic_rusqlite_bench_read_1000", actor.bench_read(1_000));
    await assertLimitCase("ic_rusqlite_bench_write_1000", actor.bench_write(1_000));

    await assertLimitCase(
      "ic_rusqlite_bench_insert_only_1000",
      actor.bench_insert_only(1_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_insert_only_5000",
      actor.bench_insert_only(5_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_append_insert_5000_1000",
      actor.bench_append_insert(5_000, 1_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_update_only_1000",
      actor.bench_update_only(1_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_update_only_5000",
      actor.bench_update_only(5_000),
    );

    await callReport("ic_rusqlite_bench_reset_5000", actor.bench_reset(5_000));
    await assertLimitCase(
      "ic_rusqlite_bench_many_rows_100",
      actor.bench_many_rows(100),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_many_rows_1000",
      actor.bench_many_rows(1_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_many_rows_5000",
      actor.bench_many_rows(5_000),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_get_many_in_100",
      actor.bench_get_many_in(100),
    );
    await assertLimitCase(
      "ic_rusqlite_bench_get_many_in_1000",
      actor.bench_get_many_in(1_000),
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
