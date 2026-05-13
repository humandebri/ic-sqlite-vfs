use ic_sqlite_vfs::config::SQLITE_CACHE_SIZE_KIB;
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

    let name = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT name FROM users WHERE id = 1", params![])
    })
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
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let joined = Db::query(|connection| {
        let mut statement = connection.prepare("SELECT v FROM kv WHERE k = ?1")?;
        let mut values = Vec::new();
        for index in [0, 7, 15] {
            let key = format!("k{index}");
            values.push(statement.query_optional_string_text(&key)?.unwrap());
        }
        assert!(statement.query_optional_string_text("missing")?.is_none());
        Ok(values.join(","))
    })
    .unwrap();

    assert_eq!(joined, "v0,v7,v15");
}

#[test]
#[serial]
fn prepared_statement_cache_reuses_sql_with_repeated_binds() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cached(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        {
            let mut statement =
                connection.prepare_cached("INSERT INTO cached(k, v) VALUES (?1, ?2)")?;
            statement.execute(params!["a", "one"])?;
        }
        {
            let mut statement =
                connection.prepare_cached("INSERT INTO cached(k, v) VALUES (?1, ?2)")?;
            statement.execute(params!["b", "two"])?;
        }
        Ok(())
    })
    .unwrap();

    let values = Db::query(|connection| {
        connection.query_column::<String>("SELECT v FROM cached ORDER BY k", params![])
    })
    .unwrap();
    assert_eq!(values, vec!["one".to_string(), "two".to_string()]);
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
fn read_only_connection_applies_cache_size_pragma() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cache_size_guard(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    let cache_size = Db::query(|connection| {
        connection.query_scalar::<i64>("PRAGMA cache_size", ic_sqlite_vfs::params![])
    })
    .unwrap();

    assert_eq!(cache_size, -i64::from(SQLITE_CACHE_SIZE_KIB));
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

    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM kv WHERE k = 'answer'", params![])
    })
    .unwrap();

    assert_eq!(value, "42");
}

#[test]
#[serial]
fn single_update_appends_less_than_full_database_image() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE growth(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO growth(k, v) VALUES (?1, ?2)")?;
        for index in 0..5_000 {
            let key = format!("k{index:08}");
            let value = format!("value-{index:08}");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let before = Superblock::load().unwrap();
    let before_pages = memory::size_pages();
    Db::update(|connection| {
        connection.execute(
            "UPDATE growth SET v = ?1 WHERE k = ?2",
            params!["updated", "k00000042"],
        )
    })
    .unwrap();
    let after_pages = memory::size_pages();
    let appended_bytes = after_pages
        .checked_sub(before_pages)
        .unwrap()
        .checked_mul(ic_sqlite_vfs::config::STABLE_PAGE_SIZE)
        .unwrap();

    assert!(before.db_size > ic_sqlite_vfs::config::STABLE_PAGE_SIZE);
    assert!(appended_bytes < before.db_size);
}

#[test]
#[serial]
fn compact_preserves_logical_database_contents() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE compacted(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO compacted(k, v) VALUES ('key', 'value')")
    })
    .unwrap();

    let checksum_before = Db::db_checksum().unwrap();
    Db::compact().unwrap();
    let checksum_after = Db::db_checksum().unwrap();
    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM compacted WHERE k = 'key'", params![])
    })
    .unwrap();

    assert_eq!(checksum_after, checksum_before);
    assert_eq!(value, "value");
    assert_eq!(Db::integrity_check().unwrap(), "ok");
}

#[test]
#[serial]
fn read_table_cache_is_invalidated_after_publish_paths() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE cache_guard(k TEXT PRIMARY KEY NOT NULL, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO cache_guard(k, v) VALUES ('key', 'before')")
    })
    .unwrap();

    let before = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(before, "before");

    Db::update(|connection| {
        connection.execute(
            "UPDATE cache_guard SET v = ?1 WHERE k = 'key'",
            params!["after"],
        )
    })
    .unwrap();
    let after_update = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(after_update, "after");

    let db_size = Superblock::load().unwrap().db_size;
    let checksum = Db::refresh_checksum().unwrap();
    let image = Db::export_chunk(0, db_size).unwrap();
    Db::begin_import(db_size, checksum).unwrap();
    Db::import_chunk(0, &image).unwrap();
    Db::finish_import().unwrap();
    let after_import = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(after_import, "after");

    Db::compact().unwrap();
    let after_compact = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM cache_guard WHERE k = 'key'", params![])
    })
    .unwrap();
    assert_eq!(after_compact, "after");
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
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM logs", params![])
    })
    .unwrap();
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
    let value = Db::query(|connection| {
        connection.query_scalar::<String>("SELECT v FROM preserved WHERE k = 'key'", params![])
    })
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
        connection.query_scalar::<i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'attached_only'",
            params![],
        )
    })
    .unwrap();

    assert_eq!(exists, 0);
}
