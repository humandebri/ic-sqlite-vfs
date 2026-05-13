//! Local IC benchmark canister for the README KV workload.
//!
//! Write reports intentionally avoid `Db::db_checksum()` so the measured
//! instruction count is the SQLite commit path, not a full DB verification scan.

use candid::CandidType;
use ic_cdk::{api::performance_counter, init, post_upgrade, query, update};
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::stable::{memory, meta::Superblock};
use ic_sqlite_vfs::Db;
use serde::Deserialize;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS bench (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];

#[derive(CandidType, Deserialize)]
pub struct BenchReport {
    pub rows: u64,
    pub instructions: u64,
    pub checksum: u64,
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
}

#[init]
fn init() {
    must(Db::migrate(MIGRATIONS));
}

#[post_upgrade]
fn post_upgrade() {
    must(Db::migrate(MIGRATIONS));
}

#[update]
fn bench_reset(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    Db::update(|connection| {
        connection.execute_batch(
            "DROP TABLE IF EXISTS bench;
             CREATE TABLE bench (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );",
        )?;
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

#[query]
fn bench_read(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut total = 0_u64;
        let mut statement = connection.prepare("SELECT value FROM bench WHERE key = ?1")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            if let Some(value) =
                statement.query_optional_scalar::<String>(ic_sqlite_vfs::params![key])?
            {
                total = total.wrapping_add(value.len() as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    report(rows, start, checksum)
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
    report(bytes, start, u64::try_from(checksum).map_err(|_| "negative blob length")?)
}

#[query]
fn bench_many_rows(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let values = connection.query_column::<String>(
            "SELECT value FROM bench ORDER BY key LIMIT ?1",
            ic_sqlite_vfs::params![i64::from(rows)],
        )?;
        Ok(values.iter().map(|value| value.len() as u64).sum::<u64>())
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
        let mut left = connection
            .prepare("INSERT INTO join_left(id, group_id, body) VALUES (?1, ?2, ?3)")?;
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
    report(rows, start, u64::try_from(checksum).map_err(|_| "negative join count")?)
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
            let changed = connection.query_scalar::<i64>(
                "SELECT changes()",
                ic_sqlite_vfs::params![],
            )?;
            if changed != 1 {
                return Err(ic_sqlite_vfs::DbError::NotFound);
            }
            Ok(())
        })
        .map_err(error_text)?;
    }

    report(rows, start, u64::from(writes))
}

fn report(rows: u32, start: u64, checksum: u64) -> Result<BenchReport, String> {
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    Ok(BenchReport {
        rows: u64::from(rows),
        instructions: performance_counter(0) - start,
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

fn error_text(error: ic_sqlite_vfs::DbError) -> String {
    error.to_string()
}
