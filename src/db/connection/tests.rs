
use super::open_read_write;
use crate::config::{SQLITE_URI, SQLITE_URI_NUL, STATEMENT_CACHE_CAPACITY, VFS_NAME, VFS_NAME_NUL};
use crate::sqlite_vfs::{lock, stable_blob};
use crate::stable::memory;
use crate::Db;
use serial_test::serial;
use std::ffi::CStr;

fn reset() {
    stable_blob::rollback_update();
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
fn sqlite_open_strings_are_static_nul_terminated() {
    let uri = CStr::from_bytes_with_nul(SQLITE_URI_NUL).unwrap();
    let vfs = CStr::from_bytes_with_nul(VFS_NAME_NUL).unwrap();
    assert_eq!(uri.to_str().unwrap(), SQLITE_URI);
    assert_eq!(vfs.to_str().unwrap(), VFS_NAME);
}

#[test]
#[serial]
fn cached_statements_are_lru_bounded() {
    reset();
    let connection = open_read_write().unwrap();

    for index in 0..(STATEMENT_CACHE_CAPACITY + 8) {
        let sql = format!("SELECT {index}");
        let mut statement = connection.prepare_cached(&sql).unwrap();
        let value = statement.query_scalar::<i64>(crate::params![]).unwrap();
        assert_eq!(value, i64::try_from(index).unwrap());
    }

    let cache = connection.cached.borrow();
    assert_eq!(cache.statements.len(), STATEMENT_CACHE_CAPACITY);
    assert!(!cache.statements.iter().any(|entry| entry.sql == "SELECT 0"));
    assert!(cache
        .statements
        .iter()
        .any(|entry| entry.sql == format!("SELECT {}", STATEMENT_CACHE_CAPACITY + 7)));
}

#[test]
#[serial]
fn discarded_cached_statement_is_finalized_not_cached() {
    reset();
    let connection = open_read_write().unwrap();

    let statement = connection.prepare_cached("SELECT 1").unwrap();
    statement.discard();

    assert_eq!(connection.cached.borrow().statements.len(), 0);
}

#[test]
#[serial]
fn cached_statement_reuses_sql_after_constraint_error() {
    reset();
    let connection = open_read_write().unwrap();
    connection
        .execute_batch("CREATE TABLE cached_error(k TEXT PRIMARY KEY, v TEXT NOT NULL)")
        .unwrap();

    {
        let mut statement = connection
            .prepare_cached("INSERT INTO cached_error(k, v) VALUES (?1, ?2)")
            .unwrap();
        statement.execute(crate::params!["a", "one"]).unwrap();
    }
    {
        let mut statement = connection
            .prepare_cached("INSERT INTO cached_error(k, v) VALUES (?1, ?2)")
            .unwrap();
        let duplicate = statement.execute(crate::params!["a", "duplicate"]);
        assert!(matches!(duplicate, Err(crate::db::DbError::Constraint(_))));
    }
    {
        let mut statement = connection
            .prepare_cached("INSERT INTO cached_error(k, v) VALUES (?1, ?2)")
            .unwrap();
        statement.execute(crate::params!["b", "two"]).unwrap();
    }

    let values = connection
        .query_column::<String>("SELECT v FROM cached_error ORDER BY k", crate::params![])
        .unwrap();
    assert_eq!(values, vec!["one".to_string(), "two".to_string()]);
}

#[test]
#[serial]
fn regular_statements_are_finalized_before_connection_close() {
    reset();
    let connection = open_read_write().unwrap();

    {
        let _statement = connection.prepare("SELECT 1").unwrap();
        assert_eq!(open_statement_count(&connection), 1);
    }
    assert_eq!(open_statement_count(&connection), 0);

    for _ in 0..512 {
        let value = connection
            .query_one("SELECT 42", crate::params![], |row| row.get::<i64>(0))
            .unwrap();
        assert_eq!(value, 42);
    }
    assert_eq!(open_statement_count(&connection), 0);
}

#[test]
#[serial]
fn cached_and_regular_statement_lifetimes_do_not_double_finalize() {
    reset();
    let connection = open_read_write().unwrap();

    {
        let mut cached = connection.prepare_cached("SELECT ?1").unwrap();
        let value = cached.query_scalar::<i64>(crate::params![7_i64]).unwrap();
        assert_eq!(value, 7);
    }
    assert_eq!(open_statement_count(&connection), 1);

    {
        let _regular = connection.prepare("SELECT 8").unwrap();
        assert_eq!(open_statement_count(&connection), 2);
    }
    assert_eq!(open_statement_count(&connection), 1);

    unsafe {
        connection.cached.borrow_mut().finalize_all();
    }
    assert_eq!(open_statement_count(&connection), 0);
}

#[test]
#[serial]
fn prepare_error_paths_do_not_leave_statements_open() {
    reset();
    let connection = open_read_write().unwrap();

    assert!(connection.prepare("").is_err());
    assert_eq!(open_statement_count(&connection), 0);

    assert!(connection.prepare("SELECT 1; SELECT 2").is_err());
    assert_eq!(open_statement_count(&connection), 0);

    assert!(connection.prepare("SELECT * FROM missing_table").is_err());
    assert_eq!(open_statement_count(&connection), 0);
}

fn open_statement_count(connection: &super::Connection) -> usize {
    let mut count = 0;
    let mut statement = std::ptr::null_mut();
    loop {
        statement =
            unsafe { crate::sqlite_vfs::ffi::sqlite3_next_stmt(connection.raw(), statement) };
        if statement.is_null() {
            return count;
        }
        count += 1;
    }
}
