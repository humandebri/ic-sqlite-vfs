//! Integration test for SQLite FTS5 support through the crate VFS.
//!
//! The canister build links a precompiled SQLite archive, while host tests use
//! the bundled build path. This test catches missing FTS5 flags in that path.

use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::test_support::lock;
use ic_sqlite_vfs::test_support::memory;
use ic_sqlite_vfs::{params, Db};
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
#[serial]
fn fts5_virtual_table_supports_match_queries() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE VIRTUAL TABLE docs_fts USING fts5(title, body);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut insert = connection.prepare("INSERT INTO docs_fts(title, body) VALUES (?1, ?2)")?;
        insert.execute(params!["alpha", "stable memory backed sqlite"])?;
        insert.execute(params!["beta", "ordinary substring path search"])?;
        Ok(())
    })
    .unwrap();

    let titles = Db::query(|connection| {
        connection.query_column::<String>(
            "SELECT title FROM docs_fts WHERE docs_fts MATCH ?1 ORDER BY rank",
            params!["sqlite"],
        )
    })
    .unwrap();

    assert_eq!(titles, vec!["alpha".to_string()]);
}
