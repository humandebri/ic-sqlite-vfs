use super::logical::page_count_for_size;
use crate::sqlite_vfs::overlay::Overlay;
use crate::stable::memory::StableMemoryError;
use crate::stable::meta::{Superblock, ZeroExtent, MAX_ZERO_EXTENTS};

pub(super) fn zero_extents_after_commit(
    block: &Superblock,
    overlay: &Overlay,
    final_page_count: u64,
) -> Result<Vec<ZeroExtent>, StableMemoryError> {
    let mut extents = block.zero_extents().to_vec();
    let old_page_count = page_count_for_size(block.db_size)?;
    let dirty_page_slack = overlay
        .dirty_pages()
        .iter()
        .filter(|(page_no, _)| *page_no < final_page_count)
        .count();
    let temporary_limit = MAX_ZERO_EXTENTS.checked_add(dirty_page_slack).ok_or(
        StableMemoryError::ZeroExtentLimitExceeded {
            limit: MAX_ZERO_EXTENTS,
        },
    )?;
    // A commit can temporarily exceed MAX_ZERO_EXTENTS while combining growth,
    // truncate masks, and dirty-page materialization. Bound that temporary
    // slack to pages that can actually remove zero-mask metadata later.
    if overlay.size() > block.db_size {
        let first_new_page = page_count_for_size(block.db_size)?;
        add_zero_extent_without_limit(&mut extents, first_new_page, final_page_count);
        enforce_temporary_zero_extent_limit(&extents, temporary_limit)?;
    }
    if overlay.size() < block.db_size {
        let first_zero_page = page_count_for_size(overlay.size())?;
        add_zero_extent_without_limit(&mut extents, first_zero_page, old_page_count);
        enforce_temporary_zero_extent_limit(&extents, temporary_limit)?;
    }
    for extent in overlay.zero_extents() {
        add_zero_extent_without_limit(&mut extents, extent.start_page, extent.end_page);
        enforce_temporary_zero_extent_limit(&extents, temporary_limit)?;
    }
    for (page_no, _) in overlay.dirty_pages() {
        if *page_no < final_page_count {
            subtract_zero_extent_without_limit(&mut extents, *page_no, page_no.saturating_add(1));
        }
    }
    enforce_zero_extent_limit(&extents)?;
    Ok(extents)
}

pub(super) fn enforce_temporary_zero_extent_limit(
    extents: &[ZeroExtent],
    temporary_limit: usize,
) -> Result<(), StableMemoryError> {
    if extents.len() > temporary_limit {
        return Err(StableMemoryError::ZeroExtentLimitExceeded {
            limit: MAX_ZERO_EXTENTS,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn add_zero_extent(
    extents: &mut Vec<ZeroExtent>,
    start_page: u64,
    end_page: u64,
) -> Result<(), StableMemoryError> {
    if start_page >= end_page {
        return Ok(());
    }
    extents.push(ZeroExtent {
        start_page,
        end_page,
    });
    normalize_zero_extents(extents)
}

fn add_zero_extent_without_limit(extents: &mut Vec<ZeroExtent>, start_page: u64, end_page: u64) {
    if start_page >= end_page {
        return;
    }
    extents.push(ZeroExtent {
        start_page,
        end_page,
    });
    normalize_zero_extents_without_limit(extents);
}

#[cfg(test)]
pub(super) fn subtract_zero_extent(
    extents: &mut Vec<ZeroExtent>,
    start_page: u64,
    end_page: u64,
) -> Result<(), StableMemoryError> {
    if start_page >= end_page || extents.is_empty() {
        return Ok(());
    }
    let mut next = Vec::with_capacity(extents.len() + 1);
    for extent in extents.iter() {
        if end_page <= extent.start_page || start_page >= extent.end_page {
            next.push(extent.clone());
            continue;
        }
        if extent.start_page < start_page {
            next.push(ZeroExtent {
                start_page: extent.start_page,
                end_page: start_page,
            });
        }
        if end_page < extent.end_page {
            next.push(ZeroExtent {
                start_page: end_page,
                end_page: extent.end_page,
            });
        }
    }
    *extents = next;
    normalize_zero_extents(extents)
}

fn subtract_zero_extent_without_limit(
    extents: &mut Vec<ZeroExtent>,
    start_page: u64,
    end_page: u64,
) {
    if start_page >= end_page || extents.is_empty() {
        return;
    }
    let mut next = Vec::with_capacity(extents.len() + 1);
    for extent in extents.iter() {
        if end_page <= extent.start_page || start_page >= extent.end_page {
            next.push(extent.clone());
            continue;
        }
        if extent.start_page < start_page {
            next.push(ZeroExtent {
                start_page: extent.start_page,
                end_page: start_page,
            });
        }
        if end_page < extent.end_page {
            next.push(ZeroExtent {
                start_page: end_page,
                end_page: extent.end_page,
            });
        }
    }
    *extents = next;
    normalize_zero_extents_without_limit(extents);
}

#[cfg(test)]
pub(super) fn normalize_zero_extents(
    extents: &mut Vec<ZeroExtent>,
) -> Result<(), StableMemoryError> {
    normalize_zero_extents_without_limit(extents);
    enforce_zero_extent_limit(extents)
}

fn normalize_zero_extents_without_limit(extents: &mut Vec<ZeroExtent>) {
    extents.retain(|extent| extent.start_page < extent.end_page);
    extents.sort_by_key(|extent| extent.start_page);
    let mut normalized: Vec<ZeroExtent> = Vec::with_capacity(extents.len());
    for extent in extents.drain(..) {
        if let Some(last) = normalized.last_mut() {
            if extent.start_page <= last.end_page {
                last.end_page = last.end_page.max(extent.end_page);
                continue;
            }
        }
        normalized.push(extent);
    }
    *extents = normalized;
}

fn enforce_zero_extent_limit(extents: &[ZeroExtent]) -> Result<(), StableMemoryError> {
    if extents.len() > MAX_ZERO_EXTENTS {
        return Err(StableMemoryError::ZeroExtentLimitExceeded {
            limit: MAX_ZERO_EXTENTS,
        });
    }
    Ok(())
}
