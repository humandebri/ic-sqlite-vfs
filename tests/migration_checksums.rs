use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::test_support::{lock, memory, Superblock};
use ic_sqlite_vfs::{params, Db, DbError, DbHandle, DefaultMemoryImpl, MemoryId, MemoryManager};
use serial_test::serial;

const ORIGINAL: Migration = Migration {
    version: 1,
    sql: "CREATE TABLE data(id INTEGER PRIMARY KEY);",
};
const CHANGED: Migration = Migration {
    version: 1,
    sql: "CREATE TABLE data(id INTEGER PRIMARY KEY); -- changed",
};

fn reset() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    Db::init(memory::memory_for_tests()).unwrap();
}

fn mismatch(result: Result<(), DbError>) {
    assert!(matches!(result, Err(DbError::Sqlite(code, message))
        if code == 19 && message.contains("migration checksum mismatch for version 1")));
}

fn checksums() -> i64 {
    Db::query(|c| {
        c.query_scalar(
            "SELECT count(*) FROM __ic_sqlite_migration_checksums",
            params![],
        )
    })
    .unwrap()
}

#[test]
#[serial]
fn fresh_history_records_checksum_and_repeated_sql_does_not_reexecute() {
    reset();
    Db::migrate(&[ORIGINAL]).unwrap();
    let stored = Db::query(|c| {
        c.query_scalar::<Vec<u8>>(
            "SELECT checksum FROM __ic_sqlite_migration_checksums WHERE version=1",
            params![],
        )
    })
    .unwrap();
    assert_eq!(stored.len(), 32);
    Db::migrate(&[ORIGINAL]).unwrap();
    assert_eq!(checksums(), 1);
    assert_eq!(Superblock::load().unwrap().schema_version, 1);
}

#[test]
#[serial]
fn changed_sql_is_rejected_before_any_pending_sql_and_changes_no_bytes() {
    reset();
    Db::migrate(&[ORIGINAL]).unwrap();
    let before = memory::snapshot_for_tests();
    mismatch(Db::migrate(&[
        Migration {
            version: 0,
            sql: "invalid SQL must never execute",
        },
        CHANGED,
        Migration {
            version: 2,
            sql: "CREATE TABLE unexpected(id INTEGER)",
        },
    ]));
    assert_eq!(memory::snapshot_for_tests(), before);
    assert_eq!(Superblock::load().unwrap().schema_version, 1);
    Db::migrate(&[ORIGINAL]).unwrap();
}

fn legacy_history() {
    Db::update(|c| c.execute_batch("CREATE TABLE __ic_sqlite_migrations(version INTEGER PRIMARY KEY NOT NULL); INSERT INTO __ic_sqlite_migrations VALUES(1); CREATE TABLE data(id INTEGER PRIMARY KEY)")).unwrap();
}

#[test]
#[serial]
fn legacy_versions_remain_unverified_and_new_versions_get_checksums() {
    reset();
    legacy_history();
    Db::migrate(&[
        CHANGED,
        Migration {
            version: 2,
            sql: "ALTER TABLE data ADD COLUMN body TEXT",
        },
    ])
    .unwrap();
    assert_eq!(checksums(), 1);
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>(
            "SELECT version FROM __ic_sqlite_migration_checksums",
            params![]
        ))
        .unwrap(),
        2
    );
    // It is not possible to infer which historical SQL ran from the version.
    Db::migrate(&[ORIGINAL]).unwrap();
    assert_eq!(checksums(), 1);
}

#[test]
#[serial]
fn explicit_adoption_never_executes_sql_and_never_overwrites_known_hashes() {
    reset();
    legacy_history();
    Db::adopt_migration_checksums(&[ORIGINAL]).unwrap();
    assert_eq!(checksums(), 1);
    Db::adopt_migration_checksums(&[ORIGINAL]).unwrap();
    let before = memory::snapshot_for_tests();
    mismatch(Db::adopt_migration_checksums(&[CHANGED]));
    assert_eq!(memory::snapshot_for_tests(), before);
    mismatch(Db::migrate(&[CHANGED]));
    assert!(matches!(Db::adopt_migration_checksums(&[
        ORIGINAL, Migration { version: 2, sql: "CREATE TABLE unexpected(x)" }
    ]), Err(DbError::Sqlite(19, message)) if message.contains("unapplied migration version 2")));
    assert_eq!(checksums(), 1);
}

#[test]
#[serial]
fn failed_adoption_does_not_partially_register_legacy_versions() {
    reset();
    legacy_history();
    assert!(Db::adopt_migration_checksums(&[
        ORIGINAL,
        Migration {
            version: 2,
            sql: "SELECT 1"
        },
    ])
    .is_err());
    assert_eq!(
        Db::query(|c| c.query_scalar::<i64>(
            "SELECT count(*) FROM sqlite_schema WHERE name='__ic_sqlite_migration_checksums'",
            params![]
        ))
        .unwrap(),
        0
    );
    Db::migrate(&[CHANGED]).unwrap();
    assert_eq!(checksums(), 0);
}

#[test]
#[serial]
fn failed_migration_rolls_back_sql_versions_and_checksums_together() {
    reset();
    assert!(Db::migrate(&[
        ORIGINAL,
        Migration {
            version: 2,
            sql: "CREATE TABLE broken("
        },
    ])
    .is_err());
    assert_eq!(Superblock::load().unwrap().schema_version, 0);
    assert_eq!(Db::query(|c| c.query_scalar::<i64>("SELECT count(*) FROM sqlite_schema WHERE name IN ('data','__ic_sqlite_migrations','__ic_sqlite_migration_checksums')", params![])).unwrap(), 0);
    Db::migrate(&[CHANGED]).unwrap();
    assert_eq!(checksums(), 1);
}

#[test]
#[serial]
fn database_handles_have_independent_migration_checksums() {
    memory::reset_for_tests();
    lock::reset_for_tests();
    let manager = MemoryManager::init(DefaultMemoryImpl::default());
    let a = DbHandle::init(manager.get(MemoryId::new(30))).unwrap();
    let b = DbHandle::init(manager.get(MemoryId::new(31))).unwrap();
    a.migrate(&[ORIGINAL]).unwrap();
    b.migrate(&[CHANGED]).unwrap();
    mismatch(a.migrate(&[CHANGED]));
    b.migrate(&[CHANGED]).unwrap();
    a.adopt_migration_checksums(&[ORIGINAL]).unwrap();
}
