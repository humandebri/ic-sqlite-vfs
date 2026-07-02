use super::logical::{checked_add, page_count_for_size, page_physical_offset, page_size};
use super::state::{hit_failpoint, StableBlobFailpoint};
use super::zero_extents::zero_extents_after_commit;
use crate::sqlite_vfs::overlay::Overlay;
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{Superblock, ZeroExtent};

pub(super) fn commit_overlay(overlay: Overlay, advance_tx: bool) -> Result<(), StableMemoryError> {
    hit_failpoint(StableBlobFailpoint::CommitCapacity)?;
    let profile_enabled = commit_profile_enabled();
    let profile_start = commit_profile_start(profile_enabled);
    let block = Superblock::load()?;
    commit_profile_record_load(profile_start);
    commit_overlay_in_place(&block, overlay, advance_tx, profile_enabled)
}

fn commit_overlay_in_place(
    block: &Superblock,
    overlay: Overlay,
    advance_tx: bool,
    profile_enabled: bool,
) -> Result<(), StableMemoryError> {
    let overlay_size = overlay.size();
    let final_page_count = page_count_for_size(overlay_size)?;
    debug_assert!(overlay
        .dirty_pages()
        .iter()
        .all(|(page_no, _)| *page_no < final_page_count));
    let dirty_pages = overlay.dirty_pages();
    let zero_extents = zero_extents_after_commit(block, &overlay, final_page_count)?;

    let mut required_end = checked_add(block.db_base_offset, overlay_size)?;
    for (page_no, _) in dirty_pages {
        if *page_no >= final_page_count {
            continue;
        }
        required_end = required_end.max(checked_add(
            page_physical_offset(block, *page_no)?,
            page_size(),
        )?);
    }
    let profile_start = commit_profile_start(profile_enabled);
    memory::ensure_capacity(required_end)?;
    commit_profile_record_capacity(profile_start);

    let profile_start = commit_profile_start(profile_enabled);
    for (page_no, page) in dirty_pages {
        if *page_no >= final_page_count {
            continue;
        }
        hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
        write_commit_page(
            page_physical_offset(block, *page_no)?,
            page,
            profile_enabled,
        )?;
    }
    commit_profile_record_page_write(profile_start);

    if let Err(error) = hit_failpoint(StableBlobFailpoint::CommitSuperblockStore) {
        panic!("failed to publish in-place commit after page writes: {error}");
    }
    let profile_start = commit_profile_start(profile_enabled);
    let result =
        store_commit_db_image(advance_tx, block.db_base_offset, overlay_size, zero_extents);
    commit_profile_record_superblock_store(profile_start);
    if let Err(error) = result {
        panic!("failed to publish in-place commit after page writes: {error}");
    }
    Ok(())
}

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[inline(always)]
fn commit_profile_enabled() -> bool {
    crate::read_metrics::metrics_enabled()
}

#[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
#[inline(always)]
fn commit_profile_enabled() -> bool {
    false
}

#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[inline(always)]
fn commit_profile_start(enabled: bool) -> Option<u64> {
    if enabled {
        Some(crate::read_metrics::instruction_counter())
    } else {
        None
    }
}

#[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
#[inline(always)]
fn commit_profile_start(_enabled: bool) -> Option<u64> {
    None
}

macro_rules! commit_profile_recorder {
    ($name:ident, $record:ident) => {
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        #[inline(always)]
        fn $name(start: Option<u64>) {
            if let Some(start) = start {
                crate::read_metrics::$record(
                    crate::read_metrics::instruction_counter().saturating_sub(start),
                );
            }
        }

        #[cfg(not(any(test, debug_assertions, feature = "bench-profile")))]
        #[inline(always)]
        fn $name(_start: Option<u64>) {}
    };
}

commit_profile_recorder!(commit_profile_record_capacity, record_commit_capacity);
commit_profile_recorder!(commit_profile_record_load, record_commit_load);
commit_profile_recorder!(commit_profile_record_page_write, record_commit_page_write);
commit_profile_recorder!(
    commit_profile_record_superblock_store,
    record_commit_superblock_store
);

#[inline(always)]
fn write_commit_page(
    offset: u64,
    page: &[u8],
    profile_enabled: bool,
) -> Result<(), StableMemoryError> {
    if profile_enabled {
        memory::write_prechecked(offset, page)
    } else {
        memory::write_prechecked_unmetered(offset, page)
    }
}

fn store_commit_db_image(
    advance_tx: bool,
    db_base_offset: u64,
    overlay_size: u64,
    zero_extents: Vec<ZeroExtent>,
) -> Result<(), StableMemoryError> {
    match advance_tx {
        true => Superblock::commit_db_image(db_base_offset, overlay_size, zero_extents),
        false => Superblock::store_db_image_without_tx(db_base_offset, overlay_size, zero_extents),
    }
}
