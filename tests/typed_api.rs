use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::{DbError, NULL};
use ic_sqlite_vfs::sqlite_vfs::lock;
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::Db;
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
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
            &[&1_i64, &"alpha", &42_i64, &3.5_f64, &blob, &NULL],
        )
    })
    .unwrap();

    let row = Db::query(|connection| {
        connection.query_one(
            "SELECT text_value, integer_value, real_value, blob_value, null_value
             FROM typed_values WHERE id = ?1",
            &[&1_i64],
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
        connection.query_string("SELECT body FROM text_values WHERE id = 1")
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
        statement.execute(&[&1_i64, &"one"])?;
        statement.execute(&[&2_i64, &"two"])?;
        Ok(())
    })
    .unwrap();

    let names = Db::query(|connection| {
        connection.query_all("SELECT name FROM items ORDER BY id", &[], |row| {
            row.get::<String>(0)
        })
    })
    .unwrap();
    assert_eq!(names, vec!["one".to_string(), "two".to_string()]);

    let exists = Db::query(|connection| {
        connection.exists(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id = ?1)",
            &[&2_i64],
        )
    })
    .unwrap();
    assert!(exists);

    let optional = Db::query(|connection| {
        connection.query_optional("SELECT name FROM items WHERE id = ?1", &[&3_i64], |row| {
            row.get::<String>(0)
        })
    })
    .unwrap();
    assert_eq!(optional, None);

    let missing = Db::query(|connection| {
        connection.query_one("SELECT name FROM items WHERE id = ?1", &[&3_i64], |row| {
            row.get::<String>(0)
        })
    });
    assert!(matches!(missing, Err(DbError::NotFound)));
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
            &[(":id", &1_i64), ("@name", &"named"), ("$score", &2.5_f64)],
        )
    })
    .unwrap();

    let row = Db::query(|connection| {
        connection.query_one_named(
            "SELECT name, score FROM named_items WHERE id = :id",
            &[(":id", &1_i64)],
            |row| Ok((row.get::<String>(0)?, row.get::<f64>(1)?)),
        )
    })
    .unwrap();
    assert_eq!(row, ("named".to_string(), 2.5));

    let missing = Db::query(|connection| {
        connection.query_optional_named(
            "SELECT name FROM named_items WHERE id = :id",
            &[(":id", &2_i64)],
            |row| row.get::<String>(0),
        )
    })
    .unwrap();
    assert_eq!(missing, None);

    let error = Db::query(|connection| {
        connection.query_one_named(
            "SELECT name FROM named_items WHERE id = :id",
            &[(":missing", &1_i64)],
            |row| row.get::<String>(0),
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
            &[&1_i64, &"a"],
        )?;
        connection.execute(
            "INSERT INTO iter_items(id, name) VALUES (?1, ?2)",
            &[&2_i64, &"b"],
        )
    })
    .unwrap();

    let values = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT name FROM iter_items ORDER BY id")?;
        let mut rows = statement.query(&[])?;
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            values.push(row.get::<String>(0)?);
        }
        Ok(values)
    })
    .unwrap();

    assert_eq!(values, vec!["a".to_string(), "b".to_string()]);
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
        connection.execute("INSERT INTO unique_items(name) VALUES (?1)", &[&"same"])?;
        connection.execute("INSERT INTO unique_items(name) VALUES (?1)", &[&"same"])
    });
    assert!(matches!(duplicate, Err(DbError::Constraint(_))));

    let mismatch =
        Db::query(|connection| connection.query_one("SELECT 1", &[], |row| row.get::<String>(0)));
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
        connection.execute("INSERT INTO logs(body) VALUES (?1)", &[&"outer"])?;
        let inner = connection.savepoint(|connection| {
            connection.execute("INSERT INTO logs(body) VALUES (?1)", &[&"inner"])?;
            connection.execute("INSERT INTO missing_table(value) VALUES (?1)", &[&1_i64])
        });
        assert!(inner.is_err());
        connection.execute("INSERT INTO logs(body) VALUES (?1)", &[&"after"])?;
        Ok(())
    })
    .unwrap();

    let bodies = Db::query(|connection| {
        connection.query_all("SELECT body FROM logs ORDER BY id", &[], |row| {
            row.get::<String>(0)
        })
    })
    .unwrap();
    assert_eq!(bodies, vec!["outer".to_string(), "after".to_string()]);
}
