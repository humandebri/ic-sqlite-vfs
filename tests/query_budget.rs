use ic_sqlite_vfs::db::{BudgetKind, QueryBudget, QueryError};
use ic_sqlite_vfs::test_support::{lock, memory};
use ic_sqlite_vfs::{params, Db, DbError};
use serial_test::serial;
use std::cell::Cell;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
    Db::update(|c| c.execute_batch("CREATE TABLE items(id INTEGER, body TEXT); INSERT INTO items VALUES (1, 'abc'), (2, 'def'), (3, 'ghi')")).unwrap();
}

#[test]
#[serial]
fn exact_limits_succeed_and_one_extra_row_fails_without_partial_results() {
    reset();
    let budget = QueryBudget {
        max_rows: 3,
        max_result_bytes: 33,
        ..QueryBudget::default()
    };
    let report = Db::query_with_budget(
        "SELECT id, body FROM items ORDER BY id",
        params![],
        budget,
        |r| Ok((r.get::<i64>(0)?, r.get::<String>(1)?)),
    );
    assert_eq!(report.result.unwrap().len(), 3);
    assert_eq!(report.metrics.result_rows, 3);
    assert_eq!(report.metrics.result_bytes, 33);
    assert!(report.metrics.statement.vm_steps > 0);
    assert!(report.metrics.statement.full_scan_steps > 0);
    assert!(report.metrics.statement.sorts > 0);
    assert_eq!(report.metrics.instructions, None);

    let mapped = Cell::new(0);
    let report = Db::query_with_budget(
        "SELECT id FROM items",
        params![],
        QueryBudget {
            max_rows: 2,
            ..budget
        },
        |r| {
            mapped.set(mapped.get() + 1);
            r.get::<i64>(0)
        },
    );
    assert!(matches!(
        report.result,
        Err(QueryError::BudgetExceeded(BudgetKind::Rows))
    ));
    assert_eq!(mapped.get(), 2);
    assert_eq!(report.metrics.result_rows, 2);
    assert!(report.metrics.statement.vm_steps > 0);
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>("SELECT COUNT(*) FROM items", params![])).unwrap(),
        3
    );
}

#[test]
#[serial]
fn oversized_blob_is_rejected_before_mapping_and_payload_classes_are_counted() {
    reset();
    let mapped = Cell::new(false);
    let report = Db::query_with_budget(
        "SELECT zeroblob(10000)",
        params![],
        QueryBudget {
            max_result_bytes: 9999,
            ..QueryBudget::default()
        },
        |r| {
            mapped.set(true);
            r.get::<Vec<u8>>(0)
        },
    );
    assert!(matches!(
        report.result,
        Err(QueryError::BudgetExceeded(BudgetKind::ResultBytes))
    ));
    assert!(!mapped.get());
    assert_eq!(report.metrics.result_bytes, 0);
    let report = Db::query_profiled("SELECT NULL, 1, 1.5, 'あ', x'0001'", params![], |_| Ok(()));
    report.result.unwrap();
    assert_eq!(report.metrics.result_bytes, 21);
}

#[test]
#[serial]
fn zero_limits_allow_empty_results_and_null_payload() {
    reset();
    let budget = QueryBudget {
        max_rows: 0,
        max_result_bytes: 0,
        ..QueryBudget::default()
    };
    assert!(
        Db::query_with_budget("SELECT id FROM items WHERE 0", params![], budget, |r| r
            .get::<i64>(0))
        .result
        .unwrap()
        .is_empty()
    );
    assert!(matches!(
        Db::query_with_budget("SELECT NULL", params![], budget, |_| Ok(())).result,
        Err(QueryError::BudgetExceeded(BudgetKind::Rows))
    ));
    assert_eq!(
        Db::query_with_budget(
            "SELECT NULL",
            params![],
            QueryBudget {
                max_rows: 1,
                ..budget
            },
            |_| Ok(())
        )
        .result
        .unwrap(),
        vec![()]
    );
}

#[test]
#[serial]
fn sql_bind_and_mapping_errors_keep_metrics_and_restore_connection_limits() {
    reset();
    let report = Db::query_profiled("SELECT id FROM items WHERE id = ?1", params![], |r| {
        r.get::<i64>(0)
    });
    assert!(matches!(
        report.result,
        Err(QueryError::Database(DbError::ParameterCountMismatch { .. }))
    ));
    let report = Db::query_profiled("SELECT id FROM items", params![], |r| r.get::<String>(0));
    assert!(matches!(
        report.result,
        Err(QueryError::Database(DbError::TypeMismatch { .. }))
    ));
    assert_eq!(report.metrics.result_rows, 1);
    assert!(report.metrics.statement.vm_steps > 0);
    assert!(
        Db::query_profiled("SELECT nope FROM items", params![], |_| Ok(()))
            .result
            .is_err()
    );
    assert!(matches!(
        Db::query_profiled("SELECT 1; SELECT 2", params![], |_| Ok(())).result,
        Err(QueryError::Database(DbError::TrailingSql))
    ));
    let report = Db::query_with_budget(
        "SELECT 'abcdefghijklmnopqrstuvwxyz'",
        params![],
        QueryBudget {
            max_sql_bytes: Some(10),
            ..QueryBudget::default()
        },
        |_| Ok(()),
    );
    assert!(report.result.is_err());
    let report = Db::query_with_budget(
        "SELECT zeroblob(1000)",
        params![],
        QueryBudget {
            max_value_bytes: Some(100),
            ..QueryBudget::default()
        },
        |_| Ok(()),
    );
    assert!(report.result.is_err());
    assert_eq!(
        Db::query_profiled("SELECT zeroblob(?1)", params![1000_i64], |r| r
            .get::<Vec<u8>>(0))
        .result
        .unwrap()[0]
            .len(),
        1000
    );
}

#[test]
#[serial]
fn nested_observer_is_rejected_and_panic_cleanup_restores_the_connection() {
    reset();
    Db::query_profiled("SELECT 1", params![], |_| {
        assert!(matches!(
            Db::query_profiled("SELECT 2", params![], |r| r.get::<i64>(0)).result,
            Err(QueryError::QueryObserverActive)
        ));
        Ok(())
    })
    .result
    .unwrap();
    let panic = std::panic::catch_unwind(|| {
        Db::query_with_budget(
            "SELECT 1",
            params![],
            QueryBudget {
                max_value_bytes: Some(100),
                ..QueryBudget::default()
            },
            |_| -> Result<(), DbError> { panic!("mapper panic") },
        );
    });
    assert!(panic.is_err());
    assert_eq!(
        Db::query_profiled("SELECT zeroblob(1000)", params![], |r| r.get::<Vec<u8>>(0))
            .result
            .unwrap()[0]
            .len(),
        1000
    );
}

#[test]
#[serial]
fn statement_metrics_reset_cached_execution_counters() {
    reset();
    Db::query(|c| {
        let mut stmt = c.prepare_cached("SELECT id FROM items")?;
        assert_eq!(stmt.query_all(params![], |r| r.get::<i64>(0))?.len(), 3);
        let metrics = stmt.reset_metrics();
        assert!(metrics.vm_steps > 0);
        assert_eq!(metrics.runs, 1);
        assert_eq!(stmt.metrics().vm_steps, 0);
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn validates_configuration_and_never_silently_ignores_host_instruction_budgets() {
    reset();
    for interval in [0, u32::MAX] {
        assert!(matches!(
            Db::query_with_budget(
                "SELECT 1",
                params![],
                QueryBudget {
                    progress_interval: interval,
                    ..QueryBudget::default()
                },
                |_| Ok(())
            )
            .result,
            Err(QueryError::InvalidQueryBudget(_))
        ));
    }
    assert!(matches!(
        Db::query_with_budget(
            "SELECT 1",
            params![],
            QueryBudget {
                max_instructions: Some(100),
                ..QueryBudget::default()
            },
            |_| Ok(())
        )
        .result,
        Err(QueryError::InstructionCounterUnavailable)
    ));
    assert!(
        Db::query_profiled("DELETE FROM items", params![], |_| Ok(()))
            .result
            .is_err()
    );
    assert_eq!(
        Db::query_profiled("SELECT COUNT(*) FROM items", params![], |r| r.get::<i64>(0))
            .result
            .unwrap(),
        vec![3]
    );
}

#[test]
#[serial]
fn detailed_vfs_metrics_are_opt_in() {
    reset();
    let report = Db::query_profiled("SELECT body FROM items", params![], |r| r.get::<String>(0));
    report.result.unwrap();
    #[cfg(feature = "query-metrics")]
    {
        let vfs = report.metrics.vfs.unwrap();
        assert!(vfs.read_calls > 0);
        assert!(vfs.stable_data_read_bytes > 0);
    }
    #[cfg(not(feature = "query-metrics"))]
    assert!(report.metrics.vfs.is_none());
}

#[test]
#[serial]
fn bound_values_obey_sqlite_length_limit_and_restore_after_failure() {
    reset();
    let value = vec![0_u8; 1000];
    let report = Db::query_with_budget(
        "SELECT ?1",
        params![value],
        QueryBudget {
            max_value_bytes: Some(100),
            ..QueryBudget::default()
        },
        |r| r.get::<Vec<u8>>(0),
    );
    assert!(matches!(report.result, Err(QueryError::Database(_))));
    assert_eq!(
        Db::query_profiled("SELECT ?1", params![value], |r| r.get::<Vec<u8>>(0))
            .result
            .unwrap()[0]
            .len(),
        1000
    );
}

#[test]
#[serial]
fn measured_queries_are_scoped_to_each_database_handle() {
    use ic_sqlite_vfs::{DbHandle, DefaultMemoryImpl, MemoryId, MemoryManager};
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let a = DbHandle::init(manager.get(MemoryId::new(30))).unwrap();
    let b = DbHandle::init(manager.get(MemoryId::new(31))).unwrap();
    for (handle, value) in [(a, 10_i64), (b, 20_i64)] {
        handle
            .update(|c| {
                c.execute_batch("CREATE TABLE item(value INTEGER)")?;
                c.execute("INSERT INTO item VALUES(?1)", params![value])
            })
            .unwrap();
    }
    assert!(matches!(
        a.query_with_budget(
            "SELECT value FROM item",
            params![],
            QueryBudget {
                max_rows: 0,
                ..QueryBudget::default()
            },
            |r| r.get::<i64>(0)
        )
        .result,
        Err(QueryError::BudgetExceeded(BudgetKind::Rows))
    ));
    assert_eq!(
        b.query_profiled("SELECT value FROM item", params![], |r| r.get::<i64>(0))
            .result
            .unwrap(),
        vec![20]
    );
    assert_eq!(
        a.query_profiled("SELECT value FROM item", params![], |r| r.get::<i64>(0))
            .result
            .unwrap(),
        vec![10]
    );
}
