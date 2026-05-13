use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::sqlite_vfs::lock;
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::stable::meta::Superblock;
use ic_sqlite_vfs::Db;
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
}

#[test]
#[serial]
fn persists_rows_after_reopen() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch("INSERT INTO users(name) VALUES ('alice')")?;
        Ok(())
    })
    .unwrap();

    let name =
        Db::query(|connection| connection.query_string("SELECT name FROM users WHERE id = 1"))
            .unwrap();

    assert_eq!(name, "alice");
    assert!(Superblock::load().unwrap().db_size > 0);
}

#[test]
#[serial]
fn reusable_statement_handles_repeated_binds() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO kv(k, v) VALUES (?1, ?2)")?;
        for index in 0..16 {
            let key = format!("k{index}");
            let value = format!("v{index}");
            statement.execute_with_texts(&[key.as_str(), value.as_str()])?;
        }
        Ok(())
    })
    .unwrap();

    let joined = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT v FROM kv WHERE k = ?1")?;
        let mut values = Vec::new();
        for index in [0, 7, 15] {
            let key = format!("k{index}");
            values.push(statement.query_optional_string_with_text(&key)?.unwrap());
        }
        Ok(values.join(","))
    })
    .unwrap();

    assert_eq!(joined, "v0,v7,v15");
}

#[test]
#[serial]
fn query_connection_rejects_writes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE items(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let result = Db::query(|connection| {
        connection.execute_batch("INSERT INTO items DEFAULT VALUES")?;
        Ok(())
    });

    assert!(result.is_err());
}

#[test]
#[serial]
fn export_import_roundtrip_restores_database_image() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO kv(k, v) VALUES ('answer', '42')")?;
        Ok(())
    })
    .unwrap();

    let db_size = Superblock::load().unwrap().db_size;
    let checksum = Db::refresh_checksum().unwrap();
    let image = Db::export_chunk(0, db_size).unwrap();

    reset();
    Db::begin_import(db_size, checksum).unwrap();
    Db::import_chunk(0, &image).unwrap();
    Db::finish_import().unwrap();
    let block = Superblock::load().unwrap();
    assert_eq!(block.checksum, checksum);
    assert!(!block.is_checksum_stale());

    let value =
        Db::query(|connection| connection.query_string("SELECT v FROM kv WHERE k = 'answer'"))
            .unwrap();

    assert_eq!(value, "42");
}

#[test]
#[serial]
fn failed_update_rolls_back_transaction() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE logs(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    }])
    .unwrap();

    let result = Db::update(|connection| {
        connection.execute_batch("INSERT INTO logs(body) VALUES ('before-error')")?;
        connection.execute_batch("INSERT INTO missing_table(value) VALUES (1)")?;
        Ok(())
    });

    assert!(result.is_err());
    let count = Db::query(|connection| connection.query_i64("SELECT COUNT(*) FROM logs")).unwrap();
    assert_eq!(count, 0);
}

#[test]
#[serial]
fn integrity_check_reports_ok() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE checks(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

#[test]
#[serial]
fn import_rejects_checksum_mismatch() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE import_check(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let db_size = Superblock::load().unwrap().db_size;
    let image = Db::export_chunk(0, db_size).unwrap();

    reset();
    Db::begin_import(db_size, 123).unwrap();
    Db::import_chunk(0, &image).unwrap();

    assert!(Db::finish_import().is_err());
}

#[test]
#[serial]
fn failed_import_preserves_existing_database() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE preserved(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO preserved(k, v) VALUES ('key', 'value')")
    })
    .unwrap();
    let stale_before_import = Superblock::load().unwrap().is_checksum_stale();

    let db_size = Superblock::load().unwrap().db_size;
    let image = Db::export_chunk(0, db_size).unwrap();
    Db::begin_import(db_size, 123).unwrap();
    Db::import_chunk(0, &image).unwrap();

    assert!(Db::finish_import().is_err());
    let value =
        Db::query(|connection| connection.query_string("SELECT v FROM preserved WHERE k = 'key'"))
            .unwrap();
    assert_eq!(value, "value");
    let block = Superblock::load().unwrap();
    assert!(!block.is_importing());
    assert_eq!(block.is_checksum_stale(), stale_before_import);
}

#[test]
#[serial]
fn import_requires_one_sequential_session() {
    reset();
    Db::begin_import(4, 0).unwrap();

    assert!(Db::begin_import(4, 0).is_err());
    assert!(Db::import_chunk(2, b"cd").is_err());
    Db::import_chunk(0, b"ab").unwrap();
    Db::import_chunk(2, b"cd").unwrap();
}

#[test]
#[serial]
fn import_rejects_physical_offset_overflow() {
    reset();

    assert!(Db::begin_import(u64::MAX, 0).is_err());
}

#[test]
#[serial]
fn attached_path_containing_vfs_name_stays_separate() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE main_table(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch(
            "ATTACH DATABASE '/tmp/not-icstable-aux.db' AS aux;
             CREATE TABLE aux.attached_only(id INTEGER PRIMARY KEY);",
        )
    })
    .unwrap();
    let exists = Db::query(|connection| {
        connection.query_i64(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'attached_only'",
        )
    })
    .unwrap();

    assert_eq!(exists, 0);
}
