use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::db::{DbError, Value};
use ic_sqlite_vfs::sqlite_vfs::{lock, stable_blob};
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::{named_params, params, Db};
use serial_test::serial;

fn reset() {
    stable_blob::invalidate_read_cache();
    memory::reset_for_tests();
    lock::reset_for_tests();
}

#[test]
#[serial]
fn text_bind_preserves_embedded_nul_bytes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE text_binds(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute(
            "INSERT INTO text_binds(id, body) VALUES (?1, ?2)",
            params![1_i64, Value::Text("A\0B")],
        )
    })
    .unwrap();

    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT body FROM text_binds WHERE id = 1", params![])
    })
    .unwrap();
    assert_eq!(value.as_bytes(), b"A\0B");
}

#[test]
#[serial]
fn empty_blob_bind_stays_blob_not_null() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE blob_binds(id INTEGER PRIMARY KEY, body BLOB);",
    }])
    .unwrap();

    Db::update(|connection| {
        let empty: &[u8] = &[];
        connection.execute(
            "INSERT INTO blob_binds(id, body) VALUES (?1, ?2)",
            params![1_i64, empty],
        )
    })
    .unwrap();

    let row = Db::query(|connection| {
        connection.query_one(
            "SELECT body IS NOT NULL, length(body), body FROM blob_binds WHERE id = 1",
            params![],
            |row| {
                Ok((
                    row.get::<i64>(0)?,
                    row.get::<i64>(1)?,
                    row.get::<Vec<u8>>(2)?,
                ))
            },
        )
    })
    .unwrap();
    assert_eq!(row, (1, 0, Vec::new()));
}

#[test]
#[serial]
fn parameter_bindings_must_match_sql_parameters() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE strict_params(id INTEGER PRIMARY KEY, value TEXT);",
    }])
    .unwrap();

    let missing_positional = Db::update(|connection| {
        connection.execute(
            "INSERT INTO strict_params(id, value) VALUES (?1, ?2)",
            params![1_i64],
        )
    });
    assert!(matches!(
        missing_positional,
        Err(DbError::ParameterCountMismatch { .. })
    ));

    let extra_positional = Db::update(|connection| {
        connection.execute(
            "INSERT INTO strict_params(id) VALUES (?1)",
            params![1_i64, "extra"],
        )
    });
    assert!(matches!(
        extra_positional,
        Err(DbError::ParameterCountMismatch { .. })
    ));

    let missing_named = Db::update(|connection| {
        connection.execute_named(
            "INSERT INTO strict_params(id, value) VALUES (:id, :value)",
            named_params![":id" => 1_i64],
        )
    });
    assert!(matches!(
        missing_named,
        Err(DbError::ParameterCountMismatch { .. })
    ));

    let anonymous_named = Db::update(|connection| {
        connection.execute_named(
            "INSERT INTO strict_params(id, value) VALUES (:id, ?)",
            named_params![":id" => 1_i64, ":value" => "value"],
        )
    });
    assert!(matches!(
        anonymous_named,
        Err(DbError::AnonymousParameterInNamedBind { .. })
    ));
}

#[test]
#[serial]
fn prepare_rejects_empty_sql_and_trailing_statement_text() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE strict_sql(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let empty = Db::update(|connection| connection.execute("", params![]));
    assert!(matches!(empty, Err(DbError::EmptySql)));

    let trailing = Db::update(|connection| {
        connection.execute(
            "INSERT INTO strict_sql(id) VALUES (1); INSERT INTO strict_sql(id) VALUES (2)",
            params![],
        )
    });
    assert!(matches!(trailing, Err(DbError::TrailingSql)));
}
