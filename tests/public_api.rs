//! Compile-time guard for the documented 1.0 Rust API surface.
//!
//! The snapshot gate catches broad public-item drift. This compile test keeps
//! the stable paths type-checking for downstream code that imports modules
//! directly rather than only using the top-level facade.

#[cfg(feature = "canister-api")]
use ic_sqlite_vfs::api::{ChecksumRefresh as ApiChecksumRefresh, DbMeta};
use ic_sqlite_vfs::config;
use ic_sqlite_vfs::db::connection::Connection;
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::row::{FromColumn, TextLen};
use ic_sqlite_vfs::db::statement::Statement;
#[cfg(feature = "bench-profile")]
use ic_sqlite_vfs::db::statement::{
    ExecuteTextTextProfile, QueryOptionalStringTextProfile, QueryTextLenSumProfile,
};
use ic_sqlite_vfs::db::transaction::UpdateConnection;
use ic_sqlite_vfs::db::value::{to_sql_ref, Null, ToSql, Value, NULL};
use ic_sqlite_vfs::read_metrics::{read_metrics_snapshot, ReadMetrics};
use ic_sqlite_vfs::sqlite_vfs::file::{FileKind, IcStableFile};
use ic_sqlite_vfs::sqlite_vfs::{ffi, register, stable_blob, temp::TempFile};
use ic_sqlite_vfs::stable::memory::{self, ContextId, StableMemoryError};
use ic_sqlite_vfs::stable::meta::{self, Superblock};
use ic_sqlite_vfs::stable::raw_memory::{Memory, VectorMemory};
use ic_sqlite_vfs::{
    named_params, params, Db, DbError, DbHandle, DbMemory, DefaultMemoryImpl, MemoryId,
    MemoryManager, MemoryManagerInitError,
};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn documented_public_api_surface_compiles() {
    const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);
    const MIGRATIONS: &[Migration] = &[Migration {
        version: 1,
        sql: "CREATE TABLE kv(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);",
    }];

    let _init: fn(DbMemory) -> Result<(), DbError> = Db::init;
    let _handle_init: fn(DbMemory) -> Result<DbHandle, DbError> = DbHandle::init;
    let _manager: MemoryManager<DefaultMemoryImpl> =
        MemoryManager::init(DefaultMemoryImpl::default());
    let _first_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(121));
    let _second_memory = MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(122));
    let _strict_init: fn(
        DefaultMemoryImpl,
    ) -> Result<MemoryManager<DefaultMemoryImpl>, MemoryManagerInitError> =
        MemoryManager::init_strict;
    let _memory_id = SQLITE_MEMORY_ID;
    let _migrations = MIGRATIONS;

    let blob = vec![0_u8, 1, 2];
    let value_blob = [3_u8, 4, 5];
    let _params = params![
        "alpha",
        String::from("owned"),
        42_i64,
        3.5_f64,
        blob,
        &value_blob[..],
        NULL,
        Value::Text("text"),
        Value::Integer(7),
        Value::Real(1.25),
        Value::Blob(&value_blob),
        Value::Null,
    ];
    let _named = named_params![":key" => "alpha", ":value" => 1_i64];
    let _null = Null;
    let _to_sql: &dyn ToSql = to_sql_ref(&"alpha");

    assert_to_sql::<&str>();
    assert_to_sql::<String>();
    assert_to_sql::<i64>();
    assert_to_sql::<f64>();
    assert_to_sql::<Vec<u8>>();
    assert_to_sql::<&[u8]>();
    assert_to_sql::<Value<'_>>();

    assert_from_column::<String>();
    assert_from_column::<i64>();
    assert_from_column::<f64>();
    assert_from_column::<Vec<u8>>();
    assert_from_column::<Option<String>>();
    assert_from_column::<TextLen>();

    let _query_closure = accepts_query(|connection: &Connection| {
        let _ = connection.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)",
            params!["a", "b"],
        );
        let _ = connection.execute_named(
            "INSERT INTO kv(key, value) VALUES (:key, :value)",
            named_params![":key" => "a", ":value" => "b"],
        );
        let _ = connection
            .query_optional_scalar::<String>("SELECT value FROM kv WHERE key = ?1", params!["a"]);
        let _ = connection.query_one("SELECT value FROM kv WHERE key = ?1", params!["a"], |row| {
            row.get::<String>(0)
        });
        Ok(())
    });
    let _update_closure = accepts_update(|connection| {
        connection.savepoint(|inner| {
            inner.execute(
                "INSERT INTO kv(key, value) VALUES (?1, ?2)",
                params!["a", "b"],
            )
        })
    });

    assert_public_support_types();
    assert_low_level_public_modules();
}

fn assert_to_sql<T: ToSql>() {}

fn assert_from_column<T: FromColumn>() {}

fn accepts_query<T, F>(f: F) -> F
where
    F: FnOnce(&Connection) -> Result<T, DbError>,
{
    f
}

fn accepts_update<T, F>(f: F) -> F
where
    F: FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>,
{
    f
}

fn assert_public_support_types() {
    #[cfg(feature = "canister-api")]
    {
        let _meta_size = std::mem::size_of::<DbMeta>();
        let _api_refresh_size = std::mem::size_of::<ApiChecksumRefresh>();
    }
    let _read_metrics_size = std::mem::size_of::<ReadMetrics>();
    let _statement_size = std::mem::size_of::<Statement<'_>>();
    #[cfg(feature = "bench-profile")]
    {
        let _query_profile = QueryOptionalStringTextProfile {
            reset_bind: 0,
            step: 0,
            column_read: 0,
        };
        let _execute_profile = ExecuteTextTextProfile {
            reset_bind: 0,
            step: 0,
        };
        let _scan_profile = QueryTextLenSumProfile {
            reset_bind: 0,
            row_scan: 0,
        };
    }
    let _snapshot: fn() -> ReadMetrics = read_metrics_snapshot;
    let _config_values = (
        config::SQLITE_PAGE_SIZE,
        config::SQLITE_CACHE_SIZE_KIB,
        config::STATEMENT_CACHE_CAPACITY,
        config::STABLE_PAGE_SIZE,
        config::SUPERBLOCK_OFFSET,
        config::SUPERBLOCK_SIZE,
        config::DB_REGION_OFFSET,
        config::MAIN_DB_PATH,
        config::VFS_NAME,
        config::SQLITE_URI,
        config::VFS_NAME_NUL,
        config::SQLITE_URI_NUL,
    );
}

fn assert_low_level_public_modules() {
    let _register: fn() -> std::ffi::c_int = register;
    let _vfs_prepare: unsafe fn() -> *mut ffi::sqlite3_vfs =
        ic_sqlite_vfs::sqlite_vfs::vfs::prepare;
    let _io_methods = &ic_sqlite_vfs::sqlite_vfs::file::IO_METHODS;
    let _file_kind = FileKind::Main;
    let _file_size = std::mem::size_of::<IcStableFile>();
    let _temp = TempFile::default();

    let _checksum_refresh_size = std::mem::size_of::<stable_blob::ChecksumRefresh>();
    let _storage_stats_size = std::mem::size_of::<stable_blob::StorageStats>();
    let _export: fn(u64, u64) -> Result<Vec<u8>, StableMemoryError> = stable_blob::export_chunk;
    let _begin_import: fn(u64, u64) -> Result<(), StableMemoryError> = stable_blob::begin_import;
    let _import_chunk: fn(u64, &[u8]) -> Result<(), StableMemoryError> = stable_blob::import_chunk;
    let _finish_import: fn() -> Result<(), StableMemoryError> = stable_blob::finish_import;
    let _cancel_import: fn() -> Result<(), StableMemoryError> = stable_blob::cancel_import;

    let _context_size = std::mem::size_of::<ContextId>();
    let _memory_error_size = std::mem::size_of::<StableMemoryError>();
    let _size_pages: fn() -> u64 = memory::size_pages;
    let _active_context: fn() -> Result<ContextId, StableMemoryError> = memory::active_context_id;

    let _superblock = Superblock::fresh();
    let _page_layout = meta::PAGE_MAP_LAYOUT_VERSION;
    let _fnv: fn(&[u8]) -> u64 = meta::fnv1a64;

    let vector: VectorMemory = Rc::new(RefCell::new(Vec::new()));
    assert_memory_trait::<VectorMemory>();
    let _memory_size = vector.size();
}

fn assert_memory_trait<T: Memory>() {}
