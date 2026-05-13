//! Local IC benchmark canister for the README KV workload.
//!
//! Write reports intentionally avoid `Db::db_checksum()` so the measured
//! instruction count is the SQLite commit path, not a full DB verification scan.

use candid::CandidType;
use ic_cdk::{api::performance_counter, init, post_upgrade, query, update};
use ic_sqlite_vfs::db::migrate::Migration;
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
            statement.execute_with_texts(&[key.as_str(), value.as_str()])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    Ok(report(rows, start, u64::from(rows)))
}

#[query]
fn bench_read(rows: u32) -> Result<BenchReport, String> {
    let start = performance_counter(0);
    let checksum = Db::query(|connection| {
        let mut total = 0_u64;
        let mut statement = connection.prepare("SELECT value FROM bench WHERE key = ?1")?;
        for index in 0..rows {
            let key = format!("k{index:08}");
            if let Some(value) = statement.query_optional_string_with_text(&key)? {
                total = total.wrapping_add(value.len() as u64);
            }
        }
        Ok(total)
    })
    .map_err(error_text)?;
    Ok(report(rows, start, checksum))
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
            statement.execute_with_texts(&[key.as_str(), value.as_str()])?;
        }
        Ok(())
    })
    .map_err(error_text)?;
    Ok(report(rows, start, u64::from(rows)))
}

fn report(rows: u32, start: u64, checksum: u64) -> BenchReport {
    BenchReport {
        rows: u64::from(rows),
        instructions: performance_counter(0) - start,
        checksum,
    }
}

fn must(result: Result<(), ic_sqlite_vfs::DbError>) {
    if let Err(error) = result {
        ic_cdk::trap(error.to_string());
    }
}

fn error_text(error: ic_sqlite_vfs::DbError) -> String {
    error.to_string()
}
