//! Compile-time guard for the documented 2.0 Rust API surface.
//!
//! The snapshot gate catches broad public-item drift. This compile test keeps
//! the stable facade type-checking for downstream code.

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
use ic_sqlite_vfs::{
    named_params, params, Db, DbError, DbHandle, DbMemory, DefaultMemoryImpl, MemoryId,
    MemoryManager, MemoryManagerInitError, StableMemoryError,
};

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
    let _statement_size = std::mem::size_of::<Statement<'_>>();
    let _stable_error_size = std::mem::size_of::<StableMemoryError>();
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
