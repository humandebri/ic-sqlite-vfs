//! Local IC benchmark canister for the README KV workload.
//!
//! Write reports intentionally avoid `Db::db_checksum()` so the measured
//! instruction count is the SQLite commit path, not a full DB verification scan.

use candid::CandidType;
use ic_cdk::{api::performance_counter, init, post_upgrade, query, update};
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::statement::QueryOptionalStringTextProfile;
use ic_sqlite_vfs::db::{TextLen, ToSql};
use ic_sqlite_vfs::read_metrics;
use ic_sqlite_vfs::stable::{memory, meta::Superblock};
use ic_sqlite_vfs::Db;
use ic_stable_structures::{
    memory_manager::{MemoryId, MemoryManager},
    DefaultMemoryImpl,
};
use serde::Deserialize;
use std::cell::RefCell;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS bench (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    ) WITHOUT ROWID;",
}];
const POINT_READ_SQL: &str = "SELECT value FROM bench WHERE key = ?1";
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
    pub page_table_root_hits: u64,
    pub page_table_root_misses: u64,
    pub page_table_segment_hits: u64,
    pub page_table_segment_misses: u64,
    pub superblock_loads: u64,
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
    let start = performance_counter(0);
    Db::update(|connection| {
        reset_bench_table(connection)?;
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_insert_only(rows: u32) -> Result<BenchReport, String> {
    Db::update(|connection| reset_bench_table(connection)).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_append_insert(base_rows: u32, append_rows: u32) -> Result<BenchReport, String> {
    seed_bench_rows(base_rows).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..append_rows {
            let row = base_rows
                .checked_add(index)
                .ok_or(ic_sqlite_vfs::DbError::TooManyParameters)?;
            let key = format!("k{row:08}");
            let value = format!("value-{row:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(append_rows, start, u64::from(append_rows))
}

#[update]
fn bench_update_only(rows: u32) -> Result<BenchReport, String> {
    seed_bench_rows(rows).map_err(error_text)?;
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare("UPDATE bench SET value = ?1 WHERE key = ?2")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("updated-{index:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![value, key])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[query]
fn bench_read(rows: u32) -> Result<BenchReport, String> {
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
fn bench_get_many_in(rows: u32) -> Result<BenchReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let sql = format!(
            "SELECT value FROM bench WHERE key IN ({}) ORDER BY key",
            placeholders(rows)?
        );
        let keys = bench_keys(rows);
        let values = keys
            .iter()
            .map(|key| key as &dyn ToSql)
            .collect::<Vec<_>>();
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query(&values)?;
        let mut total = 0_u64;
        while let Some(row) = rows.next_row()? {
            let len = row.get::<TextLen>(0)?.0;
            total = total.wrapping_add(len as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn db_stats() -> Result<DbStatsReport, String> {
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = Db::query(|connection| {
        Ok((
            connection.query_scalar::<i64>("PRAGMA page_size", ic_sqlite_vfs::params![])?,
            connection.query_scalar::<i64>("PRAGMA page_count", ic_sqlite_vfs::params![])?,
            connection.query_scalar::<i64>("PRAGMA freelist_count", ic_sqlite_vfs::params![])?,
        ))
    })
    .map_err(error_text)?;
    Ok(DbStatsReport {
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
        sqlite_page_size: u64::try_from(sqlite_page_size)
            .map_err(|_| "negative page_size".to_string())?,
        sqlite_page_count: u64::try_from(sqlite_page_count)
            .map_err(|_| "negative page_count".to_string())?,
        sqlite_freelist_count: u64::try_from(sqlite_freelist_count)
            .map_err(|_| "negative freelist_count".to_string())?,
    })
}

#[query]
fn bench_read_profile(rows: u32) -> Result<BenchReadProfileReport, String> {
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
        page_table_root_hits: metrics.page_table_root_hits,
        page_table_root_misses: metrics.page_table_root_misses,
        page_table_segment_hits: metrics.page_table_segment_hits,
        page_table_segment_misses: metrics.page_table_segment_misses,
        superblock_loads: metrics.superblock_loads,
    })
}

#[update]
fn bench_write(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    Db::update(|connection| {
        let mut statement = connection.prepare(
            "INSERT INTO bench(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?;
        for index in 0..rows {
            let key = format!("w{index:08}");
            let value = format!("updated-{index:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
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
        connection.execute(
            "INSERT INTO blob_bench(id, body) VALUES (?1, ?2)",
            ic_sqlite_vfs::params![1_i64, payload],
        )?;
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
        let mut rows = statement.query(ic_sqlite_vfs::params![i64::from(rows)])?;
        let mut total = 0_u64;
        while let Some(row) = rows.next_row()? {
            let len = row.get::<TextLen>(0)?.0;
            total = total.wrapping_add(len as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[update]
fn bench_unbounded_order_by(rows: u32) -> Result<BenchReport, String> {
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
            let value = format!("value-{:08}", rows - index);
            insert.execute(ic_sqlite_vfs::params![id, value])?;
        }
        let values = connection.query_column::<String>(
            "SELECT value FROM order_bench ORDER BY value",
            ic_sqlite_vfs::params![],
        )?;
        Ok(values.iter().map(|value| value.len() as u64).sum::<u64>())
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[update]
fn bench_join(rows: u32) -> Result<BenchReport, String> {
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
            let label = format!("group-{group:03}");
            right.execute(ic_sqlite_vfs::params![group, label])?;
        }
        for index in 0..rows {
            let id = i64::from(index);
            let group = id % 100;
            let body = format!("body-{index:08}");
            left.execute(ic_sqlite_vfs::params![id, group, body])?;
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
            let key = format!("g{index:08}");
            let value = format!("growth-{index:08}-stable-vfs");
            insert.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;

    let start = performance_counter(0);
    for index in 0..writes {
        Db::update(|connection| {
            let key = format!("g{:08}", index % rows);
            let value = format!("write-{index:08}");
            connection.execute(
                "UPDATE growth_bench SET value = ?1 WHERE key = ?2",
                ic_sqlite_vfs::params![value, key],
            )?;
            let changed =
                connection.query_scalar::<i64>("SELECT changes()", ic_sqlite_vfs::params![])?;
            if changed != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
            Ok(())
        })
        .map_err(error_text)?;
    }

    report(rows, start, u64::from(writes))
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

fn seed_bench_rows(rows: u32) -> Result<(), ic_sqlite_vfs::DbError> {
    Db::update(|connection| {
        reset_bench_table(connection)?;
        let mut statement = connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(ic_sqlite_vfs::params![key, value])?;
        }
        Ok(())
    })
}

fn placeholders(count: u32) -> Result<String, ic_sqlite_vfs::DbError> {
    let capacity = usize::try_from(count).map_err(|_| ic_sqlite_vfs::DbError::TooManyParameters)?;
    let mut out = String::with_capacity(capacity.saturating_mul(3));
    for index in 0..count {
        if index > 0 {
            out.push(',');
        }
        out.push('?');
        out.push_str(&(index + 1).to_string());
    }
    Ok(out)
}

fn bench_keys(rows: u32) -> Vec<String> {
    (0..rows).map(|index| format!("k{index:08}")).collect()
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

fn bench_key(index: u32, out: &mut [u8; 9]) -> &str {
    out[0] = b'k';
    let mut value = index;
    for byte in out[1..].iter_mut().rev() {
        *byte = b'0' + u8::try_from(value % 10).expect("digit fits u8");
        value /= 10;
    }
    // SAFETY: bytes are always ASCII `k` followed by decimal digits.
    unsafe { std::str::from_utf8_unchecked(out) }
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
