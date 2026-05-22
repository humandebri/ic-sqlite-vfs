//! Public canister API for the product-facing SQLite facade.
//!
//! Methods are intentionally synchronous. They call `Db::update` or `Db::query`,
//! so a SQLite transaction cannot cross an `await` boundary.

use crate::db::migrate::Migration;
use crate::db::value::{to_sql_ref, ToSql};
use crate::stable::memory;
use crate::stable::memory_manager::{MemoryId, MemoryManager};
use crate::stable::meta::Superblock;
use crate::stable::raw_memory::DefaultMemoryImpl;
use crate::Db;
use candid::CandidType;
use serde::Deserialize;
use std::cell::RefCell;

#[cfg(feature = "canister-api-test-failpoints")]
mod sqlite_feature_probe;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: "CREATE TABLE kv (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );",
    },
    Migration {
        version: 2,
        sql: "ALTER TABLE kv ADD COLUMN note TEXT;",
    },
];
const MAX_KV_GET_MANY_KEYS: usize = 1_000;
const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

#[derive(CandidType, Deserialize)]
pub struct DbMeta {
    pub db_size: u64,
    pub stable_pages: u64,
    pub stable_bytes: u64,
    pub schema_version: u64,
    pub last_tx_id: u64,
    pub flags: u64,
    pub checksum: u64,
    pub checksum_stale: bool,
    pub checksum_refreshing: bool,
    pub checksum_refresh_offset: u64,
    pub importing: bool,
    pub import_written_until: u64,
    pub layout_version: u64,
    pub page_count: u64,
    pub page_table_bytes: u64,
    pub active_bytes: u64,
    pub allocated_bytes: u64,
    pub orphan_bytes_estimate: u64,
    pub orphan_ratio_basis_points: u64,
    pub compact_recommended: bool,
}

#[derive(CandidType, Deserialize)]
pub struct ChecksumRefresh {
    pub complete: bool,
    pub checksum: u64,
    pub scanned_bytes: u64,
    pub db_size: u64,
}

#[ic_cdk::init]
fn init() {
    init_db();
    must(Db::migrate(MIGRATIONS));
}

#[ic_cdk::post_upgrade]
fn post_upgrade() {
    init_db();
    must(Db::migrate(MIGRATIONS));
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        must(Db::init(manager.borrow().get(SQLITE_MEMORY_ID)));
    });
}

#[ic_cdk::update]
fn kv_put(key: String, value: String) -> Result<(), String> {
    Db::update(|connection| {
        connection.execute_text_text(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            &key,
            &value,
        )
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection.query_optional_string_text("SELECT value FROM kv WHERE key = ?1", &key)
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_get_many(keys: Vec<String>) -> Result<Vec<Option<String>>, String> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    if keys.len() > MAX_KV_GET_MANY_KEYS {
        return Err(format!(
            "kv_get_many accepts at most {MAX_KV_GET_MANY_KEYS} keys"
        ));
    }
    let sql = values_lookup_sql(keys.len(), "value")?;
    Db::query(|connection| {
        let values = keys
            .iter()
            .map(|key| to_sql_ref(key) as &dyn ToSql)
            .collect::<Vec<_>>();
        connection.query_column::<Option<String>>(&sql, &values)
    })
    .map_err(error_text)
}

#[ic_cdk::update]
fn kv_set_note(key: String, note: String) -> Result<(), String> {
    Db::update(|connection| {
        connection.execute_text_text("UPDATE kv SET note = ?1 WHERE key = ?2", &note, &key)
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_get_note(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection.query_optional_string_text("SELECT note FROM kv WHERE key = ?1", &key)
    })
    .map_err(error_text)
}

fn values_lookup_sql(count: usize, column: &str) -> Result<String, String> {
    let mut sql = String::from("WITH lookup(ord, key) AS (VALUES ");
    for index in 0..count {
        if index > 0 {
            sql.push(',');
        }
        let ordinal = index + 1;
        sql.push_str(&format!("({index}, ?{ordinal})"));
    }
    sql.push_str(") SELECT kv.");
    sql.push_str(column);
    sql.push_str(" FROM lookup LEFT JOIN kv ON kv.key = lookup.key ORDER BY lookup.ord");
    Ok(sql)
}

#[ic_cdk::query]
fn kv_count() -> Result<u64, String> {
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM kv", crate::params![])
    })
    .map_err(error_text)?;
    u64::try_from(count).map_err(|_| "negative row count".to_string())
}

#[ic_cdk::query]
fn db_meta() -> Result<DbMeta, String> {
    require_controller()?;
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stats =
        crate::sqlite_vfs::stable_blob::storage_stats().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    Ok(DbMeta {
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(crate::config::STABLE_PAGE_SIZE)
            .ok_or_else(|| "stable byte size overflow".to_string())?,
        schema_version: block.schema_version,
        last_tx_id: block.last_tx_id,
        flags: block.flags,
        checksum: block.checksum,
        checksum_stale: block.is_checksum_stale(),
        checksum_refreshing: block.is_checksum_refreshing(),
        checksum_refresh_offset: block.checksum_refresh_offset,
        importing: block.is_importing(),
        import_written_until: block.import_written_until,
        layout_version: stats.layout_version,
        page_count: stats.page_count,
        page_table_bytes: stats.page_table_bytes,
        active_bytes: stats.active_bytes,
        allocated_bytes: stats.allocated_bytes,
        orphan_bytes_estimate: stats.orphan_bytes_estimate,
        orphan_ratio_basis_points: stats.orphan_ratio_basis_points,
        compact_recommended: stats.compact_recommended,
    })
}

#[ic_cdk::query]
fn db_integrity_check() -> Result<String, String> {
    require_controller()?;
    Db::integrity_check().map_err(error_text)
}

#[ic_cdk::query]
fn db_checksum() -> Result<u64, String> {
    require_controller()?;
    Db::db_checksum().map_err(error_text)
}

#[ic_cdk::update]
fn db_refresh_checksum() -> Result<u64, String> {
    require_controller()?;
    Db::refresh_checksum().map_err(error_text)
}

#[ic_cdk::update]
fn db_refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, String> {
    require_controller()?;
    let report = Db::refresh_checksum_chunk(max_bytes).map_err(error_text)?;
    Ok(ChecksumRefresh {
        complete: report.complete,
        checksum: report.checksum,
        scanned_bytes: report.scanned_bytes,
        db_size: report.db_size,
    })
}

#[ic_cdk::query]
fn db_export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, String> {
    require_controller()?;
    Db::export_chunk(offset, len).map_err(error_text)
}

#[ic_cdk::update]
fn db_begin_import(total_size: u64, expected_checksum: u64) -> Result<(), String> {
    require_controller()?;
    Db::begin_import(total_size, expected_checksum).map_err(error_text)
}

#[ic_cdk::update]
fn db_import_chunk(offset: u64, bytes: Vec<u8>) -> Result<(), String> {
    require_controller()?;
    Db::import_chunk(offset, &bytes).map_err(error_text)
}

#[ic_cdk::update]
fn db_finish_import() -> Result<(), String> {
    require_controller()?;
    Db::finish_import().map_err(error_text)
}

#[ic_cdk::update]
fn db_cancel_import() -> Result<(), String> {
    require_controller()?;
    Db::cancel_import().map_err(error_text)
}

#[ic_cdk::update]
fn db_compact() -> Result<(), String> {
    require_controller()?;
    Db::compact().map_err(error_text)
}

#[cfg(feature = "canister-api-test-failpoints")]
#[ic_cdk::update]
fn db_test_trap_after_stable_write(ordinal: u64) -> Result<(), String> {
    require_controller()?;
    crate::stable::memory::set_failpoint(crate::stable::memory::MemoryFailpoint::TrapAfterWrite {
        ordinal,
    });
    Ok(())
}

#[cfg(feature = "canister-api-test-failpoints")]
#[ic_cdk::update]
fn db_test_sqlite_feature_probe() -> Result<(), String> {
    require_controller()?;
    sqlite_feature_probe::run()
}

#[cfg(feature = "canister-api-test-failpoints")]
#[ic_cdk::update]
fn db_test_clear_failpoints() -> Result<(), String> {
    require_controller()?;
    crate::stable::memory::clear_failpoint();
    crate::db::statement::clear_step_failpoint();
    crate::sqlite_vfs::stable_blob::rollback_update();
    Ok(())
}

fn must(result: Result<(), crate::DbError>) {
    if let Err(error) = result {
        ic_cdk::trap(error.to_string());
    }
}

fn error_text(error: crate::DbError) -> String {
    error.to_string()
}

fn require_controller() -> Result<(), String> {
    let caller = ic_cdk::api::msg_caller();
    if ic_cdk::api::is_controller(&caller) {
        Ok(())
    } else {
        Err("caller is not a controller".to_string())
    }
}
