use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::sqlite_vfs::{lock, stable_blob};
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::stable::meta::Superblock;
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
fn import_management_is_rejected_inside_update_transaction() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE import_guard(id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
    }])
    .unwrap();

    let result = Db::update(|connection| {
        connection.execute(
            "INSERT INTO import_guard(value) VALUES (?1)",
            params!["pending"],
        )?;
        Db::begin_import(0, ic_sqlite_vfs::stable::meta::fnv1a64(&[]))?;
        Ok(())
    });

    assert!(result.is_err());
    assert!(!Superblock::load().unwrap().is_importing());
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM import_guard", params![])
    })
    .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[serial]
fn cancel_import_restores_access_after_incomplete_import() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE import_cancel(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO import_cancel(k, v) VALUES ('key', 'value')")
    })
    .unwrap();

    Db::begin_import(8, 0).unwrap();
    Db::import_chunk(0, b"partial").unwrap();

    assert!(Superblock::load().unwrap().is_importing());
    assert!(Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM import_cancel WHERE k = 'key'", params![])
    })
    .is_err());
    assert!(Db::finish_import().is_err());
    assert!(Superblock::load().unwrap().is_importing());

    Db::cancel_import().unwrap();
    assert!(!Superblock::load().unwrap().is_importing());
    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM import_cancel WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(value, "value");
}

#[test]
#[serial]
fn import_state_machine_rejects_invalid_transitions() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE import_state(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::begin_import(8, 0).unwrap();
    assert!(Db::import_chunk(1, b"bad").is_err());
    assert_eq!(Superblock::load().unwrap().import_written_until, 0);

    Db::import_chunk(0, b"abcd").unwrap();
    let partial = Superblock::load().unwrap();
    assert!(partial.is_importing());
    assert_eq!(partial.import_written_until, 4);
    assert!(Db::finish_import().is_err());
    assert!(Superblock::load().unwrap().is_importing());

    let update = Db::update(|connection| {
        connection.execute_batch("INSERT INTO import_state(k, v) VALUES ('k', 'v')")
    });
    assert!(update.is_err());

    Db::cancel_import().unwrap();
    assert!(!Superblock::load().unwrap().is_importing());
}
