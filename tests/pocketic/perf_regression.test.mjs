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
  const BenchReadProfileReport = IDL.Record({
    rows: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    open_query: IDL.Nat64,
    prepare: IDL.Nat64,
    key_format: IDL.Nat64,
    query_optional_string_text_total: IDL.Nat64,
    reset_bind: IDL.Nat64,
    step: IDL.Nat64,
    column_read: IDL.Nat64,
    report: IDL.Nat64,
    x_read_calls: IDL.Nat64,
    x_read_bytes: IDL.Nat64,
    stable_data_read_calls: IDL.Nat64,
    stable_data_read_bytes: IDL.Nat64,
    page_table_root_hits: IDL.Nat64,
    page_table_root_misses: IDL.Nat64,
    page_table_segment_hits: IDL.Nat64,
    page_table_segment_misses: IDL.Nat64,
    superblock_loads: IDL.Nat64,
  });
  const DbStatsReport = IDL.Record({
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    sqlite_page_size: IDL.Nat64,
    sqlite_page_count: IDL.Nat64,
    sqlite_freelist_count: IDL.Nat64,
  });
  return IDL.Service({
    bench_reset: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_insert_only: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_append_insert: IDL.Func([IDL.Nat32, IDL.Nat32], [result(BenchReport)], []),
    bench_update_only: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_read: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_read_profile: IDL.Func(
      [IDL.Nat32],
      [result(BenchReadProfileReport)],
      ["query"],
    ),
    bench_get_many_in: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    db_stats: IDL.Func([], [result(DbStatsReport)], ["query"]),
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
    const resetActor = await setupActor(pic);
    const reset = await callReport("bench_reset_1000_clean", resetActor.bench_reset(1_000));
    assertWithinBaseline("bench_reset_1000_clean", reset, 16_060_164n, 25n, 10n);
    await callReport("db_stats_after_reset_1000_clean", resetActor.db_stats());

    const readActor = await setupActor(pic);
    await callReport("bench_reset_1000_for_point_read", readActor.bench_reset(1_000));
    const read1 = await assertLimitCase("bench_read_1", readActor.bench_read(1));
    assertWithinBaseline("bench_read_1", read1, 57_315n, 25n, 10n);
    const read10 = await assertLimitCase("bench_read_10", readActor.bench_read(10));
    assertWithinBaseline("bench_read_10", read10, 187_202n, 25n, 10n);
    const read100 = await assertLimitCase("bench_read_100", readActor.bench_read(100));
    assertWithinBaseline("bench_read_100", read100, 1_489_122n, 25n, 10n);
    const read = await callReport("bench_read_1000", readActor.bench_read(1_000));
    assertWithinBaseline("bench_read", read, 14_824_983n, 25n, 10n);
    const readProfile = await callReport(
      "bench_read_profile",
      readActor.bench_read_profile(1_000),
    );
    assertReadProfile(read, readProfile);
    await callReport("db_stats_after_point_read_clean", readActor.db_stats());

    const writeActor = await setupActor(pic);
    await callReport("bench_reset_1000_for_write", writeActor.bench_reset(1_000));
    const write = await callReport("bench_write_1000_clean", writeActor.bench_write(1_000));
    assertWithinBaseline("bench_write_1000_clean", write, 19_255_157n, 25n, 10n);
    await callReport("db_stats_after_write_clean", writeActor.db_stats());

    await scenario(pic, "insert_only_1000", async (actor) => {
      await assertLimitCase("bench_insert_only_1000", actor.bench_insert_only(1_000));
      await callReport("db_stats_after_insert_only_1000", actor.db_stats());
    });
    const insert5000 = await scenario(pic, "insert_only_5000", async (actor) => {
      const report = await assertLimitCase("bench_insert_only_5000", actor.bench_insert_only(5_000));
      await callReport("db_stats_after_insert_only_5000", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_insert_only_5000", insert5000, 84_353_100n, 25n, 10n);
    await scenario(pic, "append_insert_5000_1000", async (actor) => {
      await assertLimitCase(
        "bench_append_insert_5000_1000",
        actor.bench_append_insert(5_000, 1_000),
      );
      await callReport("db_stats_after_append_insert_5000_1000", actor.db_stats());
    });
    await scenario(pic, "update_only_1000", async (actor) => {
      await assertLimitCase("bench_update_only_1000", actor.bench_update_only(1_000));
      await callReport("db_stats_after_update_only_1000", actor.db_stats());
    });
    const update5000 = await scenario(pic, "update_only_5000", async (actor) => {
      const report = await assertLimitCase("bench_update_only_5000", actor.bench_update_only(5_000));
      await callReport("db_stats_after_update_only_5000", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_update_only_5000", update5000, 115_875_483n, 25n, 10n);

    const bulkActor = await setupActor(pic);
    await callReport("bench_reset_5000_clean", bulkActor.bench_reset(5_000));
    await assertLimitCase("bench_many_rows_100", bulkActor.bench_many_rows(100));
    await assertLimitCase("bench_many_rows_1000", bulkActor.bench_many_rows(1_000));
    const bulk5000 = await assertLimitCase("bench_many_rows_5000", bulkActor.bench_many_rows(5_000));
    assertWithinBaseline("bench_many_rows_5000", bulk5000, 7_587_529n, 25n, 10n);
    await assertLimitCase("bench_get_many_in_100", bulkActor.bench_get_many_in(100));
    await assertLimitCase("bench_get_many_in_1000", bulkActor.bench_get_many_in(1_000));
    await callReport("db_stats_after_5000_clean", bulkActor.db_stats());

    await scenario(pic, "large_blob_64k", async (actor) => {
      await assertLimitCase("bench_large_blob_64k", actor.bench_large_blob(64 * 1024));
      await callReport("db_stats_after_large_blob_64k", actor.db_stats());
    });
    await scenario(pic, "large_blob_256k", async (actor) => {
      await assertLimitCase("bench_large_blob_256k", actor.bench_large_blob(256 * 1024));
      await callReport("db_stats_after_large_blob_256k", actor.db_stats());
    });

    await scenario(pic, "unbounded_order_by_5000", async (actor) => {
      await assertLimitCase("bench_unbounded_order_by_5000", actor.bench_unbounded_order_by(5_000));
      await callReport("db_stats_after_unbounded_order_by_5000", actor.db_stats());
    });
    await scenario(pic, "join_2000", async (actor) => {
      await assertLimitCase("bench_join_2000", actor.bench_join(2_000));
      await callReport("db_stats_after_join_2000", actor.db_stats());
    });
    const growth1k = await scenario(pic, "growth_1000_20", async (actor) =>
      assertLimitCase("bench_growth_1000_20", actor.bench_growth(1_000, 20)),
    );
    const growth5k = await scenario(pic, "growth_5000_20", async (actor) =>
      assertLimitCase("bench_growth_5000_20", actor.bench_growth(5_000, 20)),
    );
    assertWithinBaseline("bench_growth_1000_20", growth1k, 25_143_585n, 25n, 10n);
    assertWithinBaseline("bench_growth_5000_20", growth5k, 25_158_511n, 25n, 10n);
    assert(
      growth5k.instructions <= growth1k.instructions * 2n,
      `bench_growth should not scale with DB size: 1k=${formatReport(growth1k)}, 5k=${formatReport(growth5k)}`,
    );
  } finally {
    await pic.tearDown();
    await server.stop();
  }
});

async function setupActor(pic) {
  const { actor } = await pic.setupCanister({ idlFactory, wasm });
  return actor;
}

async function scenario(pic, _name, fn) {
  const actor = await setupActor(pic);
  return fn(actor);
}

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
  if ("instructions" in report) {
    assert(report.instructions > 0n, `${name} reported zero instructions`);
  }
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

function assertReadProfile(read, profile) {
  assert.equal(profile.rows, read.rows, "profile row count differs from bench_read");
  assert.equal(profile.checksum, read.checksum, "profile checksum differs from bench_read");
  assert(
    profile.instructions <= read.instructions * 2n,
    `bench_read_profile is too far from bench_read: read=${formatReport(read)}, profile=${formatReport(profile)}`,
  );

  const topLevel =
    profile.open_query +
    profile.prepare +
    profile.key_format +
    profile.query_optional_string_text_total +
    profile.report;
  assert(topLevel > 0n, "bench_read_profile reported no top-level work");
  assert(
    topLevel <= profile.instructions,
    `profile top-level timings exceed total instructions: topLevel=${topLevel}, profile=${formatReport(profile)}`,
  );

  const statementParts = profile.reset_bind + profile.step + profile.column_read;
  assert(statementParts > 0n, "bench_read_profile reported no statement work");
  assert(
    statementParts <= profile.query_optional_string_text_total,
    `statement timings exceed query total: statementParts=${statementParts}, profile=${formatReport(profile)}`,
  );

  assert(profile.x_read_calls > 0n, "bench_read_profile reported no xRead calls");
  assert(profile.stable_data_read_calls > 0n, "bench_read_profile reported no stable reads");
}

function formatReport(report) {
  const fields = [
    "rows" in report ? `rows=${report.rows}` : undefined,
    "instructions" in report ? `instructions=${report.instructions}` : undefined,
    "checksum" in report ? `checksum=${report.checksum}` : undefined,
    `db_size=${report.db_size}`,
    `stable_pages=${report.stable_pages}`,
    `stable_bytes=${report.stable_bytes}`,
    "sqlite_page_size" in report ? `sqlite_page_size=${report.sqlite_page_size}` : undefined,
    "sqlite_page_count" in report ? `sqlite_page_count=${report.sqlite_page_count}` : undefined,
    "sqlite_freelist_count" in report
      ? `sqlite_freelist_count=${report.sqlite_freelist_count}`
      : undefined,
  ].filter(Boolean);
  if ("open_query" in report) {
    fields.push(
      `open_query=${report.open_query}`,
      `prepare=${report.prepare}`,
      `key_format=${report.key_format}`,
      `query_optional_string_text_total=${report.query_optional_string_text_total}`,
      `reset_bind=${report.reset_bind}`,
      `step=${report.step}`,
      `column_read=${report.column_read}`,
      `report=${report.report}`,
      `x_read_calls=${report.x_read_calls}`,
      `x_read_bytes=${report.x_read_bytes}`,
      `stable_data_read_calls=${report.stable_data_read_calls}`,
      `stable_data_read_bytes=${report.stable_data_read_bytes}`,
      `page_table_root_hits=${report.page_table_root_hits}`,
      `page_table_root_misses=${report.page_table_root_misses}`,
      `page_table_segment_hits=${report.page_table_segment_hits}`,
      `page_table_segment_misses=${report.page_table_segment_misses}`,
      `superblock_loads=${report.superblock_loads}`,
    );
  }
  return fields.join(", ");
}
