use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::test_support::lock;
use ic_sqlite_vfs::test_support::memory;
use ic_sqlite_vfs::test_support::Superblock;
use ic_sqlite_vfs::{params, Db};
use serial_test::serial;

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

#[test]
#[serial]
fn committed_update_marks_checksum_stale_until_refresh() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE stale(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::refresh_checksum().unwrap();
    let before = Superblock::load().unwrap();
    assert!(!before.is_checksum_stale());

    Db::update(|connection| connection.execute_batch("INSERT INTO stale(k, v) VALUES ('k', 'v')"))
        .unwrap();
    let after_update = Superblock::load().unwrap();
    assert!(after_update.is_checksum_stale());
    assert_eq!(after_update.last_tx_id, before.last_tx_id + 1);
    assert_eq!(after_update.checksum, before.checksum);

    let current_checksum = Db::db_checksum().unwrap();
    assert!(Superblock::load().unwrap().is_checksum_stale());

    let refreshed = Db::refresh_checksum().unwrap();
    let after_refresh = Superblock::load().unwrap();
    assert_eq!(refreshed, current_checksum);
    assert_eq!(after_refresh.checksum, current_checksum);
    assert!(!after_refresh.is_checksum_stale());
}

#[test]
#[serial]
fn chunked_checksum_refresh_updates_metadata_only_when_complete() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE chunked(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::refresh_checksum().unwrap();
    let before = Superblock::load().unwrap();

    Db::update(|connection| {
        let mut statement = connection.prepare("INSERT INTO chunked(k, v) VALUES (?1, ?2)")?;
        for index in 0..32 {
            let key = format!("k{index}");
            let value = format!("value-{index}");
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let first = Db::refresh_checksum_chunk(64).unwrap();
    assert!(!first.complete);
    assert_eq!(first.scanned_bytes, 64);
    let partial = Superblock::load().unwrap();
    assert!(partial.is_checksum_stale());
    assert!(partial.is_checksum_refreshing());
    assert_eq!(partial.checksum, before.checksum);

    let mut latest = first;
    while !latest.complete {
        latest = Db::refresh_checksum_chunk(64).unwrap();
    }

    let after = Superblock::load().unwrap();
    assert_eq!(after.checksum, Db::db_checksum().unwrap());
    assert_eq!(latest.checksum, after.checksum);
    assert_eq!(latest.scanned_bytes, latest.db_size);
    assert!(!after.is_checksum_stale());
    assert!(!after.is_checksum_refreshing());
}

#[test]
#[serial]
fn chunked_checksum_refresh_matches_one_shot_for_varied_chunk_sizes() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE chunk_variants(k TEXT PRIMARY KEY, v BLOB NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        let mut statement =
            connection.prepare("INSERT INTO chunk_variants(k, v) VALUES (?1, ?2)")?;
        for index in 0..96 {
            let key = format!("k{index}");
            let value: Vec<u8> = (0..257)
                .map(|offset| (index as u8).wrapping_add(offset as u8).wrapping_mul(19))
                .collect();
            statement.execute(params![key, value])?;
        }
        Ok(())
    })
    .unwrap();

    let expected = Db::db_checksum().unwrap();
    for chunk_size in [1_u64, 7, 64, 4096, 16 * 1024, 20 * 1024] {
        let mut latest = Db::refresh_checksum_chunk(chunk_size).unwrap();
        let mut guard = 0_u32;
        while !latest.complete {
            latest = Db::refresh_checksum_chunk(chunk_size).unwrap();
            guard += 1;
            assert!(guard < 100_000);
        }
        assert_eq!(latest.checksum, expected);
        assert_eq!(latest.scanned_bytes, latest.db_size);
        assert_eq!(Superblock::load().unwrap().checksum, expected);
    }
}

#[test]
#[serial]
fn committed_update_clears_partial_checksum_refresh() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE refresh_reset(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();

    Db::update(|connection| {
        connection.execute_batch("INSERT INTO refresh_reset(k, v) VALUES ('a', 'b')")
    })
    .unwrap();
    let first = Db::refresh_checksum_chunk(64).unwrap();
    assert!(!first.complete);
    assert!(Superblock::load().unwrap().is_checksum_refreshing());

    Db::update(|connection| {
        connection.execute_batch("INSERT INTO refresh_reset(k, v) VALUES ('c', 'd')")
    })
    .unwrap();
    let after_update = Superblock::load().unwrap();
    assert!(after_update.is_checksum_stale());
    assert!(!after_update.is_checksum_refreshing());
    assert_eq!(after_update.checksum_refresh_offset, 0);
}

#[test]
#[serial]
fn checksum_refresh_rejects_empty_chunk_size() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE checksum_limit(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();

    assert!(Db::refresh_checksum_chunk(0).is_err());
}

#[test]
#[serial]
fn noop_update_does_not_mark_checksum_stale_or_advance_tx_id() {
    reset();
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE noop_update(id INTEGER PRIMARY KEY);",
    }])
    .unwrap();
    Db::refresh_checksum().unwrap();
    let before = Superblock::load().unwrap();

    Db::update(|_connection| Ok(())).unwrap();

    let after = Superblock::load().unwrap();
    assert_eq!(after.db_size, before.db_size);
    assert_eq!(after.last_tx_id, before.last_tx_id);
    assert_eq!(after.checksum, before.checksum);
    assert_eq!(after.is_checksum_stale(), before.is_checksum_stale());
}
