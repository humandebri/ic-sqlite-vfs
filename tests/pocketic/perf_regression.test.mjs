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
    superblock_loads: IDL.Nat64,
  });
  const BenchGetManyProfileReport = IDL.Record({
    rows: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    open_query: IDL.Nat64,
    sql_build: IDL.Nat64,
    key_build: IDL.Nat64,
    prepare: IDL.Nat64,
    bind: IDL.Nat64,
    row_scan: IDL.Nat64,
    report: IDL.Nat64,
    x_read_calls: IDL.Nat64,
    x_read_bytes: IDL.Nat64,
    stable_data_read_calls: IDL.Nat64,
    stable_data_read_bytes: IDL.Nat64,
    superblock_loads: IDL.Nat64,
  });
  const BenchWriteProfileReport = IDL.Record({
    rows: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    open_update: IDL.Nat64,
    prepare: IDL.Nat64,
    key_value_format: IDL.Nat64,
    execute_total: IDL.Nat64,
    reset_bind: IDL.Nat64,
    step: IDL.Nat64,
    report: IDL.Nat64,
    x_read_calls: IDL.Nat64,
    x_read_bytes: IDL.Nat64,
    x_write_calls: IDL.Nat64,
    x_write_bytes: IDL.Nat64,
    x_file_size_calls: IDL.Nat64,
    x_lock_calls: IDL.Nat64,
    x_unlock_calls: IDL.Nat64,
    x_check_reserved_lock_calls: IDL.Nat64,
    x_file_control_calls: IDL.Nat64,
    x_device_characteristics_calls: IDL.Nat64,
    stable_data_read_calls: IDL.Nat64,
    stable_data_read_bytes: IDL.Nat64,
    stable_data_write_calls: IDL.Nat64,
    stable_data_write_bytes: IDL.Nat64,
    stable_grow_calls: IDL.Nat64,
    stable_grow_pages: IDL.Nat64,
    superblock_loads: IDL.Nat64,
    commit_load: IDL.Nat64,
    commit_capacity: IDL.Nat64,
    commit_page_write: IDL.Nat64,
    commit_superblock_store: IDL.Nat64,
  });
  const BenchGrowthProfileReport = IDL.Record({
    rows: IDL.Nat64,
    writes: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    open_update: IDL.Nat64,
    key_value_format: IDL.Nat64,
    prepare: IDL.Nat64,
    execute_total: IDL.Nat64,
    changes: IDL.Nat64,
    report: IDL.Nat64,
    x_read_calls: IDL.Nat64,
    x_read_bytes: IDL.Nat64,
    x_write_calls: IDL.Nat64,
    x_write_bytes: IDL.Nat64,
    x_file_size_calls: IDL.Nat64,
    x_lock_calls: IDL.Nat64,
    x_unlock_calls: IDL.Nat64,
    x_check_reserved_lock_calls: IDL.Nat64,
    x_file_control_calls: IDL.Nat64,
    x_device_characteristics_calls: IDL.Nat64,
    stable_data_read_calls: IDL.Nat64,
    stable_data_read_bytes: IDL.Nat64,
    stable_data_write_calls: IDL.Nat64,
    stable_data_write_bytes: IDL.Nat64,
    stable_grow_calls: IDL.Nat64,
    stable_grow_pages: IDL.Nat64,
    superblock_loads: IDL.Nat64,
    commit_load: IDL.Nat64,
    commit_capacity: IDL.Nat64,
    commit_page_write: IDL.Nat64,
    commit_superblock_store: IDL.Nat64,
  });
  const BenchCapacityGrowthReport = IDL.Record({
    rows: IDL.Nat64,
    writes: IDL.Nat64,
    instructions: IDL.Nat64,
    checksum: IDL.Nat64,
    db_size: IDL.Nat64,
    stable_pages: IDL.Nat64,
    stable_bytes: IDL.Nat64,
    db_size_before: IDL.Nat64,
    db_size_after: IDL.Nat64,
    db_base_offset_before: IDL.Nat64,
    db_base_offset_after: IDL.Nat64,
    page_table_offset_before: IDL.Nat64,
    page_table_offset_after: IDL.Nat64,
    page_table_bytes_before: IDL.Nat64,
    page_table_bytes_after: IDL.Nat64,
    stable_pages_before: IDL.Nat64,
    stable_pages_after: IDL.Nat64,
    allocated_bytes_before: IDL.Nat64,
    allocated_bytes_after: IDL.Nat64,
    orphan_bytes_estimate_before: IDL.Nat64,
    orphan_bytes_estimate_after: IDL.Nat64,
    stable_grow_calls: IDL.Nat64,
    stable_grow_pages: IDL.Nat64,
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
    bench_read_public_helper: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_read_prepare_each: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_read_profile: IDL.Func(
      [IDL.Nat32],
      [result(BenchReadProfileReport)],
      ["query"],
    ),
    bench_get_many_in: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_get_many_in_profile: IDL.Func(
      [IDL.Nat32],
      [result(BenchGetManyProfileReport)],
      ["query"],
    ),
    db_stats: IDL.Func([], [result(DbStatsReport)], ["query"]),
    bench_write: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_write_profile: IDL.Func(
      [IDL.Nat32],
      [result(BenchWriteProfileReport)],
      [],
    ),
    bench_large_blob: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_many_rows: IDL.Func([IDL.Nat32], [result(BenchReport)], ["query"]),
    bench_unbounded_order_by: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_join: IDL.Func([IDL.Nat32], [result(BenchReport)], []),
    bench_growth: IDL.Func([IDL.Nat32, IDL.Nat32], [result(BenchReport)], []),
    bench_growth_profile: IDL.Func(
      [IDL.Nat32, IDL.Nat32],
      [result(BenchGrowthProfileReport)],
      [],
    ),
    bench_capacity_growth_guard: IDL.Func(
      [IDL.Nat32, IDL.Nat32],
      [result(BenchCapacityGrowthReport)],
      [],
    ),
  });
};

test("PocketIC instruction and limit-case regression checks", { timeout }, async () => {
  const server = await startPocketIcServer({ timeoutMs: timeout });
  const pic = await PocketIc.create(server.getUrl(), { processingTimeoutMs: timeout });
  try {
    const resetActor = await setupActor(pic);
    const reset = await callReport("bench_reset_1000_clean", resetActor.bench_reset(1_000));
    assertWithinBaseline("bench_reset_1000_clean", reset, 10_623_761n, 25n, 10n);
    await callReport("db_stats_after_reset_1000_clean", resetActor.db_stats());

    const readActor = await setupActor(pic);
    await callReport("bench_reset_1000_for_point_read", readActor.bench_reset(1_000));
    const read1 = await assertLimitCase("bench_read_1", readActor.bench_read(1));
    assertWithinBaseline("bench_read_1", read1, 46_515n, 25n, 10n);
    const read10 = await assertLimitCase("bench_read_10", readActor.bench_read(10));
    assertWithinBaseline("bench_read_10", read10, 132_896n, 25n, 10n);
    const read100 = await assertLimitCase("bench_read_100", readActor.bench_read(100));
    assertWithinBaseline("bench_read_100", read100, 999_756n, 25n, 10n);
    const read = await callReport("bench_read_1000", readActor.bench_read(1_000));
    assertWithinBaseline("bench_read", read, 9_982_767n, 25n, 10n);
    const publicHelperRead = await assertLimitCase(
      "bench_read_public_helper_1000",
      readActor.bench_read_public_helper(1_000),
    );
    const prepareEachRead = await assertLimitCase(
      "bench_read_prepare_each_1000",
      readActor.bench_read_prepare_each(1_000),
    );
    assertWithinBaseline("bench_read_public_helper_1000", publicHelperRead, 11_965_551n, 25n, 10n);
    assertWithinBaseline("bench_read_prepare_each_1000", prepareEachRead, 39_354_703n, 25n, 10n);
    assert(
      publicHelperRead.instructions * 2n <= prepareEachRead.instructions,
      `public helper read should reuse prepared statements: public=${formatReport(publicHelperRead)}, prepareEach=${formatReport(prepareEachRead)}`,
    );
    const readProfile = await callReport(
      "bench_read_profile",
      readActor.bench_read_profile(1_000),
    );
    assertReadProfile(read, readProfile);
    await callReport("db_stats_after_point_read_clean", readActor.db_stats());

    const writeActor = await setupActor(pic);
    await callReport("bench_reset_1000_for_write", writeActor.bench_reset(1_000));
    const write = await callReport("bench_write_1000_clean", writeActor.bench_write(1_000));
    assertWithinBaseline("bench_write_1000_clean", write, 13_174_459n, 25n, 10n);
    await callReport("db_stats_after_write_clean", writeActor.db_stats());

    const writeProfileActor = await setupActor(pic);
    await callReport("bench_reset_1000_for_write_profile", writeProfileActor.bench_reset(1_000));
    const writeProfile = await callReport(
      "bench_write_profile",
      writeProfileActor.bench_write_profile(1_000),
    );
    assertWriteProfile(write, writeProfile);

    await scenario(pic, "insert_only_1000", async (actor) => {
      await assertLimitCase("bench_insert_only_1000", actor.bench_insert_only(1_000));
      await callReport("db_stats_after_insert_only_1000", actor.db_stats());
    });
    const insert5000 = await scenario(pic, "insert_only_5000", async (actor) => {
      const report = await assertLimitCase("bench_insert_only_5000", actor.bench_insert_only(5_000));
      await callReport("db_stats_after_insert_only_5000", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_insert_only_5000", insert5000, 60_218_159n, 25n, 10n);
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
    assertWithinBaseline("bench_update_only_5000", update5000, 90_402_768n, 25n, 10n);

    const bulkActor = await setupActor(pic);
    await callReport("bench_reset_5000_clean", bulkActor.bench_reset(5_000));
    await assertLimitCase("bench_many_rows_100", bulkActor.bench_many_rows(100));
    await assertLimitCase("bench_many_rows_1000", bulkActor.bench_many_rows(1_000));
    const bulk5000 = await assertLimitCase("bench_many_rows_5000", bulkActor.bench_many_rows(5_000));
    assertWithinBaseline("bench_many_rows_5000", bulk5000, 6_460_904n, 25n, 10n);
    const getMany100 = await assertLimitCase(
      "bench_get_many_in_100",
      bulkActor.bench_get_many_in(100),
    );
    const getMany1000 = await assertLimitCase(
      "bench_get_many_in_1000",
      bulkActor.bench_get_many_in(1_000),
    );
    assertWithinBaseline("bench_get_many_in_100", getMany100, 1_313_458n, 25n, 10n);
    assertWithinBaseline("bench_get_many_in_1000", getMany1000, 14_353_642n, 25n, 10n);
    const getManyProfile = await callReport(
      "bench_get_many_in_profile",
      bulkActor.bench_get_many_in_profile(1_000),
    );
    assertGetManyProfile(getMany1000, getManyProfile);
    await callReport("db_stats_after_5000_clean", bulkActor.db_stats());

    const largeBlob64k = await scenario(pic, "large_blob_64k", async (actor) => {
      const report = await assertLimitCase(
        "bench_large_blob_64k",
        actor.bench_large_blob(64 * 1024),
      );
      await callReport("db_stats_after_large_blob_64k", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_large_blob_64k", largeBlob64k, 918_765n, 25n, 10n);
    const largeBlob256k = await scenario(pic, "large_blob_256k", async (actor) => {
      const report = await assertLimitCase(
        "bench_large_blob_256k",
        actor.bench_large_blob(256 * 1024),
      );
      await callReport("db_stats_after_large_blob_256k", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_large_blob_256k", largeBlob256k, 2_188_597n, 25n, 10n);

    const orderBy = await scenario(pic, "unbounded_order_by_5000", async (actor) => {
      const report = await assertLimitCase(
        "bench_unbounded_order_by_5000",
        actor.bench_unbounded_order_by(5_000),
      );
      await callReport("db_stats_after_unbounded_order_by_5000", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_unbounded_order_by_5000", orderBy, 65_926_719n, 25n, 10n);
    const join = await scenario(pic, "join_2000", async (actor) => {
      const report = await assertLimitCase("bench_join_2000", actor.bench_join(2_000));
      await callReport("db_stats_after_join_2000", actor.db_stats());
      return report;
    });
    assertWithinBaseline("bench_join_2000", join, 16_613_620n, 25n, 10n);
    const growth1k = await scenario(pic, "growth_1000_20", async (actor) =>
      assertLimitCase("bench_growth_1000_20", actor.bench_growth(1_000, 20)),
    );
    const growth5k = await scenario(pic, "growth_5000_20", async (actor) =>
      assertLimitCase("bench_growth_5000_20", actor.bench_growth(5_000, 20)),
    );
    assertWithinBaseline("bench_growth_1000_20", growth1k, 2_631_116n, 25n, 10n);
    assertWithinBaseline("bench_growth_5000_20", growth5k, 2_670_918n, 25n, 10n);
    assert(
      growth5k.instructions <= growth1k.instructions * 2n,
      `bench_growth should not scale with DB size: 1k=${formatReport(growth1k)}, 5k=${formatReport(growth5k)}`,
    );
    const growthProfile = await scenario(pic, "growth_profile_1000_20", async (actor) =>
      callReport("bench_growth_profile_1000_20", actor.bench_growth_profile(1_000, 20)),
    );
    assertGrowthProfile(growth1k, growthProfile, 20);
    const capacity1k = await scenario(pic, "capacity_growth_1000_128", async (actor) =>
      callReport(
        "bench_capacity_growth_guard_1000_128",
        actor.bench_capacity_growth_guard(1_000, 128),
      ),
    );
    assertCapacityGrowthGuard(capacity1k);
    const capacity5k = await scenario(pic, "capacity_growth_5000_256", async (actor) =>
      callReport(
        "bench_capacity_growth_guard_5000_256",
        actor.bench_capacity_growth_guard(5_000, 256),
      ),
    );
    assertCapacityGrowthGuard(capacity5k);
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
  assert(
    profile.x_read_calls <= 5n,
    `bench_read_profile should keep read-only pager reads cached: ${formatReport(profile)}`,
  );
  assert(profile.stable_data_read_calls > 0n, "bench_read_profile reported no stable reads");
}

function assertGetManyProfile(getMany, profile) {
  assert.equal(profile.rows, getMany.rows, "get_many profile row count differs");
  assert.equal(profile.checksum, getMany.checksum, "get_many profile checksum differs");
  assert(
    profile.instructions <= getMany.instructions * 2n,
    `bench_get_many_in_profile is too far from bench_get_many_in: getMany=${formatReport(getMany)}, profile=${formatReport(profile)}`,
  );

  const topLevel =
    profile.open_query +
    profile.sql_build +
    profile.key_build +
    profile.prepare +
    profile.bind +
    profile.row_scan +
    profile.report;
  assert(topLevel > 0n, "bench_get_many_in_profile reported no top-level work");
  assert(
    topLevel <= profile.instructions,
    `get_many profile top-level timings exceed total instructions: topLevel=${topLevel}, profile=${formatReport(profile)}`,
  );
  assert(profile.sql_build > 0n, "bench_get_many_in_profile reported no SQL build work");
  assert(profile.key_build > 0n, "bench_get_many_in_profile reported no key build work");
  assert(profile.bind > 0n, "bench_get_many_in_profile reported no bind work");
  assert(profile.row_scan > 0n, "bench_get_many_in_profile reported no row scan work");
  assert(profile.x_read_calls > 0n, "bench_get_many_in_profile reported no xRead calls");
  assert(profile.stable_data_read_calls > 0n, "bench_get_many_in_profile reported no stable reads");
}

function assertWriteProfile(write, profile) {
  assert.equal(profile.rows, write.rows, "profile row count differs from bench_write");
  assert.equal(profile.checksum, write.checksum, "profile checksum differs from bench_write");
  assert(
    profile.instructions <= write.instructions * 2n,
    `bench_write_profile is too far from bench_write: write=${formatReport(write)}, profile=${formatReport(profile)}`,
  );

  const topLevel =
    profile.open_update +
    profile.prepare +
    profile.key_value_format +
    profile.execute_total +
    profile.report;
  assert(topLevel > 0n, "bench_write_profile reported no top-level work");
  assert(
    topLevel <= profile.instructions,
    `write profile top-level timings exceed total instructions: topLevel=${topLevel}, profile=${formatReport(profile)}`,
  );

  const statementParts = profile.reset_bind + profile.step;
  assert(statementParts > 0n, "bench_write_profile reported no statement work");
  assert(
    statementParts <= profile.execute_total,
    `write statement timings exceed execute total: statementParts=${statementParts}, profile=${formatReport(profile)}`,
  );

  assert(profile.x_write_calls > 0n, "bench_write_profile reported no xWrite calls");
  assert(profile.stable_data_write_calls > 0n, "bench_write_profile reported no stable writes");
  assert(profile.commit_capacity > 0n, "bench_write_profile reported no commit capacity work");
  assert(profile.commit_page_write > 0n, "bench_write_profile reported no commit page write work");
  assert(
    profile.commit_superblock_store > 0n,
    "bench_write_profile reported no commit superblock store work",
  );

  const commitParts =
    profile.commit_capacity +
    profile.commit_page_write +
    profile.commit_superblock_store;
  assert(commitParts > 0n, "bench_write_profile reported no commit work");
  assert(
    commitParts <= profile.instructions,
    `write commit timings exceed total instructions: commitParts=${commitParts}, profile=${formatReport(profile)}`,
  );
}

function assertGrowthProfile(growth, profile, expectedWrites) {
  assert.equal(profile.rows, growth.rows, "growth profile row count differs");
  assert.equal(growth.checksum, BigInt(expectedWrites), "growth write count differs");
  assert.equal(profile.writes, BigInt(expectedWrites), "growth profile expected write count differs");
  assert.equal(profile.writes, growth.checksum, "growth profile write count differs");
  assert.equal(profile.checksum, growth.checksum, "growth profile checksum differs");
  assert(
    profile.instructions <= growth.instructions * 2n,
    `bench_growth_profile is too far from bench_growth: growth=${formatReport(growth)}, profile=${formatReport(profile)}`,
  );

  const topLevel =
    profile.open_update +
    profile.key_value_format +
    profile.prepare +
    profile.execute_total +
    profile.changes +
    profile.report;
  assert(topLevel > 0n, "bench_growth_profile reported no top-level work");
  assert(
    topLevel <= profile.instructions,
    `growth profile top-level timings exceed total instructions: topLevel=${topLevel}, profile=${formatReport(profile)}`,
  );
  assert(profile.open_update > 0n, "bench_growth_profile reported no update open work");
  assert(profile.prepare > 0n, "bench_growth_profile reported no prepare work");
  assert(profile.execute_total > 0n, "bench_growth_profile reported no execute work");
  assert(profile.x_write_calls > 0n, "bench_growth_profile reported no xWrite calls");
  assert(profile.stable_data_write_calls > 0n, "bench_growth_profile reported no stable writes");
  assert(profile.commit_capacity > 0n, "bench_growth_profile reported no commit capacity work");
  assert(profile.commit_page_write > 0n, "bench_growth_profile reported no commit page write work");
  assert(
    profile.commit_superblock_store > 0n,
    "bench_growth_profile reported no commit superblock store work",
  );

  const commitParts =
    profile.commit_capacity +
    profile.commit_page_write +
    profile.commit_superblock_store;
  assert(commitParts > 0n, "bench_growth_profile reported no commit work");
  assert(
    commitParts <= profile.instructions,
    `growth commit timings exceed total instructions: commitParts=${commitParts}, profile=${formatReport(profile)}`,
  );
}

function assertCapacityGrowthGuard(report) {
  assert.equal(report.db_size_after, report.db_size_before, "db size changed");
  assert.equal(report.db_base_offset_after, report.db_base_offset_before, "db base moved");
  assert.equal(report.page_table_offset_before, 0n, "page table offset was present before");
  assert.equal(report.page_table_offset_after, 0n, "page table offset was present after");
  assert.equal(report.page_table_bytes_before, 0n, "page table bytes were present before");
  assert.equal(report.page_table_bytes_after, 0n, "page table bytes were present after");
  assert.equal(report.stable_pages_after, report.stable_pages_before, "stable pages grew");
  assert.equal(
    report.allocated_bytes_after,
    report.allocated_bytes_before,
    "allocated bytes grew",
  );
  assert.equal(
    report.orphan_bytes_estimate_after,
    report.orphan_bytes_estimate_before,
    "orphan bytes estimate changed",
  );
  assert.equal(report.stable_grow_calls, 0n, "stable grow was called");
  assert.equal(report.stable_grow_pages, 0n, "stable grow allocated pages");
}

function formatReport(report) {
  const fields = [
    "rows" in report ? `rows=${report.rows}` : undefined,
    "writes" in report ? `writes=${report.writes}` : undefined,
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
  if ("query_optional_string_text_total" in report) {
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
      `superblock_loads=${report.superblock_loads}`,
    );
  }
  if ("sql_build" in report) {
    fields.push(
      `open_query=${report.open_query}`,
      `sql_build=${report.sql_build}`,
      `key_build=${report.key_build}`,
      `prepare=${report.prepare}`,
      `bind=${report.bind}`,
      `row_scan=${report.row_scan}`,
      `report=${report.report}`,
      `x_read_calls=${report.x_read_calls}`,
      `x_read_bytes=${report.x_read_bytes}`,
      `stable_data_read_calls=${report.stable_data_read_calls}`,
      `stable_data_read_bytes=${report.stable_data_read_bytes}`,
      `superblock_loads=${report.superblock_loads}`,
    );
  }
  if ("open_update" in report) {
    fields.push(
      `open_update=${report.open_update}`,
      `prepare=${report.prepare}`,
      `key_value_format=${report.key_value_format}`,
      `execute_total=${report.execute_total}`,
      "changes" in report ? `changes=${report.changes}` : undefined,
      "reset_bind" in report ? `reset_bind=${report.reset_bind}` : undefined,
      "step" in report ? `step=${report.step}` : undefined,
      `report=${report.report}`,
      `x_read_calls=${report.x_read_calls}`,
      `x_read_bytes=${report.x_read_bytes}`,
      `x_write_calls=${report.x_write_calls}`,
      `x_write_bytes=${report.x_write_bytes}`,
      `x_file_size_calls=${report.x_file_size_calls}`,
      `x_lock_calls=${report.x_lock_calls}`,
      `x_unlock_calls=${report.x_unlock_calls}`,
      `x_check_reserved_lock_calls=${report.x_check_reserved_lock_calls}`,
      `x_file_control_calls=${report.x_file_control_calls}`,
      `x_device_characteristics_calls=${report.x_device_characteristics_calls}`,
      `stable_data_read_calls=${report.stable_data_read_calls}`,
      `stable_data_read_bytes=${report.stable_data_read_bytes}`,
      `stable_data_write_calls=${report.stable_data_write_calls}`,
      `stable_data_write_bytes=${report.stable_data_write_bytes}`,
      `stable_grow_calls=${report.stable_grow_calls}`,
      `stable_grow_pages=${report.stable_grow_pages}`,
      `superblock_loads=${report.superblock_loads}`,
      `commit_load=${report.commit_load}`,
      `commit_capacity=${report.commit_capacity}`,
      `commit_page_write=${report.commit_page_write}`,
      `commit_superblock_store=${report.commit_superblock_store}`,
    );
  }
  if ("stable_grow_calls" in report && !("open_update" in report)) {
    fields.push(
      `db_size_before=${report.db_size_before}`,
      `db_size_after=${report.db_size_after}`,
      `db_base_offset_before=${report.db_base_offset_before}`,
      `db_base_offset_after=${report.db_base_offset_after}`,
      `page_table_offset_before=${report.page_table_offset_before}`,
      `page_table_offset_after=${report.page_table_offset_after}`,
      `page_table_bytes_before=${report.page_table_bytes_before}`,
      `page_table_bytes_after=${report.page_table_bytes_after}`,
      `stable_pages_before=${report.stable_pages_before}`,
      `stable_pages_after=${report.stable_pages_after}`,
      `allocated_bytes_before=${report.allocated_bytes_before}`,
      `allocated_bytes_after=${report.allocated_bytes_after}`,
      `orphan_bytes_estimate_before=${report.orphan_bytes_estimate_before}`,
      `orphan_bytes_estimate_after=${report.orphan_bytes_estimate_after}`,
      `stable_grow_calls=${report.stable_grow_calls}`,
      `stable_grow_pages=${report.stable_grow_pages}`,
    );
  }
  return fields.filter(Boolean).join(", ");
}
