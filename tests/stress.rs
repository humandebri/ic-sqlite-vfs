use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::test_support::lock;
use ic_sqlite_vfs::test_support::memory;
use ic_sqlite_vfs::test_support::Superblock;
use ic_sqlite_vfs::{params, Db};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serial_test::serial;
use std::collections::BTreeMap;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
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
fn duplicate_migration_version_is_rejected_before_schema_changes() {
    reset();
    let result = Db::migrate(&[
        Migration {
            version: 1,
            sql: "CREATE TABLE duplicate_first(id INTEGER PRIMARY KEY);",
        },
        Migration {
            version: 1,
            sql: "CREATE TABLE duplicate_second(id INTEGER PRIMARY KEY);",
        },
    ]);

    assert!(result.is_err());
    assert_eq!(Superblock::load().unwrap().schema_version, 0);
    Db::migrate(&[Migration {
        version: 2,
        sql: "CREATE TABLE after_duplicate(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();
    let exists = Db::query(|connection| {
        connection.query_scalar::<i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'duplicate_first'",
            params![],
        )
    })
    .unwrap();
    assert_eq!(exists, 0);
}

#[test]
#[serial]
fn migration_registry_records_versions_without_wall_clock_time() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE clockless(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let columns = Db::query(|connection| {
        connection.query_column::<String>(
            "SELECT name FROM pragma_table_info('__ic_sqlite_migrations') ORDER BY cid",
            params![],
        )
    })
    .unwrap();
    assert_eq!(columns, vec!["version"]);

    let version = Db::query(|connection| {
        connection.query_scalar::<i64>(
            "SELECT version FROM __ic_sqlite_migrations WHERE version = 1",
            params![],
        )
    })
    .unwrap();
    assert_eq!(version, 1);
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

    let sum = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COALESCE(SUM(v), 0) FROM fuzz", params![])
    })
    .unwrap();
    let expected = model.values().sum::<u64>();
    assert_eq!(u64::try_from(sum).unwrap(), expected);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

#[test]
#[serial]
fn pbt_operation_sequences_match_model_after_compact_and_import() {
    let strategy = prop::collection::vec((0_u8..5, 0_i64..48, -50_000_i64..50_000), 1..96);
    let mut runner = TestRunner::new(Config {
        cases: 96,
        max_shrink_iters: 2_048,
        failure_persistence: None,
        ..Config::default()
    });

    runner
        .run(&strategy, |operations| {
            reset();
            Db::migrate(&[Migration {
                version: 1,
                sql: "CREATE TABLE pbt(k INTEGER PRIMARY KEY, v INTEGER NOT NULL);",
            }])
            .map_err(|error| TestCaseError::fail(error.to_string()))?;

            let mut model = BTreeMap::<i64, i64>::new();
            for (kind, key, value) in operations {
                match kind {
                    0 => {
                        model.insert(key, value);
                        Db::update(|connection| {
                            connection.execute(
                                "INSERT INTO pbt(k, v) VALUES (?1, ?2)
                                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                                params![key, value],
                            )
                        })
                    }
                    1 => {
                        model.remove(&key);
                        Db::update(|connection| {
                            connection.execute("DELETE FROM pbt WHERE k = ?1", params![key])
                        })
                    }
                    2 => {
                        if let Some(stored) = model.get_mut(&key) {
                            *stored = stored.saturating_add(value);
                        }
                        Db::update(|connection| {
                            connection.execute(
                                "UPDATE pbt SET v = v + ?1 WHERE k = ?2",
                                params![value, key],
                            )
                        })
                    }
                    3 => {
                        let before = read_pbt_rows()?;
                        Db::update(|connection| {
                            connection.execute(
                                "INSERT INTO pbt(k, v) VALUES (?1, ?2)",
                                params![key, value],
                            )?;
                            connection.execute(
                                "INSERT INTO pbt(k, v) VALUES (?1, ?2)",
                                params![key, value.saturating_add(1)],
                            )
                        })
                        .expect_err("duplicate insert must rollback");
                        prop_assert_eq!(read_pbt_rows()?, before);
                        Ok(())
                    }
                    _ => {
                        let before = read_pbt_rows()?;
                        Db::refresh_checksum()
                            .map_err(|error| TestCaseError::fail(error.to_string()))?;
                        prop_assert_eq!(read_pbt_rows()?, before);
                        Ok(())
                    }
                }
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert_eq!(read_pbt_rows()?, model.clone());
                prop_assert_eq!(Db::integrity_check().unwrap(), "ok");
            }

            Db::compact().map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(read_pbt_rows()?, model.clone());

            let db_size = Superblock::load().unwrap().db_size;
            let checksum =
                Db::refresh_checksum().map_err(|error| TestCaseError::fail(error.to_string()))?;
            let image = Db::export_chunk(0, db_size)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            Db::begin_import(db_size, checksum)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            Db::import_chunk(0, &image).map_err(|error| TestCaseError::fail(error.to_string()))?;
            Db::finish_import().map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(read_pbt_rows()?, model.clone());
            prop_assert_eq!(Db::integrity_check().unwrap(), "ok");
            Ok(())
        })
        .unwrap();
}

fn read_pbt_rows() -> Result<BTreeMap<i64, i64>, TestCaseError> {
    let rows = Db::query(|connection| {
        connection.query_map::<(i64, i64), _>("SELECT k, v FROM pbt ORDER BY k", params![], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
    })
    .map_err(|error| TestCaseError::fail(error.to_string()))?;
    Ok(rows.into_iter().collect())
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
    let checksum = Db::refresh_checksum().unwrap();
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

    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM endurance", params![])
    })
    .unwrap();
    assert_eq!(count, 1_000);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}
