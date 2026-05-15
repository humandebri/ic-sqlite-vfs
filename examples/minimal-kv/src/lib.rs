//! Minimal KV canister backed by SQLite inside one MemoryManager virtual memory.

use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::{params, Db};
use ic_stable_structures::{
    memory_manager::{MemoryId, MemoryManager},
    DefaultMemoryImpl,
};
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

#[ic_cdk::init]
fn init() {
    init_db();
    Db::migrate(MIGRATIONS).unwrap();
}

#[ic_cdk::post_upgrade]
fn post_upgrade() {
    init_db();
    Db::migrate(MIGRATIONS).unwrap();
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
    .map_err(|error| error.to_string())
}

#[ic_cdk::query]
fn kv_get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection
            .query_optional_scalar::<String>("SELECT value FROM kv WHERE key = ?1", params![key])
    })
    .map_err(|error| error.to_string())
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        Db::init(manager.borrow().get(SQLITE_MEMORY_ID)).unwrap();
    });
}
