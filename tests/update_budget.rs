use ic_sqlite_vfs::db::{UpdateBudget, UpdateBudgetKind, UpdateError};
use ic_sqlite_vfs::test_support::{lock, memory};
use ic_sqlite_vfs::{params, Db, DbError};
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
    Db::update(|c| c.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY, body BLOB); INSERT INTO items VALUES(1,x'01')")).unwrap();
}
fn count() -> i64 {
    Db::query(|c| c.query_scalar("SELECT count(*) FROM items", params![])).unwrap()
}
fn integrity() {
    assert_eq!(
        Db::query(|c| c.query_scalar::<String>("PRAGMA integrity_check", params![])).unwrap(),
        "ok"
    );
}

#[test]
#[serial]
fn success_reports_committed_pages_and_drains_returning() {
    reset();
    let report = Db::update_with_budget(
        "INSERT INTO items VALUES(2, ?1) RETURNING id",
        params![vec![7u8; 9000]],
        UpdateBudget::default(),
    );
    assert_eq!(report.result.unwrap(), 1);
    assert!(report.metrics.statement.vm_steps > 0);
    assert!(report.metrics.peak_dirty_pages > 0);
    assert!(report.metrics.overlay_peak_bytes >= 9000);
    assert!(report.metrics.committed_bytes > 0);
    assert_eq!(report.metrics.instructions, None);
    assert_eq!(count(), 2);
    integrity();
}

#[test]
#[serial]
fn page_and_byte_overflow_roll_back_the_whole_statement() {
    for bytes in [false, true] {
        reset();
        let before = ic_sqlite_vfs::test_support::stable_blob::storage_stats().unwrap();
        let budget = if bytes {
            UpdateBudget {
                max_changed_bytes: 4095,
                ..UpdateBudget::default()
            }
        } else {
            UpdateBudget {
                max_dirty_pages: 1,
                ..UpdateBudget::default()
            }
        };
        let report = Db::update_with_budget("WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<100) INSERT INTO items SELECT x, zeroblob(20000) FROM n", params![], budget);
        assert!(
            matches!(report.result, Err(UpdateError::BudgetExceeded(kind)) if kind == if bytes { UpdateBudgetKind::ChangedBytes } else { UpdateBudgetKind::DirtyPages })
        );
        assert_eq!(report.metrics.committed_bytes, 0);
        assert!(report.metrics.peak_dirty_pages <= budget.max_dirty_pages);
        assert!(report.metrics.overlay_peak_bytes <= budget.max_changed_bytes);
        assert_eq!(count(), 1);
        let after = ic_sqlite_vfs::test_support::stable_blob::storage_stats().unwrap();
        assert_eq!(before.active_bytes, after.active_bytes);
        // Ordinary writes remain usable after failed COMMIT/cache spill.
        Db::update(|c| c.execute("INSERT INTO items VALUES(2,x'02')", params![])).unwrap();
        assert_eq!(count(), 2);
        integrity();
    }
}

#[test]
#[serial]
fn zero_pages_allow_noop_but_reject_actual_changes() {
    reset();
    let budget = UpdateBudget {
        max_dirty_pages: 0,
        max_changed_bytes: 0,
        ..UpdateBudget::default()
    };
    assert_eq!(
        Db::update_with_budget("UPDATE items SET body=x'02' WHERE id=99", params![], budget)
            .result
            .unwrap(),
        0
    );
    assert!(matches!(
        Db::update_with_budget("UPDATE items SET body=x'02'", params![], budget).result,
        Err(UpdateError::BudgetExceeded(_))
    ));
    assert_eq!(
        Db::query(|c| c.query_scalar::<Vec<u8>>("SELECT body FROM items", params![])).unwrap(),
        vec![1]
    );
    integrity();
}

#[test]
#[serial]
fn transaction_escape_and_connection_mutation_are_rejected() {
    reset();
    for sql in [
        "COMMIT",
        "END",
        "ROLLBACK",
        "BEGIN",
        "SAVEPOINT s",
        "RELEASE s",
        "PRAGMA journal_mode=OFF",
        "PRAGMA data_version=1",
        "PRAGMA data_version",
        "ATTACH ':memory:' AS other",
        "DETACH other",
        "CREATE TEMP TABLE escape(x)",
        "SELECT 1",
        "INSERT INTO items VALUES(2,x'02'); COMMIT",
    ] {
        let report = Db::update_with_budget(sql, params![], UpdateBudget::default());
        assert!(report.result.is_err(), "unexpected success: {sql}");
        assert_eq!(count(), 1);
    }
    assert!(Db::update_with_budget(
        "INSERT INTO items VALUES(2,x'02')",
        params![],
        UpdateBudget::default()
    )
    .result
    .is_ok());
    integrity();
}

#[test]
#[serial]
fn trigger_changes_and_schema_changes_remain_atomic() {
    reset();
    Db::update(|c| c.execute_batch("CREATE TABLE audit(x BLOB); CREATE TRIGGER log AFTER INSERT ON items BEGIN INSERT INTO audit VALUES(zeroblob(20000)); END")).unwrap();
    let report = Db::update_with_budget(
        "INSERT INTO items VALUES(2,x'02')",
        params![],
        UpdateBudget {
            max_dirty_pages: 1,
            ..UpdateBudget::default()
        },
    );
    assert!(matches!(report.result, Err(UpdateError::BudgetExceeded(_))));
    assert_eq!(count(), 1);
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>("SELECT count(*) FROM audit", params![])).unwrap(),
        0
    );
    assert!(Db::update_with_budget(
        "CREATE TABLE new_table AS SELECT zeroblob(10000) AS body",
        params![],
        UpdateBudget {
            max_dirty_pages: 0,
            ..UpdateBudget::default()
        }
    )
    .result
    .is_err());
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>(
            "SELECT count(*) FROM sqlite_schema WHERE name='new_table'",
            params![]
        ))
        .unwrap(),
        0
    );
    integrity();
}

#[test]
#[serial]
fn sql_binding_constraint_and_configuration_errors_preserve_database() {
    reset();
    for sql in [
        "INSERT INTO items VALUES(2,?1)",
        "INSERT INTO items VALUES(1,x'02')",
        "not SQL",
    ] {
        assert!(
            Db::update_with_budget(sql, params![], UpdateBudget::default())
                .result
                .is_err()
        );
        assert_eq!(count(), 1);
    }
    assert!(matches!(
        Db::update_with_budget(
            "DELETE FROM items",
            params![],
            UpdateBudget {
                progress_interval: 0,
                ..UpdateBudget::default()
            }
        )
        .result,
        Err(UpdateError::InvalidBudget(_))
    ));
    assert!(matches!(
        Db::update_with_budget(
            "DELETE FROM items",
            params![],
            UpdateBudget {
                max_instructions: Some(1),
                commit_reserve_instructions: 1,
                ..UpdateBudget::default()
            }
        )
        .result,
        Err(UpdateError::InvalidBudget(_))
    ));
    assert!(matches!(
        Db::update_with_budget(
            "DELETE FROM items",
            params![],
            UpdateBudget {
                max_instructions: Some(100),
                commit_reserve_instructions: 1,
                ..UpdateBudget::default()
            }
        )
        .result,
        Err(UpdateError::InstructionCounterUnavailable)
    ));
    assert!(Db::update_with_budget(
        "INSERT INTO items VALUES(2,?1)",
        params![vec![1u8; 10000]],
        UpdateBudget {
            max_value_bytes: Some(1000),
            ..UpdateBudget::default()
        }
    )
    .result
    .is_err());
    assert!(Db::update_with_budget(
        "INSERT INTO items VALUES(2,?1)",
        params![vec![1u8; 10000]],
        UpdateBudget::default()
    )
    .result
    .is_ok());
    integrity();
}

#[test]
#[serial]
fn active_read_connection_blocks_budgeted_update() {
    reset();
    Db::query(|_| {
        assert!(matches!(
            Db::update_with_budget("DELETE FROM items", params![], UpdateBudget::default()).result,
            Err(UpdateError::Database(DbError::ReadConnectionInUse))
        ));
        Ok(())
    })
    .unwrap();
    assert_eq!(count(), 1);
}

#[test]
#[serial]
fn ddl_returns_zero_and_fts5_dml_remains_supported() {
    reset();
    let created = Db::update_with_budget(
        "CREATE VIRTUAL TABLE search USING fts5(body)",
        params![],
        UpdateBudget::default(),
    );
    assert_eq!(created.result.unwrap(), 0);
    assert_eq!(
        Db::update_with_budget(
            "INSERT INTO search VALUES('alpha beta')",
            params![],
            UpdateBudget::default()
        )
        .result
        .unwrap(),
        1
    );
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>(
            "SELECT count(*) FROM search WHERE search MATCH 'alpha'",
            params![]
        ))
        .unwrap(),
        1
    );
    assert_eq!(
        Db::update_with_budget(
            "CREATE TABLE extra(id INTEGER)",
            params![],
            UpdateBudget::default()
        )
        .result
        .unwrap(),
        0
    );
    integrity();
}

#[test]
#[serial]
fn write_limits_are_scoped_to_each_database_handle() {
    use ic_sqlite_vfs::{DbHandle, DefaultMemoryImpl, MemoryId, MemoryManager};
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let a = DbHandle::init(manager.get(MemoryId::new(30))).unwrap();
    let b = DbHandle::init(manager.get(MemoryId::new(31))).unwrap();
    for db in [a, b] {
        db.update(|c| c.execute_batch("CREATE TABLE items(id INTEGER)"))
            .unwrap();
    }
    assert!(a
        .update_with_budget(
            "INSERT INTO items VALUES(1)",
            params![],
            UpdateBudget {
                max_dirty_pages: 0,
                ..UpdateBudget::default()
            }
        )
        .result
        .is_err());
    assert!(b
        .update_with_budget(
            "INSERT INTO items VALUES(2)",
            params![],
            UpdateBudget::default()
        )
        .result
        .is_ok());
    assert_eq!(
        a.query(|c| c.query_scalar::<i64>("SELECT count(*) FROM items", params![]))
            .unwrap(),
        0
    );
    assert_eq!(
        b.query(|c| c.query_scalar::<i64>("SELECT count(*) FROM items", params![]))
            .unwrap(),
        1
    );
    assert!(a
        .update_with_budget(
            "INSERT INTO items VALUES(3)",
            params![],
            UpdateBudget::default()
        )
        .result
        .is_ok());
}

#[test]
#[serial]
fn caught_binding_panic_allows_the_first_follow_up_write() {
    struct PanickingValue;
    impl ic_sqlite_vfs::db::ToSql for PanickingValue {
        fn bind_to(
            &self,
            _statement: *mut ic_sqlite_vfs::test_support::ffi::sqlite3_stmt,
            _index: std::ffi::c_int,
        ) -> Result<(), DbError> {
            panic!("binding panic regression probe");
        }
    }

    for budgeted_recovery in [false, true] {
        reset();
        let before = ic_sqlite_vfs::test_support::stable_blob::storage_stats().unwrap();
        let panic = std::panic::catch_unwind(|| {
            Db::update_with_budget(
                "INSERT INTO items VALUES(2,?1)",
                params![PanickingValue],
                UpdateBudget::default(),
            )
        });
        assert!(panic.is_err());
        assert_eq!(count(), 1);
        let after = ic_sqlite_vfs::test_support::stable_blob::storage_stats().unwrap();
        assert_eq!(before.active_bytes, after.active_bytes);
        assert_eq!(before.allocated_bytes, after.allocated_bytes);
        if budgeted_recovery {
            assert_eq!(
                Db::update_with_budget(
                    "INSERT INTO items VALUES(2,x'02')",
                    params![],
                    UpdateBudget::default(),
                )
                .result
                .unwrap(),
                1
            );
        } else {
            Db::update(|c| c.execute("INSERT INTO items VALUES(2,x'02')", params![])).unwrap();
        }
        assert_eq!(count(), 2);
        integrity();
    }
}
