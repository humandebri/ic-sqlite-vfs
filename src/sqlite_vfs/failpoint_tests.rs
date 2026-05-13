//! Failpoint tests for stable-memory publish behavior.
//!
//! These tests live inside the crate so low-level stable blob mutation helpers
//! stay unavailable to external users.

use crate::db::migrate::Migration;
use crate::db::statement::{self, StepFailpoint};
use crate::sqlite_vfs::stable_blob::{self, StableBlobFailpoint};
use crate::sqlite_vfs::{lock, stable_blob as blob};
use crate::stable::memory::{self, MemoryFailpoint};
use crate::stable::meta::Superblock;
use crate::{Db, DbError};
use serial_test::serial;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, Eq, PartialEq)]
struct Snapshot {
    db_size: u64,
    last_tx_id: u64,
    checksum: u64,
    image: Vec<u8>,
}

fn reset() {
    stable_blob::clear_failpoint();
    stable_blob::rollback_update();
    statement::clear_step_failpoint();
    memory::reset_for_tests();
    lock::reset_for_tests();
}

fn restart_after_failed_message(snapshot: Vec<u8>) {
    stable_blob::clear_failpoint();
    stable_blob::rollback_update();
    statement::clear_step_failpoint();
    memory::clear_failpoint();
    memory::restore_for_tests(snapshot);
    lock::reset_for_tests();
    Db::init().unwrap();
}

fn seed() {
    Db::migrate(&[Migration {
        version: 1,
        sql: "CREATE TABLE durable(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    }])
    .unwrap();
    Db::update(|connection| {
        connection.execute_batch("INSERT INTO durable(k, v) VALUES ('before', 'stable')")
    })
    .unwrap();
}

fn snapshot() -> Snapshot {
    let block = Superblock::load().unwrap();
    Snapshot {
        db_size: block.db_size,
        last_tx_id: block.last_tx_id,
        checksum: Db::db_checksum().unwrap(),
        image: Db::export_chunk(0, block.db_size).unwrap(),
    }
}

fn assert_database_unchanged(before: &Snapshot) {
    assert_eq!(&snapshot(), before);
    assert_eq!(Db::integrity_check().unwrap(), "ok");
    let count =
        Db::query(|connection| connection.query_i64("SELECT COUNT(*) FROM durable")).unwrap();
    assert_eq!(count, 1);
}

#[test]
#[serial]
fn failed_overlay_write_keeps_active_image_unchanged() {
    reset();
    seed();
    let before = snapshot();

    stable_blob::set_failpoint(StableBlobFailpoint::BeforeOverlayWrite);
    let result = Db::update(|connection| {
        connection.execute_batch("INSERT INTO durable(k, v) VALUES ('after', 'lost')")
    });

    assert!(result.is_err());
    assert_database_unchanged(&before);
}

#[test]
#[serial]
fn failed_overlay_truncate_keeps_active_image_unchanged() {
    reset();
    blob::write_at(0, b"abcdef").unwrap();
    let before = blob::export_chunk(0, 6).unwrap();

    stable_blob::begin_update().unwrap();
    stable_blob::set_failpoint(StableBlobFailpoint::BeforeOverlayTruncate);
    assert!(blob::truncate(2).is_err());
    stable_blob::rollback_update();

    assert_eq!(blob::export_chunk(0, 6).unwrap(), before);
}

#[test]
#[serial]
fn sparse_extend_zero_fills_gap_after_truncate() {
    reset();
    blob::write_at(0, b"abcd").unwrap();
    blob::truncate(1).unwrap();
    blob::write_at(3, b"z").unwrap();

    let image = blob::export_chunk(0, 4).unwrap();
    assert_eq!(image, b"a\0\0z");
}

#[test]
#[serial]
fn failed_commit_capacity_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::BeforeCommitCapacity);
}

#[test]
#[serial]
fn failed_commit_chunk_write_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::BeforeCommitChunkWrite);
}

#[test]
#[serial]
fn failed_commit_superblock_store_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::BeforeCommitSuperblockStore);
}

fn assert_commit_failpoint_preserves_snapshot(failpoint: StableBlobFailpoint) {
    reset();
    seed();
    let before = snapshot();

    stable_blob::set_failpoint(failpoint);
    let result = Db::update(|connection| {
        connection.execute_batch("INSERT INTO durable(k, v) VALUES ('after', 'shadow')")
    });

    assert!(result.is_err());
    assert_database_unchanged(&before);
}

#[test]
#[serial]
fn stable_write_trap_ordinals_preserve_active_image_after_restart() {
    for ordinal in [1_u64, 2] {
        reset();
        seed();
        let before = snapshot();
        let memory_before = memory::snapshot_for_tests();

        memory::set_failpoint(MemoryFailpoint::TrapAfterWrite { ordinal });
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = Db::update(|connection| {
                connection.execute(
                    "INSERT INTO durable(k, v) VALUES (?1, ?2)",
                    &[&"after", &"trap"],
                )
            });
        }));

        assert!(result.is_err());
        restart_after_failed_message(memory_before);
        assert_database_unchanged(&before);
    }
}

#[test]
#[serial]
fn stable_grow_failure_keeps_active_image_unchanged() {
    reset();
    seed();
    let before = snapshot();

    memory::set_failpoint(MemoryFailpoint::GrowFailed { ordinal: 1 });
    let result = Db::update(|connection| {
        connection.execute(
            "INSERT INTO durable(k, v) VALUES (?1, ?2)",
            &[&"after", &"grow"],
        )
    });

    assert!(result.is_err());
    assert_database_unchanged(&before);
}

#[test]
#[serial]
fn sqlite_step_error_rolls_back_without_changing_active_image() {
    reset();
    seed();
    let before = snapshot();

    statement::set_step_failpoint(StepFailpoint {
        ordinal: 1,
        code: crate::sqlite_vfs::ffi::SQLITE_IOERR,
    });
    let result = Db::update(|connection| {
        connection.execute(
            "INSERT INTO durable(k, v) VALUES (?1, ?2)",
            &[&"after", &"step"],
        )
    });

    assert!(result.is_err());
    assert_database_unchanged(&before);
}

#[test]
#[serial]
fn panic_inside_update_drops_overlay_without_changing_active_image() {
    reset();
    seed();
    let before = snapshot();

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Db::update(|connection| -> Result<(), DbError> {
            connection.execute_batch("INSERT INTO durable(k, v) VALUES ('panic', 'shadow')")?;
            panic!("fail after sqlite write");
        });
    }));

    assert!(result.is_err());
    assert_database_unchanged(&before);
}
