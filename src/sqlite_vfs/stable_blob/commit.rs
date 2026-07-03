use super::logical::{checked_add, page_count_for_size, page_physical_offset, page_size};
use super::state::{hit_failpoint, StableBlobFailpoint};
use super::zero_extents::zero_extents_after_commit;
use crate::profiling::{self, ProfileTimer};
use crate::sqlite_vfs::overlay::Overlay;
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{Superblock, ZeroExtent};

pub(super) fn commit_overlay(overlay: Overlay, advance_tx: bool) -> Result<(), StableMemoryError> {
    hit_failpoint(StableBlobFailpoint::CommitCapacity)?;
    let profile_enabled = profiling::metrics_enabled();
    let profile_start = ProfileTimer::start_if(profile_enabled);
    let block = Superblock::load()?;
    profiling::record_commit_load(profile_start);
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
    let profile_start = ProfileTimer::start_if(profile_enabled);
    memory::ensure_capacity(required_end)?;
    profiling::record_commit_capacity(profile_start);

    let profile_start = ProfileTimer::start_if(profile_enabled);
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
    profiling::record_commit_page_write(profile_start);

    // docs/API_STABILITY.md defines this as trap-for-rollback durability:
    // after dirty pages are written, publish failures must remain panics so IC
    // message rollback discards all stable-memory writes from this execution.
    if let Err(error) = hit_failpoint(StableBlobFailpoint::CommitSuperblockStore) {
        panic!("failed to publish in-place commit after page writes: {error}");
    }
    let profile_start = ProfileTimer::start_if(profile_enabled);
    let result =
        store_commit_db_image(advance_tx, block.db_base_offset, overlay_size, zero_extents);
    profiling::record_commit_superblock_store(profile_start);
    // MUST NOT become a recoverable error; recovering here would expose a
    // partial in-place commit instead of relying on the documented trap rollback.
    if let Err(error) = result {
        panic!("failed to publish in-place commit after page writes: {error}");
    }
    Ok(())
}

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
