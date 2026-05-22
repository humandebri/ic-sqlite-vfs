#![no_main]

use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::sqlite_vfs::{lock, stable_blob};
use ic_sqlite_vfs::stable::memory;
use ic_sqlite_vfs::stable::meta::Superblock;
use ic_sqlite_vfs::{params, Db};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE fuzz_state(k INTEGER PRIMARY KEY, v INTEGER NOT NULL);",
}];

fuzz_target!(|data: &[u8]| {
    reset();
    Db::migrate(MIGRATIONS).expect("migration should succeed");

    let mut model = BTreeMap::<i64, i64>::new();
    for chunk in data.chunks(3).take(128) {
        let op = chunk.first().copied().unwrap_or(0) % 5;
        let key = i64::from(chunk.get(1).copied().unwrap_or(0) % 32);
        let value = i64::from(chunk.get(2).copied().unwrap_or(0)) - 128;
        match op {
            0 => {
                model.insert(key, value);
                Db::update(|connection| {
                    connection.execute(
                        "INSERT INTO fuzz_state(k, v) VALUES (?1, ?2)
                         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                        params![key, value],
                    )
                })
                .expect("upsert should succeed");
            }
            1 => {
                model.remove(&key);
                Db::update(|connection| {
                    connection.execute("DELETE FROM fuzz_state WHERE k = ?1", params![key])
                })
                .expect("delete should succeed");
            }
            2 => {
                if let Some(stored) = model.get_mut(&key) {
                    *stored = stored.saturating_add(value);
                }
                Db::update(|connection| {
                    connection.execute(
                        "UPDATE fuzz_state SET v = v + ?1 WHERE k = ?2",
                        params![value, key],
                    )
                })
                .expect("update should succeed");
            }
            3 => {
                let before = read_rows();
                Db::update(|connection| {
                    connection.execute(
                        "INSERT INTO fuzz_state(k, v) VALUES (?1, ?2)",
                        params![key, value],
                    )?;
                    connection.execute(
                        "INSERT INTO fuzz_state(k, v) VALUES (?1, ?2)",
                        params![key, value.saturating_add(1)],
                    )
                })
                .expect_err("duplicate insert should rollback");
                assert_eq!(read_rows(), before);
            }
            _ => {
                let before = read_rows();
                Db::refresh_checksum().expect("checksum refresh should succeed");
                assert_eq!(read_rows(), before);
            }
        }
        assert_eq!(read_rows(), model);
        assert_eq!(
            Db::integrity_check().expect("integrity check should run"),
            "ok"
        );
    }

    Db::compact().expect("compact should succeed");
    assert_eq!(read_rows(), model);

    let db_size = Superblock::load().expect("superblock should load").db_size;
    let checksum = Db::refresh_checksum().expect("checksum refresh should succeed");
    let image = Db::export_chunk(0, db_size).expect("export should succeed");
    Db::begin_import(db_size, checksum).expect("import begin should succeed");
    Db::import_chunk(0, &image).expect("import chunk should succeed");
    Db::finish_import().expect("import finish should succeed");
    assert_eq!(read_rows(), model);
    assert_eq!(
        Db::integrity_check().expect("integrity check should run"),
        "ok"
    );
});

fn reset() {
    stable_blob::invalidate_read_cache();
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).expect("test memory should initialize");
}

fn read_rows() -> BTreeMap<i64, i64> {
    let rows = Db::query(|connection| {
        connection.query_map::<(i64, i64), _>(
            "SELECT k, v FROM fuzz_state ORDER BY k",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    })
    .expect("query should succeed");
    rows.into_iter().collect()
}
