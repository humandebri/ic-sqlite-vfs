//! 0.2.2 compatibility canister used by PocketIC cross-version tests.
//!
//! It intentionally exposes the old schema version so current builds can prove
//! upgrade and import/export compatibility from an already deployed image.

use candid::CandidType;
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::stable::{memory, meta::Superblock};
use ic_sqlite_vfs::{params, Db, DefaultMemoryImpl, MemoryId, MemoryManager};
use serde::Deserialize;
use std::cell::RefCell;

const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];

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

#[ic_cdk::update]
fn kv_put(key: String, value: String) -> Result<(), String> {
    Db::update(|connection| {
        connection.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection
            .query_optional_scalar::<String>("SELECT value FROM kv WHERE key = ?1", params![key])
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_count() -> Result<u64, String> {
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM kv", params![])
    })
    .map_err(error_text)?;
    u64::try_from(count).map_err(|_| "negative row count".to_string())
}

#[ic_cdk::query]
fn db_meta() -> Result<DbMeta, String> {
    require_controller()?;
    let block = Superblock::load().map_err(|error| error.to_string())?;
    let stats =
        ic_sqlite_vfs::sqlite_vfs::stable_blob::storage_stats().map_err(|error| error.to_string())?;
    let stable_pages = memory::size_pages();
    Ok(DbMeta {
        db_size: block.db_size,
        stable_pages,
        stable_bytes: stable_pages
            .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
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

#[ic_cdk::update]
fn db_refresh_checksum() -> Result<u64, String> {
    require_controller()?;
    Db::refresh_checksum().map_err(error_text)
}

#[ic_cdk::query]
fn db_export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, String> {
    require_controller()?;
    Db::export_chunk(offset, len).map_err(error_text)
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        must(Db::init(manager.borrow().get(SQLITE_MEMORY_ID)));
    });
}

fn must(result: Result<(), ic_sqlite_vfs::DbError>) {
    if let Err(error) = result {
        ic_cdk::trap(error.to_string());
    }
}

fn error_text(error: ic_sqlite_vfs::DbError) -> String {
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
