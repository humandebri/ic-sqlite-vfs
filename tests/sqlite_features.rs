//! Integration tests for enabled SQLite SQL feature families.
//!
//! These cover compile-time options that can silently disappear from either the
//! bundled build or the vendored precompiled archive.

use ic_sqlite_vfs::sqlite_vfs::{lock, stable_blob};
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::{params, Db};
use serial_test::serial;

fn reset() {
    stable_blob::invalidate_read_cache();
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
#[serial]
fn utc_date_time_functions_are_available() {
    reset();

    let date = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT date('2026-05-15 12:34:56')", params![])
    })
    .unwrap();
    assert_eq!(date, "2026-05-15");

    let time = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT time('2026-05-15 12:34:56')", params![])
    })
    .unwrap();
    assert_eq!(time, "12:34:56");

    let unix_epoch = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT datetime(0, 'unixepoch')", params![])
    })
    .unwrap();
    assert_eq!(unix_epoch, "1970-01-01 00:00:00");

    let year_month = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT strftime('%Y-%m', '2026-05-15')", params![])
    })
    .unwrap();
    assert_eq!(year_month, "2026-05");

    let now = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT datetime('now')", params![])
    })
    .unwrap();
    assert_sqlite_datetime_shape(&now);
}

#[test]
#[serial]
fn json_functions_and_table_valued_functions_are_available() {
    reset();

    let extracted = Db::query(|connection| {
        connection.query_scalar::<i64>(
            "SELECT json_extract('{\"a\":{\"b\":2}}', '$.a.b')",
            params![],
        )
    })
    .unwrap();
    assert_eq!(extracted, 2);

    let valid = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT json_valid('{\"ok\":true}')", params![])
    })
    .unwrap();
    assert_eq!(valid, 1);

    let values = Db::query(|connection| {
        connection.query_column::<i64>(
            "SELECT value FROM json_each('[10,20]') ORDER BY key",
            params![],
        )
    })
    .unwrap();
    assert_eq!(values, vec![10, 20]);

    let jsonb_value = Db::query(|connection| {
        connection.query_scalar::<String>(
            "SELECT jsonb_extract(jsonb('{\"k\":\"v\"}'), '$.k')",
            params![],
        )
    })
    .unwrap();
    assert_eq!(jsonb_value, "v");
}

fn assert_sqlite_datetime_shape(value: &str) {
    let bytes = value.as_bytes();
    assert_eq!(bytes.len(), 19);
    assert_eq!(bytes[4], b'-');
    assert_eq!(bytes[7], b'-');
    assert_eq!(bytes[10], b' ');
    assert_eq!(bytes[13], b':');
    assert_eq!(bytes[16], b':');
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        assert!(bytes[index].is_ascii_digit());
    }
}
