//! Local IC benchmark canister for the README KV workload.
//!
//! Write reports intentionally avoid `Db::db_checksum()` so the measured
//! instruction count is the SQLite commit path, not a full DB verification scan.

use candid::CandidType;
use ic_cdk::{api::performance_counter, init, post_upgrade, query, update};
use ic_sqlite_vfs::bench_support::{memory, read_metrics, stable_blob, Superblock};
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::statement::{ExecuteTextTextProfile, QueryOptionalStringTextProfile};
use ic_sqlite_vfs::{Db, DefaultMemoryImpl, MemoryId, MemoryManager};
use serde::Deserialize;
use std::cell::RefCell;

mod key;

use key::{
    bench_key, bench_value, body_value, group_label, growth_value, order_value, prefixed_key,
    updated_value, validate_fixed_bench_key_index, validate_fixed_bench_key_range,
    validate_fixed_bench_key_rows, write_value,
};

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS bench (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    ) WITHOUT ROWID;",
}];
const POINT_READ_SQL: &str = "SELECT value FROM bench WHERE key = ?1";
const MULTI_GET_SQL_PREFIX: &[u8] = b"SELECT value FROM bench WHERE key IN (";
const MULTI_GET_SQL_SUFFIX: &[u8] = b") ORDER BY key";
const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

#[derive(CandidType, Deserialize)]
pub struct BenchReport {
    pub rows: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
}

#[derive(CandidType, Deserialize)]
pub struct DbStatsReport {
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub sqlite_page_size: u64,
    pub sqlite_page_count: u64,
    pub sqlite_freelist_count: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchChurnStepReport {
    pub cycle: u64,
    pub phase: String,
    pub rows: u64,
    pub instructions: u64,
    pub row_count: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub sqlite_page_size: u64,
    pub sqlite_page_count: u64,
    pub sqlite_freelist_count: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchReadProfileReport {
    pub rows: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub open_query: u64,
    pub prepare: u64,
    pub key_format: u64,
    pub query_optional_string_text_total: u64,
    pub reset_bind: u64,
    pub step: u64,
    pub column_read: u64,
    pub report: u64,
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub superblock_loads: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchGetManyProfileReport {
    pub rows: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub open_query: u64,
    pub sql_build: u64,
    pub key_build: u64,
    pub prepare: u64,
    pub bind: u64,
    pub row_scan: u64,
    pub report: u64,
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub superblock_loads: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchWriteProfileReport {
    pub rows: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub open_update: u64,
    pub prepare: u64,
    pub key_value_format: u64,
    pub execute_total: u64,
    pub reset_bind: u64,
    pub step: u64,
    pub report: u64,
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub x_write_calls: u64,
    pub x_write_bytes: u64,
    pub x_file_size_calls: u64,
    pub x_lock_calls: u64,
    pub x_unlock_calls: u64,
    pub x_check_reserved_lock_calls: u64,
    pub x_file_control_calls: u64,
    pub x_device_characteristics_calls: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub stable_data_write_calls: u64,
    pub stable_data_write_bytes: u64,
    pub stable_grow_calls: u64,
    pub stable_grow_pages: u64,
    pub superblock_loads: u64,
    pub commit_load: u64,
    pub commit_capacity: u64,
    pub commit_page_write: u64,
    pub commit_superblock_store: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchGrowthProfileReport {
    pub rows: u64,
    pub writes: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub open_update: u64,
    pub key_value_format: u64,
    pub prepare: u64,
    pub execute_total: u64,
    pub changes: u64,
    pub report: u64,
    pub x_read_calls: u64,
    pub x_read_bytes: u64,
    pub x_write_calls: u64,
    pub x_write_bytes: u64,
    pub x_file_size_calls: u64,
    pub x_lock_calls: u64,
    pub x_unlock_calls: u64,
    pub x_check_reserved_lock_calls: u64,
    pub x_file_control_calls: u64,
    pub x_device_characteristics_calls: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
    pub stable_data_write_calls: u64,
    pub stable_data_write_bytes: u64,
    pub stable_grow_calls: u64,
    pub stable_grow_pages: u64,
    pub superblock_loads: u64,
    pub commit_load: u64,
    pub commit_capacity: u64,
    pub commit_page_write: u64,
    pub commit_superblock_store: u64,
}

#[derive(CandidType, Deserialize)]
pub struct BenchCapacityGrowthReport {
    pub rows: u64,
    pub writes: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub db_size_before: u64,
    pub db_size_after: u64,
    pub db_base_offset_before: u64,
    pub db_base_offset_after: u64,
    pub page_table_offset_before: u64,
    pub page_table_offset_after: u64,
    pub page_table_bytes_before: u64,
    pub page_table_bytes_after: u64,
    pub stable_pages_before: u64,
    pub stable_pages_after: u64,
    pub allocated_bytes_before: u64,
    pub allocated_bytes_after: u64,
    pub orphan_bytes_estimate_before: u64,
    pub orphan_bytes_estimate_after: u64,
    pub stable_grow_calls: u64,
    pub stable_grow_pages: u64,
}

#[init]
fn init() {
    init_db();
    must(Db::migrate(MIGRATIONS));
}

#[post_upgrade]
fn post_upgrade() {
    init_db();
    must(Db::migrate(MIGRATIONS));
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        must(Db::init(manager.borrow().get(SQLITE_MEMORY_ID)));
    });
}

#[update]
fn bench_reset(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        reset_bench_table(connection)?;
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = bench_key(index, &mut key);
            let value = bench_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_insert_only(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    Db::update(|connection| reset_bench_table(connection)).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = bench_key(index, &mut key);
            let value = bench_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_append_insert(base_rows: u32, append_rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(base_rows)?;
    validate_fixed_bench_key_range(base_rows, append_rows)?;
    seed_bench_rows(base_rows).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..append_rows {
            let row = base_rows
                .checked_add(index)
                .ok_or(ic_sqlite_vfs::DbError::TooManyParameters)?;
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = bench_key(row, &mut key);
            let value = bench_value(row, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(append_rows, start, u64::from(append_rows))
}

#[update]
fn bench_update_only(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    seed_bench_rows(rows).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("UPDATE bench SET value = ?1 WHERE key = ?2")?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 27];
            let key = bench_key(index, &mut key);
            let value = updated_value(index, &mut value);
            statement.execute_text_text(&value, &key)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[query]
fn bench_read(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    warm_point_read_statement()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut total = 0_u64;
        let mut statement = connection.prepare_cached(POINT_READ_SQL)?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let key = bench_key(index, &mut key);
            if let Some(len) = statement.query_optional_string_text_len(key)? {
                total = total.wrapping_add(len as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn bench_read_public_helper(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut total = 0_u64;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let key = bench_key(index, &mut key);
            if let Some(value) = connection.query_optional_string_text(POINT_READ_SQL, key)? {
                total = total.wrapping_add(value.len() as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn bench_read_prepare_each(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut total = 0_u64;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let key = bench_key(index, &mut key);
            let mut statement = connection.prepare(POINT_READ_SQL)?;
            if let Some(value) = statement.query_optional_string_text(key)? {
                total = total.wrapping_add(value.len() as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn bench_get_many_in(rows: u32) -> Result<BenchReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_rows(rows)?;
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let sql = multi_get_sql(rows)?;
        let keys = bench_key_buffers(rows);
        connection.query_text_iter_text_len_sum(&sql, keys.iter().map(bench_key_buffer_str))
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn bench_get_many_in_profile(rows: u32) -> Result<BenchGetManyProfileReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_rows(rows)?;
    warm_read_connection()?;
    read_metrics::reset_read_metrics();
    let start = performance_counter(0);
    let mut profile = BenchGetManyProfile::default();
    let checksum = Db::query(|connection| {
        profile.open_query = performance_counter(0).saturating_sub(start);

        let sql_start = performance_counter(0);
        let sql = multi_get_sql(rows)?;
        profile.sql_build = performance_counter(0).saturating_sub(sql_start);

        let key_start = performance_counter(0);
        let keys = bench_key_buffers(rows);
        profile.key_build = performance_counter(0).saturating_sub(key_start);

        let prepare_start = performance_counter(0);
        let mut statement = connection.prepare_cached(&sql)?;
        profile.prepare = performance_counter(0).saturating_sub(prepare_start);

        let (total, statement_profile) =
            statement.query_text_iter_text_len_sum_profiled(keys.iter().map(bench_key_buffer_str))?;
        profile.bind = statement_profile.reset_bind;
        profile.row_scan = statement_profile.row_scan;
        Ok(total)
    })
    .map_err(error_text)?;

    let report_start = performance_counter(0);
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let stable_bytes = stable_pages
        .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    profile.report = performance_counter(0).saturating_sub(report_start);
    let metrics = read_metrics::read_metrics_snapshot();
    read_metrics::disable_read_metrics();

    Ok(BenchGetManyProfileReport {
        rows: u64::from(rows),
        instructions: performance_counter(0).saturating_sub(start),
        checksum,
        db_size: block.db_size,
        stable_pages,
        stable_bytes,
        open_query: profile.open_query,
        sql_build: profile.sql_build,
        key_build: profile.key_build,
        prepare: profile.prepare,
        bind: profile.bind,
        row_scan: profile.row_scan,
        report: profile.report,
        x_read_calls: metrics.x_read_calls,
        x_read_bytes: metrics.x_read_bytes,
        stable_data_read_calls: metrics.stable_data_read_calls,
        stable_data_read_bytes: metrics.stable_data_read_bytes,
        superblock_loads: metrics.superblock_loads,
    })
}

#[query]
fn db_stats() -> Result<DbStatsReport, String> {
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = sqlite_stats()?;
    Ok(DbStatsReport {
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
        sqlite_page_size,
        sqlite_page_count,
        sqlite_freelist_count,
    })
}

#[update]
fn bench_churn_reset(base_rows: u32) -> Result<BenchChurnStepReport, String> {
    validate_fixed_bench_key_rows(base_rows)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        reset_churn_table(connection)?;
        let mut statement =
            connection.prepare("INSERT INTO churn_bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..base_rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = prefixed_key(b'c', index, &mut key);
            let value = bench_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    churn_report(0, "reset", base_rows, start)
}

#[update]
fn bench_churn_delete(
    start_index: u32,
    rows: u32,
    cycle: u32,
) -> Result<BenchChurnStepReport, String> {
    validate_churn_range(start_index, rows)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("DELETE FROM churn_bench WHERE key = ?1")?;
        for offset in 0..rows {
            let index = start_index
                .checked_add(offset)
                .ok_or(ic_sqlite_vfs::DbError::TooManyParameters)?;
            let mut key = [0_u8; 9];
            let key = prefixed_key(b'c', index, &mut key);
            statement.execute(ic_sqlite_vfs::params![key])?;
            if connection.changes() != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
        }
        Ok(())
    })
    .map_err(error_text)?;
    churn_report(cycle, "delete", rows, start)
}

#[update]
fn bench_churn_insert(
    start_index: u32,
    rows: u32,
    cycle: u32,
) -> Result<BenchChurnStepReport, String> {
    validate_churn_range(start_index, rows)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement =
            connection.prepare("INSERT INTO churn_bench(key, value) VALUES (?1, ?2)")?;
        for offset in 0..rows {
            let index = start_index
                .checked_add(offset)
                .ok_or(ic_sqlite_vfs::DbError::TooManyParameters)?;
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = prefixed_key(b'c', index, &mut key);
            let value = bench_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    churn_report(cycle, "insert", rows, start)
}

#[query]
fn bench_read_profile(rows: u32) -> Result<BenchReadProfileReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    warm_point_read_statement()?;
    read_metrics::reset_read_metrics();
    let start = performance_counter(0);
    let mut profile = BenchReadProfile::default();
    let checksum = Db::query(|connection| {
        profile.open_query = performance_counter(0).saturating_sub(start);

        let prepare_start = performance_counter(0);
        let mut statement = connection.prepare_cached(POINT_READ_SQL)?;
        profile.prepare = performance_counter(0).saturating_sub(prepare_start);

        let mut total = 0_u64;
        for index in 0..rows {
            let key_start = performance_counter(0);
            let mut key = [0_u8; 9];
            let key = bench_key(index, &mut key);
            profile.key_format = profile
                .key_format
                .saturating_add(performance_counter(0).saturating_sub(key_start));

            let query_start = performance_counter(0);
            let (value, statement_profile) =
                statement.query_optional_string_text_len_profiled(key)?;
            profile.query_optional_string_text_total = profile
                .query_optional_string_text_total
                .saturating_add(performance_counter(0).saturating_sub(query_start));
            profile.add_statement(statement_profile);

            if let Some(len) = value {
                total = total.wrapping_add(len as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;

    let report_start = performance_counter(0);
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let stable_bytes = stable_pages
        .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    profile.report = performance_counter(0).saturating_sub(report_start);
    let metrics = read_metrics::read_metrics_snapshot();
    read_metrics::disable_read_metrics();

    Ok(BenchReadProfileReport {
        rows: u64::from(rows),
        instructions: performance_counter(0).saturating_sub(start),
        checksum,
        db_size: block.db_size,
        stable_pages,
        stable_bytes,
        open_query: profile.open_query,
        prepare: profile.prepare,
        key_format: profile.key_format,
        query_optional_string_text_total: profile.query_optional_string_text_total,
        reset_bind: profile.reset_bind,
        step: profile.step,
        column_read: profile.column_read,
        report: profile.report,
        x_read_calls: metrics.x_read_calls,
        x_read_bytes: metrics.x_read_bytes,
        stable_data_read_calls: metrics.stable_data_read_calls,
        stable_data_read_bytes: metrics.stable_data_read_bytes,
        superblock_loads: metrics.superblock_loads,
    })
}

#[update]
fn bench_write(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare(
            "INSERT INTO bench(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 27];
            let key = prefixed_key(b'w', index, &mut key);
            let value = updated_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_write_profile(rows: u32) -> Result<BenchWriteProfileReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    read_metrics::reset_read_metrics();
    let start = performance_counter(0);
    let mut profile = BenchWriteProfile::default();
    Db::update(|connection| {
        profile.open_update = performance_counter(0).saturating_sub(start);

        let prepare_start = performance_counter(0);
        let mut statement = connection.prepare(
            "INSERT INTO bench(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?;
        profile.prepare = performance_counter(0).saturating_sub(prepare_start);

        for index in 0..rows {
            let format_start = performance_counter(0);
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 27];
            let key = prefixed_key(b'w', index, &mut key);
            let value = updated_value(index, &mut value);
            profile.key_value_format = profile
                .key_value_format
                .saturating_add(performance_counter(0).saturating_sub(format_start));

            let execute_start = performance_counter(0);
            let statement_profile = statement.execute_text_text_profiled(&key, &value)?;
            profile.execute_total = profile
                .execute_total
                .saturating_add(performance_counter(0).saturating_sub(execute_start));
            profile.add_statement(statement_profile);
        }
        Ok(())
    })
    .map_err(error_text)?;

    let report_start = performance_counter(0);
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let stable_bytes = stable_pages
        .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    profile.report = performance_counter(0).saturating_sub(report_start);
    let metrics = read_metrics::read_metrics_snapshot();
    read_metrics::disable_read_metrics();

    Ok(BenchWriteProfileReport {
        rows: u64::from(rows),
        instructions: performance_counter(0).saturating_sub(start),
        checksum: u64::from(rows),
        db_size: block.db_size,
        stable_pages,
        stable_bytes,
        open_update: profile.open_update,
        prepare: profile.prepare,
        key_value_format: profile.key_value_format,
        execute_total: profile.execute_total,
        reset_bind: profile.reset_bind,
        step: profile.step,
        report: profile.report,
        x_read_calls: metrics.x_read_calls,
        x_read_bytes: metrics.x_read_bytes,
        x_write_calls: metrics.x_write_calls,
        x_write_bytes: metrics.x_write_bytes,
        x_file_size_calls: metrics.x_file_size_calls,
        x_lock_calls: metrics.x_lock_calls,
        x_unlock_calls: metrics.x_unlock_calls,
        x_check_reserved_lock_calls: metrics.x_check_reserved_lock_calls,
        x_file_control_calls: metrics.x_file_control_calls,
        x_device_characteristics_calls: metrics.x_device_characteristics_calls,
        stable_data_read_calls: metrics.stable_data_read_calls,
        stable_data_read_bytes: metrics.stable_data_read_bytes,
        stable_data_write_calls: metrics.stable_data_write_calls,
        stable_data_write_bytes: metrics.stable_data_write_bytes,
        stable_grow_calls: metrics.stable_grow_calls,
        stable_grow_pages: metrics.stable_grow_pages,
        superblock_loads: metrics.superblock_loads,
        commit_load: metrics.commit_load,
        commit_capacity: metrics.commit_capacity,
        commit_page_write: metrics.commit_page_write,
        commit_superblock_store: metrics.commit_superblock_store,
    })
}

#[update]
fn bench_large_blob(bytes: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    let payload = vec![0x5a_u8; usize::try_from(bytes).map_err(|_| "blob too large")?];
    let checksum = Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS blob_bench;
             CREATE TABLE blob_bench (
                id INTEGER PRIMARY KEY,
                body BLOB NOT NULL
             );",
        )?;
        let mut insert = connection.prepare("INSERT INTO blob_bench(id, body) VALUES (?1, ?2)")?;
        insert.execute_i64_blob(1, &payload)?;
        connection.query_scalar::<i64>(
            "SELECT length(body) FROM blob_bench WHERE id = 1",
            ic_sqlite_vfs::params![],
        )
    })
    .map_err(error_text)?;
    report(
        bytes,
        start,
        u64::try_from(checksum).map_err(|_| "negative blob length")?,
    )
}

#[query]
fn bench_many_rows(rows: u32) -> Result<BenchReport, String> {
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT value FROM bench ORDER BY key LIMIT ?1")?;
        let mut rows = statement.query_i64(i64::from(rows))?;
        let mut total = 0_u64;
        while let Some(len) = rows.next_text_len_zero()? {
            total = total.wrapping_add(len as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[update]
fn bench_unbounded_order_by(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_index(rows)?;
    let start = performance_counter(0);
    let checksum = Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS order_bench;
             CREATE TABLE order_bench (
                id INTEGER PRIMARY KEY,
                value TEXT NOT NULL
             );",
        )?;
        let mut insert =
            connection.prepare("INSERT INTO order_bench(id, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let id = i64::from(index);
            let mut value = [0_u8; 14];
            let value = order_value(rows - index, &mut value);
            insert.execute_i64_text(id, &value)?;
        }
        let mut query = connection.prepare("SELECT value FROM order_bench ORDER BY value")?;
        let mut rows = query.query(ic_sqlite_vfs::params![])?;
        let mut total = 0_u64;
        while let Some(len) = rows.next_text_len_zero()? {
            total = total.wrapping_add(len as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[update]
fn bench_join(rows: u32) -> Result<BenchReport, String> {
    validate_fixed_bench_key_rows(rows)?;
    let start = performance_counter(0);
    let checksum = Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS join_left;
             DROP TABLE IF EXISTS join_right;
             CREATE TABLE join_left (
                id INTEGER PRIMARY KEY,
                group_id INTEGER NOT NULL,
                body TEXT NOT NULL
             );
             CREATE TABLE join_right (
                group_id INTEGER PRIMARY KEY,
                label TEXT NOT NULL
             );",
        )?;
        let mut left =
            connection.prepare("INSERT INTO join_left(id, group_id, body) VALUES (?1, ?2, ?3)")?;
        let mut right =
            connection.prepare("INSERT INTO join_right(group_id, label) VALUES (?1, ?2)")?;
        for group in 0..100_i64 {
            let mut label = [0_u8; 9];
            let label = group_label(group, &mut label);
            right.execute_i64_text(group, &label)?;
        }
        for index in 0..rows {
            let id = i64::from(index);
            let group = id % 100;
            let mut body = [0_u8; 13];
            let body = body_value(index, &mut body);
            left.execute_i64_i64_text(id, group, &body)?;
        }
        connection.query_scalar::<i64>(
            "SELECT COUNT(*)
             FROM join_left
             JOIN join_right ON join_left.group_id = join_right.group_id",
            ic_sqlite_vfs::params![],
        )
    })
    .map_err(error_text)?;
    report(
        rows,
        start,
        u64::try_from(checksum).map_err(|_| "negative join count")?,
    )
}

#[update]
fn bench_growth(rows: u32, writes: u32) -> Result<BenchReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_rows(rows)?;
    validate_fixed_bench_key_rows(writes)?;
    seed_growth_rows(rows).map_err(error_text)?;

    let start = performance_counter(0);
    for index in 0..writes {
        Db::update(|connection| {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 14];
            let key = prefixed_key(b'g', index % rows, &mut key);
            let value = write_value(index, &mut value);
            let mut statement =
                connection.prepare_cached("UPDATE growth_bench SET value = ?1 WHERE key = ?2")?;
            statement.execute_text_text(&value, &key)?;
            if connection.changes() != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
            Ok(())
        })
        .map_err(error_text)?;
    }

    report(rows, start, u64::from(writes))
}

#[update]
fn bench_growth_profile(rows: u32, writes: u32) -> Result<BenchGrowthProfileReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_rows(rows)?;
    validate_fixed_bench_key_rows(writes)?;
    seed_growth_rows(rows).map_err(error_text)?;

    read_metrics::reset_read_metrics();
    let start = performance_counter(0);
    let mut profile = BenchGrowthProfile::default();
    for index in 0..writes {
        let update_start = performance_counter(0);
        Db::update(|connection| {
            profile.open_update = profile
                .open_update
                .saturating_add(performance_counter(0).saturating_sub(update_start));

            let format_start = performance_counter(0);
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 14];
            let key = prefixed_key(b'g', index % rows, &mut key);
            let value = write_value(index, &mut value);
            profile.key_value_format = profile
                .key_value_format
                .saturating_add(performance_counter(0).saturating_sub(format_start));

            let prepare_start = performance_counter(0);
            let mut statement =
                connection.prepare_cached("UPDATE growth_bench SET value = ?1 WHERE key = ?2")?;
            profile.prepare = profile
                .prepare
                .saturating_add(performance_counter(0).saturating_sub(prepare_start));

            let execute_start = performance_counter(0);
            statement.execute_text_text(&value, &key)?;
            profile.execute_total = profile
                .execute_total
                .saturating_add(performance_counter(0).saturating_sub(execute_start));

            let changes_start = performance_counter(0);
            let changed = connection.changes();
            profile.changes = profile
                .changes
                .saturating_add(performance_counter(0).saturating_sub(changes_start));
            if changed != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
            Ok(())
        })
        .map_err(error_text)?;
    }

    let report_start = performance_counter(0);
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let stable_bytes = stable_pages
        .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    profile.report = performance_counter(0).saturating_sub(report_start);
    let metrics = read_metrics::read_metrics_snapshot();
    read_metrics::disable_read_metrics();

    Ok(BenchGrowthProfileReport {
        rows: u64::from(rows),
        writes: u64::from(writes),
        instructions: performance_counter(0).saturating_sub(start),
        checksum: u64::from(writes),
        db_size: block.db_size,
        stable_pages,
        stable_bytes,
        open_update: profile.open_update,
        key_value_format: profile.key_value_format,
        prepare: profile.prepare,
        execute_total: profile.execute_total,
        changes: profile.changes,
        report: profile.report,
        x_read_calls: metrics.x_read_calls,
        x_read_bytes: metrics.x_read_bytes,
        x_write_calls: metrics.x_write_calls,
        x_write_bytes: metrics.x_write_bytes,
        x_file_size_calls: metrics.x_file_size_calls,
        x_lock_calls: metrics.x_lock_calls,
        x_unlock_calls: metrics.x_unlock_calls,
        x_check_reserved_lock_calls: metrics.x_check_reserved_lock_calls,
        x_file_control_calls: metrics.x_file_control_calls,
        x_device_characteristics_calls: metrics.x_device_characteristics_calls,
        stable_data_read_calls: metrics.stable_data_read_calls,
        stable_data_read_bytes: metrics.stable_data_read_bytes,
        stable_data_write_calls: metrics.stable_data_write_calls,
        stable_data_write_bytes: metrics.stable_data_write_bytes,
        stable_grow_calls: metrics.stable_grow_calls,
        stable_grow_pages: metrics.stable_grow_pages,
        superblock_loads: metrics.superblock_loads,
        commit_load: metrics.commit_load,
        commit_capacity: metrics.commit_capacity,
        commit_page_write: metrics.commit_page_write,
        commit_superblock_store: metrics.commit_superblock_store,
    })
}

#[update]
fn bench_capacity_growth_guard(
    rows: u32,
    writes: u32,
) -> Result<BenchCapacityGrowthReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_rows(rows)?;
    validate_fixed_bench_key_rows(writes)?;
    seed_growth_rows(rows).map_err(error_text)?;

    let before_block = Superblock::load().map_err(|error| error.to_string())?;
    let before_stats = stable_blob::storage_stats().map_err(|error| error.to_string())?;
    let before_pages = memory::size_pages();
    read_metrics::reset_read_metrics();

    let start = performance_counter(0);
    for index in 0..writes {
        Db::update(|connection| {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 14];
            let key = prefixed_key(b'g', index % rows, &mut key);
            let value = write_value(index, &mut value);
            let mut statement =
                connection.prepare_cached("UPDATE growth_bench SET value = ?1 WHERE key = ?2")?;
            statement.execute_text_text(&value, &key)?;
            if connection.changes() != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
            Ok(())
        })
        .map_err(error_text)?;
    }
    let instructions = performance_counter(0).saturating_sub(start);

    let after_block = Superblock::load().map_err(|error| error.to_string())?;
    let after_stats = stable_blob::storage_stats().map_err(|error| error.to_string())?;
    let after_pages = memory::size_pages();
    let metrics = read_metrics::read_metrics_snapshot();
    read_metrics::disable_read_metrics();

    let report = BenchCapacityGrowthReport {
        rows: u64::from(rows),
        writes: u64::from(writes),
        instructions,
        checksum: u64::from(writes),
        db_size: after_block.db_size,
        stable_pages: after_pages,
        stable_bytes: after_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
        db_size_before: before_block.db_size,
        db_size_after: after_block.db_size,
        db_base_offset_before: before_block.db_base_offset,
        db_base_offset_after: after_block.db_base_offset,
        page_table_offset_before: before_block.page_table_offset,
        page_table_offset_after: after_block.page_table_offset,
        page_table_bytes_before: before_stats.page_table_bytes,
        page_table_bytes_after: after_stats.page_table_bytes,
        stable_pages_before: before_pages,
        stable_pages_after: after_pages,
        allocated_bytes_before: before_stats.allocated_bytes,
        allocated_bytes_after: after_stats.allocated_bytes,
        orphan_bytes_estimate_before: before_stats.orphan_bytes_estimate,
        orphan_bytes_estimate_after: after_stats.orphan_bytes_estimate,
        stable_grow_calls: metrics.stable_grow_calls,
        stable_grow_pages: metrics.stable_grow_pages,
    };

    verify_capacity_growth_report(&report)?;
    Ok(report)
}

fn reset_bench_table(connection: &ic_sqlite_vfs::db::connection::Connection) -> Result<(), ic_sqlite_vfs::DbError> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS bench;
         CREATE TABLE bench (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
         ) WITHOUT ROWID;",
    )
}

fn reset_churn_table(
    connection: &ic_sqlite_vfs::db::connection::Connection,
) -> Result<(), ic_sqlite_vfs::DbError> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS churn_bench;
         CREATE TABLE churn_bench (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
         ) WITHOUT ROWID;",
    )
}

fn seed_growth_rows(rows: u32) -> Result<(), ic_sqlite_vfs::DbError> {
    Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS growth_bench;
             CREATE TABLE growth_bench (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );",
        )?;
        let mut insert =
            connection.prepare("INSERT INTO growth_bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 26];
            let key = prefixed_key(b'g', index, &mut key);
            let value = growth_value(index, &mut value);
            insert.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
}

fn seed_bench_rows(rows: u32) -> Result<(), ic_sqlite_vfs::DbError> {
    Db::update(|connection| {
        reset_bench_table(connection)?;
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let mut value = [0_u8; 25];
            let key = bench_key(index, &mut key);
            let value = bench_value(index, &mut value);
            statement.execute_text_text(&key, &value)?;
        }
        Ok(())
    })
}

fn verify_capacity_growth_report(report: &BenchCapacityGrowthReport) -> Result<(), String> {
    if report.db_base_offset_after != report.db_base_offset_before {
        return Err(format!(
            "db_base_offset changed: before={} after={}",
            report.db_base_offset_before, report.db_base_offset_after
        ));
    }
    if report.db_size_after != report.db_size_before {
        return Err(format!(
            "db_size changed during existing-capacity updates: before={} after={}",
            report.db_size_before, report.db_size_after
        ));
    }
    if report.page_table_offset_before != 0 || report.page_table_offset_after != 0 {
        return Err(format!(
            "page table offset published: before={} after={}",
            report.page_table_offset_before, report.page_table_offset_after
        ));
    }
    if report.page_table_bytes_before != 0 || report.page_table_bytes_after != 0 {
        return Err(format!(
            "page table bytes allocated: before={} after={}",
            report.page_table_bytes_before, report.page_table_bytes_after
        ));
    }
    if report.stable_pages_after != report.stable_pages_before {
        return Err(format!(
            "stable pages grew during existing-capacity updates: before={} after={}",
            report.stable_pages_before, report.stable_pages_after
        ));
    }
    if report.allocated_bytes_after != report.allocated_bytes_before {
        return Err(format!(
            "allocated bytes grew during existing-capacity updates: before={} after={}",
            report.allocated_bytes_before, report.allocated_bytes_after
        ));
    }
    if report.orphan_bytes_estimate_after != report.orphan_bytes_estimate_before {
        return Err(format!(
            "orphan bytes estimate changed during existing-capacity updates: before={} after={}",
            report.orphan_bytes_estimate_before, report.orphan_bytes_estimate_after
        ));
    }
    if report.stable_grow_calls != 0 || report.stable_grow_pages != 0 {
        return Err(format!(
            "stable grow called during existing-capacity updates: calls={} pages={}",
            report.stable_grow_calls, report.stable_grow_pages
        ));
    }
    Ok(())
}

fn validate_churn_range(start: u32, rows: u32) -> Result<(), String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    validate_fixed_bench_key_range(start, rows)
}

fn sqlite_stats() -> Result<(u64, u64, u64), String> {
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = Db::query(|connection| {
        Ok((
            connection.query_scalar::<i64>("PRAGMA page_size", ic_sqlite_vfs::params![])?,
            connection.query_scalar::<i64>("PRAGMA page_count", ic_sqlite_vfs::params![])?,
            connection.query_scalar::<i64>("PRAGMA freelist_count", ic_sqlite_vfs::params![])?,
        ))
    })
    .map_err(error_text)?;
    Ok((
        u64::try_from(sqlite_page_size).map_err(|_| "negative page_size".to_string())?,
        u64::try_from(sqlite_page_count).map_err(|_| "negative page_count".to_string())?,
        u64::try_from(sqlite_freelist_count)
            .map_err(|_| "negative freelist_count".to_string())?,
    ))
}

fn churn_report(
    cycle: u32,
    phase: &str,
    rows: u32,
    start: u64,
) -> Result<BenchChurnStepReport, String> {
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = sqlite_stats()?;
    let row_count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM churn_bench", ic_sqlite_vfs::params![])
    })
    .map_err(error_text)?;
    Ok(BenchChurnStepReport {
        cycle: u64::from(cycle),
        phase: phase.to_string(),
        rows: u64::from(rows),
        instructions: performance_counter(0).saturating_sub(start),
        row_count: u64::try_from(row_count).map_err(|_| "negative row count".to_string())?,
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
        sqlite_page_size,
        sqlite_page_count,
        sqlite_freelist_count,
    })
}

fn multi_get_sql(count: u32) -> Result<String, ic_sqlite_vfs::DbError> {
    let capacity = usize::try_from(count).map_err(|_| ic_sqlite_vfs::DbError::TooManyParameters)?;
    let placeholder_len = capacity.saturating_mul(2).saturating_sub(1);
    let mut bytes = Vec::with_capacity(
        MULTI_GET_SQL_PREFIX
            .len()
            .saturating_add(placeholder_len)
            .saturating_add(MULTI_GET_SQL_SUFFIX.len()),
    );
    bytes.extend_from_slice(MULTI_GET_SQL_PREFIX);
    debug_assert!(capacity > 0);
    let placeholder_start = bytes.len();
    bytes.resize(placeholder_start + placeholder_len, b',');
    let mut index = placeholder_start;
    while index < bytes.len() {
        bytes[index] = b'?';
        index += 2;
    }
    bytes.extend_from_slice(MULTI_GET_SQL_SUFFIX);
    // SAFETY: the SQL is assembled from fixed ASCII fragments and placeholders.
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

fn bench_key_buffers(rows: u32) -> Vec<[u8; 9]> {
    let capacity = usize::try_from(rows).expect("row count fits usize");
    let mut keys = vec![[0_u8; 9]; capacity];
    for (index, key) in keys.iter_mut().enumerate() {
        bench_key(
            u32::try_from(index).expect("row count already came from u32"),
            key,
        );
    }
    keys
}

fn bench_key_buffer_str(key: &[u8; 9]) -> &str {
    // SAFETY: `bench_key_buffers` fills each buffer with ASCII `k` plus digits.
    unsafe { std::str::from_utf8_unchecked(key) }
}

#[derive(Default)]
struct BenchReadProfile {
    open_query: u64,
    prepare: u64,
    key_format: u64,
    query_optional_string_text_total: u64,
    reset_bind: u64,
    step: u64,
    column_read: u64,
    report: u64,
}

impl BenchReadProfile {
    fn add_statement(&mut self, profile: QueryOptionalStringTextProfile) {
        self.reset_bind = self.reset_bind.saturating_add(profile.reset_bind);
        self.step = self.step.saturating_add(profile.step);
        self.column_read = self.column_read.saturating_add(profile.column_read);
    }
}

#[derive(Default)]
struct BenchGetManyProfile {
    open_query: u64,
    sql_build: u64,
    key_build: u64,
    prepare: u64,
    bind: u64,
    row_scan: u64,
    report: u64,
}

#[derive(Default)]
struct BenchWriteProfile {
    open_update: u64,
    prepare: u64,
    key_value_format: u64,
    execute_total: u64,
    reset_bind: u64,
    step: u64,
    report: u64,
}

#[derive(Default)]
struct BenchGrowthProfile {
    open_update: u64,
    key_value_format: u64,
    prepare: u64,
    execute_total: u64,
    changes: u64,
    report: u64,
}

impl BenchWriteProfile {
    fn add_statement(&mut self, profile: ExecuteTextTextProfile) {
        self.reset_bind = self.reset_bind.saturating_add(profile.reset_bind);
        self.step = self.step.saturating_add(profile.step);
    }
}

fn report(rows: u32, start: u64, checksum: u64) -> Result<BenchReport, String> {
    let instructions = performance_counter(0).saturating_sub(start);
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    Ok(BenchReport {
        rows: u64::from(rows),
        instructions,
        checksum,
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
    })
}

fn must(result: Result<(), ic_sqlite_vfs::DbError>) {
    if let Err(error) = result {
        ic_cdk::trap(error.to_string());
    }
}

fn warm_read_connection() -> Result<(), String> {
    Db::query(|_| Ok(())).map_err(error_text)
}

fn warm_point_read_statement() -> Result<(), String> {
    Db::query(|connection| {
        let _statement = connection.prepare_cached(POINT_READ_SQL)?;
        Ok(())
    })
    .map_err(error_text)
}

fn error_text(error: ic_sqlite_vfs::DbError) -> String {
    error.to_string()
}
