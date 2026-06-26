//! PocketIC benchmark canister for the wasi2ic + ic-rusqlite KV workload.
//!
//! The schema and measured methods mirror `benchmarks/kv-canister` so README
//! comparisons use the same SQL shape.

use candid::CandidType;
use ic_cdk::{api::performance_counter, init, post_upgrade, query, update};
use ic_rusqlite::{params, params_from_iter, with_connection, OptionalExtension};
use serde::Deserialize;

mod key;

use key::{bench_key, validate_fixed_bench_key_rows};

const STABLE_PAGE_SIZE: u64 = 65_536;
const MIGRATION_SQL: &str = "CREATE TABLE IF NOT EXISTS bench (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) WITHOUT ROWID";
const POINT_READ_SQL: &str = "SELECT value FROM bench WHERE key = ?1";

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

#[init]
fn init() {
    must(migrate());
}

#[post_upgrade]
fn post_upgrade() {
    must(migrate());
}

#[update]
fn bench_reset(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        reset_bench_table(&connection)?;
        let mut statement =
            connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[update]
fn bench_insert_only(rows: u32) -> Result<BenchReport, String> {
    with_connection(|connection| reset_bench_table(&connection)).map_err(error_text)?;
    let start = performance_counter(0);
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement =
            connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
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
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement =
            connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..append_rows {
            let row = base_rows
                .checked_add(index)
                .ok_or_else(|| ic_rusqlite::Error::ExecuteReturnedResults)?;
            let key = format!("k{row:08}");
            let value = format!("value-{row:08}-stable-vfs");
            statement.execute(params![key, value])?;
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
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement = connection.prepare("UPDATE bench SET value = ?1 WHERE key = ?2")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("updated-{index:08}-stable-vfs");
            statement.execute(params![value, key])?;
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
    let checksum = with_connection(|connection| -> Result<u64, ic_rusqlite::Error> {
        let mut total = 0_u64;
        let mut statement = connection.prepare_cached(POINT_READ_SQL)?;
        for index in 0..rows {
            let mut key = [0_u8; 9];
            let key = bench_key(index, &mut key);
            let value = statement
                .query_row(params![key], |row| {
                    let value = row.get_ref(0)?;
                    Ok(value.as_str()?.len())
                })
                .optional()?;
            if let Some(len) = value {
                total = total.wrapping_add(len as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn bench_many_rows(rows: u32) -> Result<BenchReport, String> {
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = with_connection(|connection| -> Result<u64, ic_rusqlite::Error> {
        let mut statement =
            connection.prepare("SELECT value FROM bench ORDER BY key LIMIT ?1")?;
        let values = statement.query_map(params![i64::from(rows)], |row| {
            let value = row.get_ref(0)?;
            Ok(value.as_str()?.len())
        })?;
        let mut total = 0_u64;
        for len in values {
            total = total.wrapping_add(len? as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[update]
fn bench_write(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement = connection.prepare(
            "INSERT INTO bench(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?;
        for index in 0..rows {
            let key = format!("w{index:08}");
            let value = format!("updated-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    report(rows, start, u64::from(rows))
}

#[query]
fn bench_get_many_in(rows: u32) -> Result<BenchReport, String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    warm_read_connection()?;
    let start = performance_counter(0);
    let checksum = with_connection(|connection| -> Result<u64, ic_rusqlite::Error> {
        let sql = format!(
            "SELECT value FROM bench WHERE key IN ({}) ORDER BY key",
            placeholders(rows)
        );
        let keys = bench_keys(rows);
        let mut statement = connection.prepare(&sql)?;
        let values = statement.query_map(params_from_iter(keys.iter()), |row| {
            let value = row.get_ref(0)?;
            Ok(value.as_str()?.len())
        })?;
        let mut total = 0_u64;
        for len in values {
            total = total.wrapping_add(len? as u64);
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
}

#[query]
fn db_stats() -> Result<DbStatsReport, String> {
    let db_size = db_file_size()?;
    let stable_pages = ic_cdk::api::stable_size();
    let stable_bytes = stable_pages
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = sqlite_stats()?;
    Ok(DbStatsReport {
        db_size,
        stable_pages,
        stable_bytes,
        sqlite_page_size,
        sqlite_page_count,
        sqlite_freelist_count,
    })
}

#[update]
fn bench_churn_reset(base_rows: u32) -> Result<BenchChurnStepReport, String> {
    let start = performance_counter(0);
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        reset_churn_table(&connection)?;
        let mut statement =
            connection.prepare("INSERT INTO churn_bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..base_rows {
            let key = format!("c{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
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
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement = connection.prepare("DELETE FROM churn_bench WHERE key = ?1")?;
        for offset in 0..rows {
            let index = start_index
                .checked_add(offset)
                .ok_or_else(|| ic_rusqlite::Error::ExecuteReturnedResults)?;
            let key = format!("c{index:08}");
            let changed = statement.execute(params![key])?;
            if changed != 1 {
                return Err(ic_rusqlite::Error::ExecuteReturnedResults);
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
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let mut statement =
            connection.prepare("INSERT INTO churn_bench(key, value) VALUES (?1, ?2)")?;
        for offset in 0..rows {
            let index = start_index
                .checked_add(offset)
                .ok_or_else(|| ic_rusqlite::Error::ExecuteReturnedResults)?;
            let key = format!("c{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    churn_report(cycle, "insert", rows, start)
}

fn migrate() -> Result<(), ic_rusqlite::Error> {
    with_connection(|connection| connection.execute_batch(MIGRATION_SQL))
}

fn reset_bench_table(connection: &ic_rusqlite::Connection) -> Result<(), ic_rusqlite::Error> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS bench;
         CREATE TABLE bench (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
         ) WITHOUT ROWID;",
    )
}

fn reset_churn_table(connection: &ic_rusqlite::Connection) -> Result<(), ic_rusqlite::Error> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS churn_bench;
         CREATE TABLE churn_bench (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
         ) WITHOUT ROWID;",
    )
}

fn seed_bench_rows(rows: u32) -> Result<(), ic_rusqlite::Error> {
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        reset_bench_table(&connection)?;
        let mut statement =
            connection.prepare("INSERT INTO bench(key, value) VALUES (?1, ?2)")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}-stable-vfs");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
}

fn validate_churn_range(start: u32, rows: u32) -> Result<(), String> {
    if rows == 0 {
        return Err("rows must be positive".to_string());
    }
    start
        .checked_add(rows)
        .map(|_| ())
        .ok_or_else(|| "benchmark key index overflow".to_string())
}

fn sqlite_stats() -> Result<(u64, u64, u64), String> {
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) =
        with_connection(|connection| -> Result<(i64, i64, i64), ic_rusqlite::Error> {
            Ok((
                connection.query_row("PRAGMA page_size", [], |row| row.get(0))?,
                connection.query_row("PRAGMA page_count", [], |row| row.get(0))?,
                connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?,
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
    let db_size = db_file_size()?;
    let stable_pages = ic_cdk::api::stable_size();
    let stable_bytes = stable_pages
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    let (sqlite_page_size, sqlite_page_count, sqlite_freelist_count) = sqlite_stats()?;
    let row_count = with_connection(|connection| -> Result<i64, ic_rusqlite::Error> {
        connection.query_row("SELECT COUNT(*) FROM churn_bench", [], |row| row.get(0))
    })
    .map_err(error_text)?;
    Ok(BenchChurnStepReport {
        cycle: u64::from(cycle),
        phase: phase.to_string(),
        rows: u64::from(rows),
        instructions: performance_counter(0).saturating_sub(start),
        row_count: u64::try_from(row_count).map_err(|_| "negative row count".to_string())?,
        db_size,
        stable_pages,
        stable_bytes,
        sqlite_page_size,
        sqlite_page_count,
        sqlite_freelist_count,
    })
}

fn placeholders(count: u32) -> String {
    let capacity = usize::try_from(count).unwrap_or(usize::MAX / 3);
    let mut out = String::with_capacity(capacity.saturating_mul(3));
    for index in 0..count {
        if index > 0 {
            out.push(',');
        }
        out.push('?');
        out.push_str(&(index + 1).to_string());
    }
    out
}

fn bench_keys(rows: u32) -> Vec<String> {
    (0..rows).map(|index| format!("k{index:08}")).collect()
}

fn report(rows: u32, start: u64, checksum: u64) -> Result<BenchReport, String> {
    let instructions = performance_counter(0).saturating_sub(start);
    let db_size = db_file_size()?;
    let stable_pages = ic_cdk::api::stable_size();
    let stable_bytes = stable_pages
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or_else(|| "stable byte size overflow".to_string())?;
    Ok(BenchReport {
        rows: u64::from(rows),
        instructions,
        checksum,
        db_size,
        stable_pages,
        stable_bytes,
    })
}

fn db_file_size() -> Result<u64, String> {
    std::fs::metadata("./DB/main.db")
        .map(|metadata| metadata.len())
        .map_err(error_text)
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn warm_point_read_statement() -> Result<(), String> {
    with_connection(|connection| -> Result<(), ic_rusqlite::Error> {
        let _statement = connection.prepare_cached(POINT_READ_SQL)?;
        Ok(())
    })
    .map_err(error_text)
}

fn warm_read_connection() -> Result<(), String> {
    with_connection(|_| -> Result<(), ic_rusqlite::Error> { Ok(()) }).map_err(error_text)
}

fn must<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|error| ic_cdk::trap(format!("database init failed: {error}")))
}
