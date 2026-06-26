import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import { IDL } from "@dfinity/candid";
import { PocketIc } from "@dfinity/pic";
import { startPocketIcServer } from "./server.mjs";

const timeout = 600_000;
const maxLimitCaseInstructions = 10_000_000_000n;
const baseRows = 5_000;
const churnRows = 1_000;
const cycles = 100;

const result = (ok) => IDL.Variant({ Ok: ok, Err: IDL.Text });

const idlFactory = ({ IDL }) => {
  const BenchChurnStepReport = IDL.Record({
    cycle: IDL.Nat64,
    phase: IDL.Text,
    rows: IDL.Nat64,
    instructions: IDL.Nat64,
    row_count: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    sqlite_page_size: IDL.Nat64,
    sqlite_page_count: IDL.Nat64,
    sqlite_freelist_count: IDL.Nat64,
  });
  return IDL.Service({
    bench_churn_reset: IDL.Func([IDL.Nat32], [result(BenchChurnStepReport)], []),
    bench_churn_delete: IDL.Func(
      [IDL.Nat32, IDL.Nat32, IDL.Nat32],
      [result(BenchChurnStepReport)],
      [],
    ),
    bench_churn_insert: IDL.Func(
      [IDL.Nat32, IDL.Nat32, IDL.Nat32],
      [result(BenchChurnStepReport)],
      [],
    ),
  });
};

test("PocketIC write/delete capacity churn comparison", { timeout }, async () => {
  const server = await startPocketIcServer({ timeoutMs: timeout });
  const pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
  try {
    const icSqlite = await runImplementation(
      pic,
      "ic_sqlite_vfs",
      resolve("target/pocketic/ic_sqlite_vfs_kv_bench.wasm"),
    );
    const icRusqlite = await runImplementation(
      pic,
      "ic_rusqlite",
      resolve("target/pocketic/ic_rusqlite_kv_bench.wasm"),
    );

    printSummary(icSqlite);
    printSummary(icRusqlite);
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

async function runImplementation(pic, label, wasm) {
  const { actor } = await pic.setupCanister({ idlFactory, wasm });
  const reports = [];
  reports.push(await callChurnReport(`${label}_churn_reset_${baseRows}`, actor.bench_churn_reset(baseRows)));

  for (let cycle = 0; cycle < cycles; cycle += 1) {
    const deleteStart = cycle * churnRows;
    const insertStart = baseRows + cycle * churnRows;
    const deleteReport = await callChurnReport(
      `${label}_churn_delete_${cycle}_${deleteStart}_${churnRows}`,
      actor.bench_churn_delete(deleteStart, churnRows, cycle),
    );
    assert.equal(
      deleteReport.row_count,
      BigInt(baseRows - churnRows),
      `${label} delete cycle ${cycle} should leave baseRows - churnRows`,
    );
    reports.push(deleteReport);

    const insertReport = await callChurnReport(
      `${label}_churn_insert_${cycle}_${insertStart}_${churnRows}`,
      actor.bench_churn_insert(insertStart, churnRows, cycle),
    );
    assert.equal(
      insertReport.row_count,
      BigInt(baseRows),
      `${label} insert cycle ${cycle} should restore baseRows`,
    );
    reports.push(insertReport);
  }

  assertStableMemoryDoesNotGrow(label, reports);
  assertDbSizeStopsAfterFirstInsert(label, reports);
  return { label, reports };
}

async function callChurnReport(name, promise) {
  const result = await promise;
  if ("Err" in result) {
    throw new Error(`${name} failed: ${result.Err}`);
  }
  const report = result.Ok;
  assert(
    report.instructions < maxLimitCaseInstructions,
    `${name} exceeded instruction limit: ${formatReport(report)}`,
  );
  console.log(`${name}: ${formatReport(report)}`);
  return report;
}

function assertStableMemoryDoesNotGrow(label, reports) {
  const reset = reports[0];
  for (let index = 1; index < reports.length; index += 1) {
    const current = reports[index];
    assert.equal(
      current.stable_pages,
      reset.stable_pages,
      `${label} stable_pages grew at step ${index}: reset=${formatReport(reset)}, current=${formatReport(current)}`,
    );
    assert.equal(
      current.stable_bytes,
      reset.stable_bytes,
      `${label} stable_bytes grew at step ${index}: reset=${formatReport(reset)}, current=${formatReport(current)}`,
    );
  }
}

function assertDbSizeStopsAfterFirstInsert(label, reports) {
  const firstInsert = reports.find((report) => report.phase === "insert");
  assert(firstInsert, `${label} produced no insert report`);
  for (const report of reports) {
    assert(
      report.db_size <= firstInsert.db_size,
      `${label} db_size grew after first insert: firstInsert=${formatReport(firstInsert)}, current=${formatReport(report)}`,
    );
    assert(
      report.sqlite_page_count <= firstInsert.sqlite_page_count,
      `${label} sqlite_page_count grew after first insert: firstInsert=${formatReport(firstInsert)}, current=${formatReport(report)}`,
    );
  }
}

function printSummary({ label, reports }) {
  const reset = reports[0];
  const maxStablePages = maxBy(reports, (report) => report.stable_pages).stable_pages;
  const maxStableBytes = maxBy(reports, (report) => report.stable_bytes).stable_bytes;
  const maxDbSize = maxBy(reports, (report) => report.db_size).db_size;
  const final = reports[reports.length - 1];
  const totalDeleteInstructions = sumInstructions(reports, "delete");
  const totalInsertInstructions = sumInstructions(reports, "insert");
  console.log(
    `${label}_churn_summary: reset_stable_pages=${reset.stable_pages}, max_stable_pages=${maxStablePages}, stable_grow=${maxStablePages > reset.stable_pages}, reset_stable_bytes=${reset.stable_bytes}, max_stable_bytes=${maxStableBytes}, final_stable_bytes=${final.stable_bytes}, reset_db_size=${reset.db_size}, max_db_size=${maxDbSize}, final_db_size=${final.db_size}, final_freelist_pages=${final.sqlite_freelist_count}, total_delete_instructions=${totalDeleteInstructions}, total_insert_instructions=${totalInsertInstructions}`,
  );
}

function maxBy(reports, value) {
  return reports.reduce((max, report) => (value(report) > value(max) ? report : max), reports[0]);
}

function sumInstructions(reports, phase) {
  return reports
    .filter((report) => report.phase === phase)
    .reduce((sum, report) => sum + report.instructions, 0n);
}

function formatReport(report) {
  return [
    `cycle=${report.cycle}`,
    `phase=${report.phase}`,
    `rows=${report.rows}`,
    `instructions=${report.instructions}`,
    `row_count=${report.row_count}`,
    `db_size=${report.db_size}`,
    `stable_pages=${report.stable_pages}`,
    `stable_bytes=${report.stable_bytes}`,
    `sqlite_page_size=${report.sqlite_page_size}`,
    `sqlite_page_count=${report.sqlite_page_count}`,
    `sqlite_freelist_count=${report.sqlite_freelist_count}`,
  ].join(", ");
}
