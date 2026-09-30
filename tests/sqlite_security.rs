//! Regression coverage for the patched engine reached through this VFS.
use ic_sqlite_vfs::test_support::{ffi, lock, memory};
use ic_sqlite_vfs::{params, Db, DbError};
use serial_test::serial;

#[test]
#[serial]
fn patched_engine_rejects_invalid_fts_leaf_sizes_and_recovers() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
    Db::update(|c| {
        assert_eq!(c.query_scalar::<String>("SELECT sqlite_version()", params![])?, "3.53.4");
        assert_eq!(c.query_scalar::<String>("SELECT sqlite_source_id()", params![])?, ffi::SQLITE_SOURCE_ID.to_str().unwrap());
        c.execute_batch("CREATE VIRTUAL TABLE temp.corrupt_probe USING fts5(body); INSERT INTO temp.corrupt_probe VALUES('alpha beta')")?;
        for block in [vec![0u8,0,0,0], vec![0,0,0,3], vec![0,0]] {
            c.execute_batch("SAVEPOINT malformed_leaf")?;
            c.execute("UPDATE temp.corrupt_probe_data SET block=?1 WHERE id>10", params![block])?;
            let result = c.query_scalar::<i64>("SELECT count(*) FROM temp.corrupt_probe WHERE corrupt_probe MATCH 'alpha'", params![]);
            assert!(matches!(result, Err(DbError::Sqlite(code, _)) if code & 0xff == ffi::SQLITE_CORRUPT));
            c.execute_batch("ROLLBACK TO malformed_leaf; RELEASE malformed_leaf")?;
            assert_eq!(c.query_scalar::<i64>("SELECT count(*) FROM temp.corrupt_probe WHERE corrupt_probe MATCH 'alpha'", params![])?, 1);
        }
        Ok(())
    }).unwrap();
}
