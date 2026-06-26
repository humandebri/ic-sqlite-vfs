use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::{DbError, NULL};
use ic_sqlite_vfs::test_support::lock;
use ic_sqlite_vfs::test_support::memory;
use ic_sqlite_vfs::{named_params, params, Db};
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
#[serial]
fn typed_bind_and_column_read_cover_sqlite_storage_classes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE typed_values(
            id INTEGER PRIMARY KEY,
            text_value TEXT NOT NULL,
            integer_value INTEGER NOT NULL,
            real_value REAL NOT NULL,
            blob_value BLOB NOT NULL,
            null_value TEXT
        );",
    }])
    .unwrap();

    let blob = vec![0, 1, 2, 255];
    Db::update(|connection| {
        connection.execute(
            "INSERT INTO typed_values(
                id, text_value, integer_value, real_value, blob_value, null_value
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![1_i64, "alpha", 42_i64, 3.5_f64, blob, NULL],
        )
    })
    .unwrap();

    let row = Db::query(|connection| {
        connection.query_one(
            "SELECT text_value, integer_value, real_value, blob_value, null_value
             FROM typed_values WHERE id = ?1",
            params![1_i64],
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<i64>(1)?,
                    row.get::<f64>(2)?,
                    row.get::<Vec<u8>>(3)?,
                    row.get::<Option<String>>(4)?,
                ))
            },
        )
    })
    .unwrap();

    assert_eq!(row, ("alpha".to_string(), 42, 3.5, blob, None));
}

#[test]
#[serial]
fn text_column_read_preserves_embedded_nul_bytes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE text_values(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch("INSERT INTO text_values(id, body) VALUES (1, char(65, 0, 66))")
    })
    .unwrap();

    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT body FROM text_values WHERE id = 1", params![])
    })
    .unwrap();

    assert_eq!(value.as_bytes(), b"A\0B");
}

#[test]
#[serial]
fn query_helpers_report_missing_rows_and_collect_rows() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO items(id, name) VALUES (?1, ?2)")?;
        statement.execute(params![1_i64, "one"])?;
        statement.execute(params![2_i64, "two"])?;
        statement.execute(params![3_i64, "A\0B"])?;
        Ok(())
    })
    .unwrap();

    let names = Db::query(|connection| {
        connection.query_column::<String>("SELECT name FROM items ORDER BY id", params![])
    })
    .unwrap();
    assert_eq!(
        names,
        vec!["one".to_string(), "two".to_string(), "A\0B".to_string()]
    );

    let lengths = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT name FROM items ORDER BY id")?;
        let mut rows = statement.query(params![])?;
        let mut lengths = Vec::new();
        while let Some(len) = rows.next_text_len_zero()? {
            lengths.push(len);
        }
        Ok(lengths)
    })
    .unwrap();
    assert_eq!(lengths, vec![3, 3, 3]);

    let cached_optional = Db::query(|connection| {
        let first = connection
            .query_optional_string_text("SELECT name FROM items WHERE name = ?1", "A\0B")?;
        let second = connection
            .query_optional_string_text("SELECT name FROM items WHERE name = ?1", "two")?;
        let missing = connection
            .query_optional_string_text("SELECT name FROM items WHERE name = ?1", "missing")?;
        Ok((first, second, missing))
    })
    .unwrap();
    assert_eq!(
        cached_optional,
        (Some("A\0B".to_string()), Some("two".to_string()), None)
    );

    let cached_temporary_key = Db::query(|connection| {
        let first = connection.query_optional_string_text(
            "SELECT name FROM items WHERE name = ?1",
            &["t", "wo"].concat(),
        )?;
        let second = connection.query_optional_string_text(
            "SELECT name FROM items WHERE name = ?1",
            &["A\0", "B"].concat(),
        )?;
        Ok((first, second))
    })
    .unwrap();
    assert_eq!(
        cached_temporary_key,
        (Some("two".to_string()), Some("A\0B".to_string()))
    );

    let exists = Db::query(|connection| {
        connection.exists(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id = ?1)",
            params![2_i64],
        )
    })
    .unwrap();
    assert!(exists);

    let optional = Db::query(|connection| {
        connection
            .query_optional_scalar::<String>("SELECT name FROM items WHERE id = ?1", params![4_i64])
    })
    .unwrap();
    assert_eq!(optional, None);

    let missing = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT name FROM items WHERE id = ?1", params![4_i64])
    });
    assert!(matches!(missing, Err(DbError::NotFound)));
}

#[test]
#[serial]
fn specialized_query_fast_paths_match_generic_queries() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE fast_items(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute(
            "INSERT INTO fast_items(k, v) VALUES
                (?1, ?2),
                (?3, ?4),
                (?5, ?6),
                (?7, ?8)",
            params![
                "alpha",
                "one",
                "nul",
                "A\0B",
                "",
                "empty-key",
                "long",
                "0123456789abcdef".repeat(16)
            ],
        )
    })
    .unwrap();

    Db::query(|connection| {
        for key in ["alpha", "nul", "", "long", "missing"] {
            let fast = connection
                .query_optional_string_text("SELECT v FROM fast_items WHERE k = ?1", key)?;
            let generic = connection.query_optional_scalar::<String>(
                "SELECT v FROM fast_items WHERE k = ?1",
                params![key],
            )?;
            assert_eq!(fast, generic);
        }

        let sql = "SELECT v FROM fast_items WHERE k IN (?1, ?2, ?3, ?4) ORDER BY k";
        let keys = ["alpha", "nul", "", "missing"];
        let generic =
            connection.query_column::<String>(sql, params![keys[0], keys[1], keys[2], keys[3]])?;
        let expected_len = generic.iter().map(String::len).sum::<usize>() as u64;
        let first = connection.query_text_iter_text_len_sum(sql, keys.iter().copied())?;
        let second = connection.query_text_iter_text_len_sum(sql, keys.iter().copied())?;
        assert_eq!(first, expected_len);
        assert_eq!(second, expected_len);
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn specialized_execute_helpers_reuse_borrowed_text_bindings() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE typed_exec(
            id INTEGER PRIMARY KEY,
            group_id INTEGER NOT NULL,
            body TEXT NOT NULL
        );",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_text_text(
            "INSERT INTO typed_exec(id, group_id, body) VALUES (3, ?1, ?2)",
            "8",
            "three",
        )?;

        let mut one_text =
            connection.prepare("INSERT INTO typed_exec(id, group_id, body) VALUES (?1, 0, ?2)")?;
        one_text.execute_i64_text(1, "one")?;

        let mut two_ints =
            connection.prepare("INSERT INTO typed_exec(id, group_id, body) VALUES (?1, ?2, ?3)")?;
        two_ints.execute_i64_i64_text(2, 7, "two")
    })
    .unwrap();

    let rows = Db::query(|connection| {
        connection.query_column::<String>(
            "SELECT group_id || ':' || body FROM typed_exec ORDER BY id",
            params![],
        )
    })
    .unwrap();

    assert_eq!(rows, vec!["0:one", "7:two", "8:three"]);
}

#[test]
#[serial]
fn specialized_execute_helpers_clear_temporary_bindings_before_return() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE temporary_exec(
            id INTEGER PRIMARY KEY,
            group_id INTEGER NOT NULL,
            body TEXT NOT NULL,
            blob_body BLOB NOT NULL DEFAULT x''
        );",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_text_text(
            "INSERT INTO temporary_exec(id, group_id, body) VALUES (1, ?1, ?2)",
            &format!("{}", 10),
            &format!("body-{}", 1),
        )?;
        {
            let mut cached = connection.prepare_cached(
                "INSERT INTO temporary_exec(id, group_id, body) VALUES (?1, 15, ?2)",
            )?;
            cached.execute_i64_text(6, &format!("body-{}", 6))?;
        }
        {
            let mut cached = connection.prepare_cached(
                "INSERT INTO temporary_exec(id, group_id, body) VALUES (?1, 15, ?2)",
            )?;
            cached.execute_i64_text(7, &format!("body-{}", 7))?;
        }

        let mut text_statement = connection
            .prepare("INSERT INTO temporary_exec(id, group_id, body) VALUES (?1, 20, ?2)")?;
        text_statement.execute_i64_text(2, &format!("body-{}", 2))?;
        text_statement.execute_i64_text(3, &format!("body-{}", 3))?;

        let mut blob_statement =
            connection.prepare("UPDATE temporary_exec SET blob_body = ?2 WHERE id = ?1")?;
        blob_statement.execute_i64_blob(2, format!("blob-{}", 2).as_bytes())?;
        blob_statement.execute_i64_blob(3, format!("blob-{}", 3).as_bytes())?;

        let mut mixed_statement = connection
            .prepare("INSERT INTO temporary_exec(id, group_id, body) VALUES (?1, ?2, ?3)")?;
        mixed_statement.execute_i64_i64_text(4, 40, &format!("body-{}", 4))?;
        mixed_statement.execute_i64_i64_text(5, 50, &format!("body-{}", 5))
    })
    .unwrap();

    let rows = Db::query(|connection| {
        connection.query_column::<String>(
            "SELECT group_id || ':' || body || ':' || length(blob_body)
             FROM temporary_exec
             ORDER BY id",
            params![],
        )
    })
    .unwrap();

    assert_eq!(
        rows,
        vec![
            "10:body-1:0",
            "20:body-2:6",
            "20:body-3:6",
            "40:body-4:0",
            "50:body-5:0",
            "15:body-6:0",
            "15:body-7:0",
        ]
    );
}

#[test]
#[serial]
fn connection_changes_reports_last_statement_row_count() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE change_items(id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch(
            "INSERT INTO change_items(id, value) VALUES
                (1, 'one'),
                (2, 'two'),
                (3, 'three')",
        )?;
        assert_eq!(connection.changes(), 3);

        connection.execute(
            "UPDATE change_items SET value = ?1 WHERE id <= ?2",
            params!["updated", 2_i64],
        )?;
        assert_eq!(connection.changes(), 2);

        connection.execute(
            "UPDATE change_items SET value = ?1 WHERE id = ?2",
            params!["missing", 99_i64],
        )?;
        assert_eq!(connection.changes(), 0);
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn specialized_execute_blob_helper_reuses_borrowed_bytes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE typed_blob(
            id INTEGER PRIMARY KEY,
            body BLOB NOT NULL
        );",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement =
            connection.prepare("INSERT INTO typed_blob(id, body) VALUES (?1, ?2)")?;
        statement.execute_i64_blob(1, b"abc")?;
        statement.execute_i64_blob(2, b"")
    })
    .unwrap();

    let rows = Db::query(|connection| {
        connection.query_column::<i64>("SELECT length(body) FROM typed_blob ORDER BY id", params![])
    })
    .unwrap();

    assert_eq!(rows, vec![3, 0]);
}

#[test]
#[serial]
fn rusqlite_style_query_aliases_delegate_to_existing_helpers() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE alias_items(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch(
            "INSERT INTO alias_items(id, name) VALUES (1, 'one'), (2, 'two'), (3, 'three')",
        )
    })
    .unwrap();

    let one = Db::query(|connection| {
        connection.query_row(
            "SELECT name FROM alias_items WHERE id = ?1",
            params![1_i64],
            |row| row.get::<String>(0),
        )
    })
    .unwrap();
    assert_eq!(one, "one");

    let missing = Db::query(|connection| {
        connection.query_row(
            "SELECT name FROM alias_items WHERE id = ?1",
            params![4_i64],
            |row| row.get::<String>(0),
        )
    });
    assert!(matches!(missing, Err(DbError::NotFound)));

    let names = Db::query(|connection| {
        connection.query_map(
            "SELECT name FROM alias_items WHERE id >= ?1 ORDER BY id",
            params![2_i64],
            |row| row.get::<String>(0),
        )
    })
    .unwrap();
    assert_eq!(names, vec!["two".to_string(), "three".to_string()]);
}

#[test]
#[serial]
fn rusqlite_style_named_query_aliases_delegate_to_existing_helpers() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE alias_named_items(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection
            .execute_batch("INSERT INTO alias_named_items(id, name) VALUES (1, 'one'), (2, 'two')")
    })
    .unwrap();

    let one = Db::query(|connection| {
        connection.query_row_named(
            "SELECT name FROM alias_named_items WHERE id = :id",
            named_params![":id" => 1_i64],
            |row| row.get::<String>(0),
        )
    })
    .unwrap();
    assert_eq!(one, "one");

    let names = Db::query(|connection| {
        connection.query_map_named(
            "SELECT name FROM alias_named_items WHERE id >= :min_id ORDER BY id",
            named_params![":min_id" => 1_i64],
            |row| row.get::<String>(0),
        )
    })
    .unwrap();
    assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
}

#[test]
#[serial]
fn named_parameters_bind_by_sql_name() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE named_items(id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_named(
            "INSERT INTO named_items(id, name, score) VALUES (:id, @name, $score)",
            named_params![":id" => 1_i64, "@name" => "named", "$score" => 2.5_f64],
        )
    })
    .unwrap();

    let row = Db::query(|connection| {
        connection.query_one_named(
            "SELECT name, score FROM named_items WHERE id = :id",
            named_params![":id" => 1_i64],
            |row| Ok((row.get::<String>(0)?, row.get::<f64>(1)?)),
        )
    })
    .unwrap();
    assert_eq!(row, ("named".to_string(), 2.5));

    let missing = Db::query(|connection| {
        connection.query_optional_scalar_named::<String>(
            "SELECT name FROM named_items WHERE id = :id",
            named_params![":id" => 2_i64],
        )
    })
    .unwrap();
    assert_eq!(missing, None);

    let error = Db::query(|connection| {
        connection.query_scalar_named::<String>(
            "SELECT name FROM named_items WHERE id = :id",
            named_params![":missing" => 1_i64],
        )
    });
    assert!(matches!(error, Err(DbError::ParameterNotFound(_))));
}

#[test]
#[serial]
fn statement_row_iterator_steps_until_done() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE iter_items(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute(
            "INSERT INTO iter_items(id, name) VALUES (?1, ?2)",
            params![1_i64, "a"],
        )?;
        connection.execute(
            "INSERT INTO iter_items(id, name) VALUES (?1, ?2)",
            params![2_i64, "b"],
        )
    })
    .unwrap();

    let values = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT name FROM iter_items ORDER BY id")?;
        statement.query_column::<String>(params![])
    })
    .unwrap();

    assert_eq!(values, vec!["a".to_string(), "b".to_string()]);

    let limited = Db::query(|connection| {
        let mut statement =
            connection.prepare("SELECT name FROM iter_items WHERE id <= ?1 ORDER BY id")?;
        let mut rows = statement.query_i64(1)?;
        let mut values = Vec::new();
        while let Some(row) = rows.next_row()? {
            values.push(row.get::<String>(0)?);
        }
        Ok(values)
    })
    .unwrap();
    assert_eq!(limited, vec!["a".to_string()]);

    let text_len_total = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT name FROM iter_items ORDER BY id")?;
        let mut rows = statement.query(params![])?;
        let mut total = 0_usize;
        while let Some(len) = rows.next_text_len_zero()? {
            total += len;
        }
        Ok(total)
    })
    .unwrap();
    assert_eq!(text_len_total, 2);

    let text_values = Db::query(|connection| {
        let mut statement =
            connection.prepare("SELECT name FROM iter_items WHERE name IN (?1, ?2) ORDER BY id")?;
        let values = ["a", "b"];
        let mut rows = statement.query_texts(&values)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next_row()? {
            out.push(row.get::<String>(0)?);
        }
        Ok(out)
    })
    .unwrap();
    assert_eq!(text_values, vec!["a".to_string(), "b".to_string()]);

    let text_values_iter = Db::query(|connection| {
        let mut statement =
            connection.prepare("SELECT name FROM iter_items WHERE name IN (?1, ?2) ORDER BY id")?;
        let values = ["a", "b"];
        let mut rows = statement.query_text_iter(values.iter().copied())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next_row()? {
            out.push(row.get::<String>(0)?);
        }
        Ok(out)
    })
    .unwrap();
    assert_eq!(text_values_iter, vec!["a".to_string(), "b".to_string()]);

    let ephemeral_text_values_iter = Db::query(|connection| {
        let mut statement =
            connection.prepare("SELECT name FROM iter_items WHERE name IN (?1, ?2) ORDER BY id")?;
        let values = ["a", "b"];
        let mut rows = statement.query_text_iter_ephemeral(values.iter().copied())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next_row()? {
            out.push(row.get::<String>(0)?);
        }
        Ok(out)
    })
    .unwrap();
    assert_eq!(
        ephemeral_text_values_iter,
        vec!["a".to_string(), "b".to_string()]
    );

    let ephemeral_drop_then_reuse = Db::query(|connection| {
        let mut statement =
            connection.prepare("SELECT name FROM iter_items WHERE name IN (?1, ?2) ORDER BY id")?;
        {
            let values = ["a".to_string(), "b".to_string()];
            let mut rows =
                statement.query_text_iter_ephemeral(values.iter().map(String::as_str))?;
            assert_eq!(
                rows.next_row()?
                    .map(|row| row.get::<String>(0))
                    .transpose()?,
                Some("a".to_string())
            );
        }
        let values = ["b", "a"];
        let mut rows = statement.query_text_iter_ephemeral(values.iter().copied())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next_row()? {
            out.push(row.get::<String>(0)?);
        }
        Ok(out)
    })
    .unwrap();
    assert_eq!(
        ephemeral_drop_then_reuse,
        vec!["a".to_string(), "b".to_string()]
    );

    let count = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT COUNT(*) FROM iter_items")?;
        statement.query_scalar::<i64>(params![])
    })
    .unwrap();
    assert_eq!(count, 2);
}

#[test]
#[serial]
fn borrowed_text_rows_clear_bindings_after_early_errors_and_drop() {
    reset();

    Db::query(|connection| {
        let mut statement = connection.prepare("SELECT ?1 WHERE ?1 IN (?1, ?2)")?;
        {
            let values = ["alpha".to_string(), "beta".to_string()];
            let mut rows =
                statement.query_text_iter_ephemeral(values.iter().map(String::as_str))?;
            let row = rows.next_row()?.expect("first borrowed row exists");
            let error = row.get::<i64>(0).unwrap_err();
            assert!(matches!(error, DbError::TypeMismatch { .. }));
        }

        let values = ["gamma", "gamma"];
        let mut rows = statement.query_text_iter_ephemeral(values.iter().copied())?;
        let value = rows
            .next_row()?
            .map(|row| row.get::<String>(0))
            .transpose()?;
        assert_eq!(value, Some("gamma".to_string()));
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn borrowed_text_iter_bind_errors_clear_partial_bindings() {
    reset();

    Db::query(|connection| {
        let mut statement = connection.prepare("SELECT ?1, ?2")?;
        {
            let mismatch = statement.query_text_iter_ephemeral(["only-one"].iter().copied());
            assert!(matches!(
                mismatch,
                Err(DbError::ParameterCountMismatch {
                    expected: 2,
                    actual: 1
                })
            ));
        }

        let mut rows = statement.query_text_iter_ephemeral(["left", "right"].iter().copied())?;
        let row = rows.next_row()?.expect("row exists after bind error");
        assert_eq!(row.get::<String>(0)?, "left");
        assert_eq!(row.get::<String>(1)?, "right");
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn cached_borrowed_text_helpers_reuse_statement_after_type_error() {
    reset();

    Db::query(|connection| {
        let error = connection.query_optional_string_text("SELECT 1 WHERE ?1 = ?1", "x");
        assert!(matches!(error, Err(DbError::TypeMismatch { .. })));

        let value = connection.query_optional_string_text("SELECT ?1 WHERE ?1 = ?1", "after")?;
        assert_eq!(value, Some("after".to_string()));
        Ok(())
    })
    .unwrap();
}

#[test]
#[serial]
fn typed_errors_preserve_constraint_and_type_information() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE unique_items(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);",
    }])
    .unwrap();

    let duplicate = Db::update(|connection| {
        connection.execute(
            "INSERT INTO unique_items(name) VALUES (?1)",
            params!["same"],
        )?;
        connection.execute(
            "INSERT INTO unique_items(name) VALUES (?1)",
            params!["same"],
        )
    });
    assert!(matches!(duplicate, Err(DbError::Constraint(_))));

    let mismatch = Db::query(|connection| connection.query_scalar::<String>("SELECT 1", params![]));
    assert!(matches!(mismatch, Err(DbError::TypeMismatch { .. })));
}

#[test]
#[serial]
fn savepoint_rolls_back_inner_work_without_escaping_update() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE logs(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute("INSERT INTO logs(body) VALUES (?1)", params!["outer"])?;
        let inner = connection.savepoint(|connection| {
            connection.execute("INSERT INTO logs(body) VALUES (?1)", params!["inner"])?;
            connection.execute(
                "INSERT INTO missing_table(value) VALUES (?1)",
                params![1_i64],
            )
        });
        assert!(inner.is_err());
        connection.execute("INSERT INTO logs(body) VALUES (?1)", params!["after"])?;
        Ok(())
    })
    .unwrap();

    let bodies = Db::query(|connection| {
        connection.query_column::<String>("SELECT body FROM logs ORDER BY id", params![])
    })
    .unwrap();
    assert_eq!(bodies, vec!["outer".to_string(), "after".to_string()]);
}
