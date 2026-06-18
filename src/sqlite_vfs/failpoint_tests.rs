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
    Db::init(memory::memory_for_tests()).unwrap();
}

fn restart_after_failed_message(snapshot: Vec<u8>) {
    stable_blob::clear_failpoint();
    stable_blob::rollback_update();
    statement::clear_step_failpoint();
    memory::clear_failpoint();
    let memory = memory::restore_for_tests(snapshot);
    lock::reset_for_tests();
    Db::init(memory).unwrap();
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
    let count = Db::query(|connection| {
        connection.query_scalar::<i64>("SELECT COUNT(*) FROM durable", crate::params![])
    })
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[serial]
fn failed_overlay_write_keeps_active_image_unchanged() {
    reset();
    seed();
    let before = snapshot();

    stable_blob::set_failpoint(StableBlobFailpoint::OverlayWrite);
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
    stable_blob::set_failpoint(StableBlobFailpoint::OverlayTruncate);
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
fn sparse_extend_after_truncate_keeps_base_and_uses_zero_mask() {
    reset();
    blob::write_at(0, b"abcd").unwrap();
    let initial_base = Superblock::load().unwrap().db_base_offset;

    blob::truncate(1).unwrap();
    let truncated = Superblock::load().unwrap();
    assert_eq!(truncated.db_base_offset, initial_base);
    assert_eq!(truncated.zero_extent_count(), 0);

    let page_size = u64::from(crate::config::SQLITE_PAGE_SIZE);
    blob::write_at(page_size + 3, b"z").unwrap();
    let grown = Superblock::load().unwrap();
    let image = blob::export_chunk(0, page_size + 4).unwrap();

    assert_eq!(grown.db_base_offset, initial_base);
    assert_eq!(grown.zero_extent_count(), 0);
    assert_eq!(image[0], b'a');
    assert!(image[1..usize::try_from(page_size + 3).unwrap()]
        .iter()
        .all(|byte| *byte == 0));
    assert_eq!(image[usize::try_from(page_size + 3).unwrap()], b'z');
}

#[test]
#[serial]
fn grow_without_prior_truncate_keeps_existing_base() {
    reset();
    blob::write_at(0, b"a").unwrap();
    let initial_base = Superblock::load().unwrap().db_base_offset;

    blob::write_at(3, b"z").unwrap();
    let grown = Superblock::load().unwrap();
    let image = blob::export_chunk(0, 4).unwrap();

    assert_eq!(image, b"a\0\0z");
    assert_eq!(grown.db_base_offset, initial_base);
    assert_eq!(grown.zero_extent_count(), 0);
}

#[test]
#[serial]
fn zero_mask_grow_failure_keeps_active_image_unchanged() {
    reset();
    blob::write_at(0, b"abcd").unwrap();
    blob::truncate(1).unwrap();
    let before = blob::export_chunk(0, 1).unwrap();
    let block_before = Superblock::load().unwrap();

    memory::set_failpoint(MemoryFailpoint::GrowFailed { ordinal: 1 });
    let result = blob::write_at(300_000, b"z");

    assert!(result.is_err());
    memory::clear_failpoint();
    assert_eq!(blob::export_chunk(0, block_before.db_size).unwrap(), before);
    assert_eq!(Superblock::load().unwrap(), block_before);
}

#[test]
#[serial]
fn zero_mask_dirty_page_failure_keeps_active_image_unchanged() {
    reset();
    blob::write_at(0, b"abcd").unwrap();
    blob::truncate(1).unwrap();
    let before = blob::export_chunk(0, 1).unwrap();
    let block_before = Superblock::load().unwrap();

    stable_blob::set_failpoint(StableBlobFailpoint::CommitChunkWrite);
    let result = blob::write_at(3, b"z");

    assert!(result.is_err());
    assert_eq!(blob::export_chunk(0, block_before.db_size).unwrap(), before);
    assert_eq!(Superblock::load().unwrap(), block_before);
}

#[test]
#[serial]
fn zero_mask_superblock_store_failure_keeps_active_image_after_restart() {
    reset();
    blob::write_at(0, b"abcd").unwrap();
    blob::truncate(1).unwrap();
    let before = blob::export_chunk(0, 1).unwrap();
    let block_before = Superblock::load().unwrap();
    let memory_before = memory::snapshot_for_tests();

    stable_blob::set_failpoint(StableBlobFailpoint::CommitSuperblockStore);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = blob::write_at(3, b"z");
    }));

    assert!(result.is_err());
    restart_after_failed_message(memory_before);
    assert_eq!(blob::export_chunk(0, block_before.db_size).unwrap(), before);
    assert_eq!(Superblock::load().unwrap(), block_before);
}

#[test]
#[serial]
fn zero_mask_write_traps_preserve_active_image_after_restart() {
    for ordinal in [1_u64] {
        reset();
        blob::write_at(0, b"abcd").unwrap();
        blob::truncate(1).unwrap();
        let before = blob::export_chunk(0, 1).unwrap();
        let block_before = Superblock::load().unwrap();
        let memory_before = memory::snapshot_for_tests();

        memory::set_failpoint(MemoryFailpoint::TrapAfterWrite { ordinal });
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = blob::write_at(3, b"z");
        }));

        assert!(result.is_err());
        restart_after_failed_message(memory_before);
        assert_eq!(blob::export_chunk(0, block_before.db_size).unwrap(), before);
        assert_eq!(Superblock::load().unwrap(), block_before);
    }
}

#[test]
#[serial]
fn in_place_commit_reuses_existing_capacity_for_existing_pages() {
    reset();
    let segment_size = u64::from(crate::config::SQLITE_PAGE_SIZE) * 256;
    blob::write_at(0, b"a").unwrap();
    blob::write_at(segment_size, b"b").unwrap();
    let pages_after_seed = memory::size_pages();

    blob::write_at(7, b"x").unwrap();
    assert_eq!(memory::size_pages(), pages_after_seed);

    stable_blob::begin_update().unwrap();
    blob::write_at(8, b"y").unwrap();
    blob::write_at(segment_size + 8, b"z").unwrap();
    stable_blob::commit_update().unwrap();
    assert_eq!(memory::size_pages(), pages_after_seed);
}

#[test]
#[serial]
fn in_place_truncate_and_reextend_zero_fills_boundary_tail() {
    reset();
    let page_size = u64::from(crate::config::SQLITE_PAGE_SIZE);
    let segment_size = page_size * 256;
    blob::write_at(segment_size, b"a").unwrap();
    blob::write_at(segment_size + page_size, b"stale").unwrap();

    blob::truncate(segment_size + 1).unwrap();
    blob::write_at(segment_size + page_size + 2, b"n").unwrap();
    let image = blob::export_chunk(segment_size, page_size + 3).unwrap();
    assert_eq!(image[0], b'a');
    assert!(image[1..usize::try_from(page_size + 2).unwrap()]
        .iter()
        .all(|byte| *byte == 0));
    assert_eq!(image[usize::try_from(page_size + 2).unwrap()], b'n');

    blob::truncate(1).unwrap();
    assert_eq!(Superblock::load().unwrap().page_count, 1);
}

#[test]
#[serial]
fn failed_commit_capacity_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::CommitCapacity);
}

#[test]
#[serial]
fn failed_commit_chunk_write_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::CommitChunkWrite);
}

#[test]
#[serial]
fn failed_commit_superblock_store_keeps_active_image_unchanged() {
    assert_commit_failpoint_preserves_snapshot(StableBlobFailpoint::CommitSuperblockStore);
}

fn assert_commit_failpoint_preserves_snapshot(failpoint: StableBlobFailpoint) {
    reset();
    seed();
    let before = snapshot();
    let memory_before = memory::snapshot_for_tests();

    stable_blob::set_failpoint(failpoint);
    let result = catch_unwind(AssertUnwindSafe(|| {
        Db::update(|connection| {
            connection.execute_batch("INSERT INTO durable(k, v) VALUES ('after', 'shadow')")
        })
    }));

    if failpoint == StableBlobFailpoint::CommitSuperblockStore {
        assert!(result.is_err());
        restart_after_failed_message(memory_before);
    } else {
        assert!(result.unwrap().is_err());
    }
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
                    crate::params!["after", "trap"],
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
fn compact_is_noop_and_does_not_hit_write_failpoint() {
    reset();
    seed();
    let before = snapshot();

    memory::set_failpoint(MemoryFailpoint::TrapAfterWrite { ordinal: 1 });
    Db::compact().unwrap();
    memory::clear_failpoint();
    assert_database_unchanged(&before);
}

#[test]
#[serial]
fn stable_grow_failure_keeps_active_image_unchanged() {
    reset();
    seed();
    let before = snapshot();

    memory::set_failpoint(MemoryFailpoint::GrowFailed { ordinal: 1 });
    let large_value = "x".repeat(300_000);
    let result = Db::update(|connection| {
        connection.execute(
            "INSERT INTO durable(k, v) VALUES (?1, ?2)",
            crate::params!["after", large_value.as_str()],
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
            crate::params!["after", "step"],
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
