//! Public canister API for the product-facing SQLite facade.
//!
//! Methods are intentionally synchronous. They call `Db::update` or `Db::query`,
//! so a SQLite transaction cannot cross an `await` boundary.

use crate::db::migrate::Migration;
use crate::stable::meta::Superblock;
use crate::Db;
use candid::CandidType;
use serde::Deserialize;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];

#[derive(CandidType, Deserialize)]
pub struct DbMeta {
    pub db_size: u64,
    pub schema_version: u64,
    pub last_tx_id: u64,
    pub flags: u64,
    pub checksum: u64,
    pub importing: bool,
    pub import_written_until: u64,
}

#[ic_cdk::init]
fn init() {
    must(Db::migrate(MIGRATIONS));
}

#[ic_cdk::post_upgrade]
fn post_upgrade() {
    must(Db::migrate(MIGRATIONS));
}

#[ic_cdk::update]
fn kv_put(key: String, value: String) -> Result<(), String> {
    Db::update(|connection| {
        connection.execute_with_texts(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            &[key.as_str(), value.as_str()],
        )
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection.query_optional_string_with_text("SELECT value FROM kv WHERE key = ?1", &key)
    })
    .map_err(error_text)
}

#[ic_cdk::query]
fn kv_count() -> Result<u64, String> {
    let count = Db::query(|connection| connection.query_i64("SELECT COUNT(*) FROM kv"))
        .map_err(error_text)?;
    u64::try_from(count).map_err(|_| "negative row count".to_string())
}

#[ic_cdk::query]
fn db_meta() -> Result<DbMeta, String> {
    require_controller()?;
    let block = Superblock::load().map_err(|error| error.to_string())?;
    Ok(DbMeta {
        db_size: block.db_size,
        schema_version: block.schema_version,
        last_tx_id: block.last_tx_id,
        flags: block.flags,
        checksum: block.checksum,
        importing: block.is_importing(),
        import_written_until: block.import_written_until,
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
