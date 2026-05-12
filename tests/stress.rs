use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::sqlite_vfs::lock;
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::stable::meta::Superblock;
use ic_sqlite_vfs::Db;
use serial_test::serial;
use std::collections::BTreeMap;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
}

#[test]
#[serial]
fn failed_migration_does_not_advance_schema_version() {
    reset();
    let result = Db::migrate(&[
        Migration {
            version: 1,
            sql: "CREATE TABLE ok_table(id INTEGER PRIMARY KEY);",
        },
        Migration {
            version: 2,
            sql: "CREATE TABLE broken(",
        },
    ]);

    assert!(result.is_err());
    assert_eq!(Superblock::load().unwrap().schema_version, 0);
}

#[test]
#[serial]
fn deterministic_fuzz_matches_model() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE fuzz(k INTEGER PRIMARY KEY, v INTEGER NOT NULL);",
    }])
    .unwrap();

    let mut model = BTreeMap::new();
    let mut state = 7_u64;
    for _ in 0..250 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = state % 32;
        let value = state % 10_000;
        if state & 1 == 0 {
            model.insert(key, value);
            Db::update(|connection| {
                connection.execute_batch(&format!(
                    "INSERT INTO fuzz(k, v) VALUES ({key}, {value})
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v"
                ))
            })
            .unwrap();
        } else {
            model.remove(&key);
            Db::update(|connection| {
                connection.execute_batch(&format!("DELETE FROM fuzz WHERE k = {key}"))
            })
            .unwrap();
        }
    }

    let sum = Db::query(|connection| connection.query_i64("SELECT COALESCE(SUM(v), 0) FROM fuzz"))
        .unwrap();
    let expected = model.values().sum::<u64>();
    assert_eq!(u64::try_from(sum).unwrap(), expected);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

#[test]
#[serial]
fn capacity_and_import_bounds_are_rejected() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cap(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let db_size = Superblock::load().unwrap().db_size;
    let checksum = Db::db_checksum().unwrap();
    Db::begin_import(db_size, checksum).unwrap();
    let result = Db::import_chunk(db_size + 1, &[1, 2, 3]);

    assert!(result.is_err());
}

#[test]
#[serial]
fn long_endurance_many_transactions_keeps_integrity() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE endurance(id INTEGER PRIMARY KEY, v INTEGER NOT NULL);",
    }])
    .unwrap();

    for id in 0..1_000_u64 {
        Db::update(|connection| {
            connection.execute_batch(&format!("INSERT INTO endurance(id, v) VALUES ({id}, {id})"))
        })
        .unwrap();
    }

    let count =
        Db::query(|connection| connection.query_i64("SELECT COUNT(*) FROM endurance")).unwrap();
    assert_eq!(count, 1_000);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}
