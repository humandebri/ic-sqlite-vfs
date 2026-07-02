use super::commit::commit_overlay;
use super::logical::{
    active_page_count, checksum_logical_range, fold_fnv1a64, page_size, read_logical_range,
    read_logical_range_with_page_cache, zero_page,
};
#[cfg(test)]
use super::logical::{
    append_base, checked_add, checksum_physical_range, clear_import, import_offset,
    page_count_for_size,
};
use super::state::{
    hit_failpoint, ChecksumRefresh, PageOffsetCache, StableBlobFailpoint, StorageStats,
    CHECKSUM_CHUNK_LEN,
};
use crate::config::{STABLE_PAGE_SIZE, SUPERBLOCK_SIZE};
use crate::sqlite_vfs::overlay::{self, Overlay};
use crate::stable::memory::{self, StableMemoryError};
#[cfg(test)]
use crate::stable::meta::FLAG_IMPORTING;
use crate::stable::meta::{
    fnv1a64, Superblock, CURRENT_LAYOUT_VERSION, FLAG_CHECKSUM_REFRESHING, FLAG_CHECKSUM_STALE,
};

pub(crate) fn ensure_current_layout() -> Result<(), StableMemoryError> {
    let block = Superblock::load()?;
    if block.layout_version == CURRENT_LAYOUT_VERSION {
        return Ok(());
    }
    Err(StableMemoryError::UnsupportedLayoutVersion(
        block.layout_version,
    ))
}

pub(crate) fn begin_update() -> Result<u64, StableMemoryError> {
    let block = Superblock::load()?;
    if block.layout_version != CURRENT_LAYOUT_VERSION {
        return Err(StableMemoryError::UnsupportedLayoutVersion(
            block.layout_version,
        ));
    }
    if block.is_importing() {
        return Err(StableMemoryError::ImportAlreadyStarted);
    }
    overlay::begin(block.db_size)?;
    Ok(block.db_size)
}

pub(crate) fn rollback_update() {
    overlay::rollback();
}

pub(crate) fn commit_update() -> Result<(), StableMemoryError> {
    let Some(overlay) = overlay::take() else {
        return Ok(());
    };
    if overlay.is_empty() {
        return Ok(());
    }
    commit_overlay(overlay, true)
}

pub(crate) fn read_at(offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
    if let Some(result) = overlay::read_at(offset, dst) {
        return result;
    }
    read_base_at(offset, dst)
}

pub(crate) fn read_base_at(offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
    if dst.is_empty() {
        return Ok(true);
    }
    let block = Superblock::load()?;
    read_base_at_with_block(&block, offset, dst)
}

pub(crate) fn read_base_at_with_block(
    block: &Superblock,
    offset: u64,
    dst: &mut [u8],
) -> Result<bool, StableMemoryError> {
    if dst.is_empty() {
        return Ok(true);
    }
    if offset >= block.db_size {
        dst.fill(0);
        return Ok(false);
    }
    let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if requested <= block.db_size - offset {
        read_logical_range(block, offset, dst)?;
        return Ok(true);
    }
    let copied = requested.min(block.db_size - offset);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    read_logical_range(block, offset, &mut dst[..copied_len])?;
    dst[copied_len..].fill(0);
    Ok(copied == requested)
}

#[inline(always)]
pub(crate) fn read_base_at_with_page_cache(
    block: &Superblock,
    offset: u64,
    dst: &mut [u8],
    page_offsets: &mut PageOffsetCache,
) -> Result<bool, StableMemoryError> {
    if dst.is_empty() {
        return Ok(true);
    }
    if offset >= block.db_size {
        dst.fill(0);
        return Ok(false);
    }
    let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if requested <= block.db_size - offset {
        read_logical_range_with_page_cache(block, offset, dst, page_offsets)?;
        return Ok(true);
    }
    let copied = requested.min(block.db_size - offset);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    read_logical_range_with_page_cache(block, offset, &mut dst[..copied_len], page_offsets)?;
    dst[copied_len..].fill(0);
    Ok(copied == requested)
}

pub(crate) fn read_base_page(page_no: u64) -> Result<Vec<u8>, StableMemoryError> {
    let block = Superblock::load()?;
    let mut page = zero_page();
    read_base_at_with_block(
        &block,
        page_no
            .checked_mul(page_size())
            .ok_or(StableMemoryError::OffsetOverflow)?,
        &mut page,
    )?;
    Ok(page)
}

pub(crate) fn write_at(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    if let Some(result) = overlay::write_at(offset, bytes) {
        hit_failpoint(StableBlobFailpoint::OverlayWrite)?;
        return result;
    }
    if bytes.is_empty() {
        return Ok(());
    }
    ensure_current_layout()?;
    let mut direct = Overlay::new(Superblock::load()?.db_size);
    direct.write_at(offset, bytes)?;
    commit_overlay(direct, false)
}

pub(crate) fn truncate(size: u64) -> Result<(), StableMemoryError> {
    if let Some(result) = overlay::truncate(size) {
        hit_failpoint(StableBlobFailpoint::OverlayTruncate)?;
        return result;
    }
    ensure_current_layout()?;
    let mut direct = Overlay::new(Superblock::load()?.db_size);
    direct.truncate(size)?;
    if direct.is_empty() {
        return Ok(());
    }
    commit_overlay(direct, false)
}

pub(crate) fn file_size() -> Result<u64, StableMemoryError> {
    if let Some(size) = overlay::file_size() {
        return Ok(size);
    }
    Ok(Superblock::load()?.db_size)
}

#[allow(dead_code)]
pub fn export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, StableMemoryError> {
    reject_during_update()?;
    let block = Superblock::load()?;
    if offset >= block.db_size {
        return Ok(Vec::new());
    }
    let copied = len.min(block.db_size - offset);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let mut out = vec![0_u8; copied_len];
    read_logical_range(&block, offset, &mut out)?;
    Ok(out)
}

#[cfg(test)]
pub fn import_chunk(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    reject_during_update()?;
    let mut block = Superblock::load()?;
    if !block.is_importing() {
        return Err(StableMemoryError::ImportNotStarted);
    }
    let len = u64::try_from(bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if offset != block.import_written_until {
        return Err(StableMemoryError::ImportOutOfOrder {
            offset,
            expected: block.import_written_until,
        });
    }
    let end = checked_add(offset, len)?;
    if end > block.import_total_size {
        return Err(StableMemoryError::ImportOutOfBounds {
            offset,
            len,
            db_size: block.import_total_size,
        });
    }
    memory::write(import_offset(&block, offset)?, bytes)?;
    block.import_written_until = end;
    block.store()?;
    Ok(())
}

#[cfg(test)]
pub fn begin_import(total_size: u64, expected_checksum: u64) -> Result<(), StableMemoryError> {
    reject_during_update()?;
    let mut block = Superblock::load()?;
    if block.is_importing() {
        return Err(StableMemoryError::ImportAlreadyStarted);
    }
    let import_base_offset = append_base()?;
    checked_add(import_base_offset, total_size)?;
    block.flags |= FLAG_IMPORTING;
    block.clear_checksum_refresh();
    block.import_expected_checksum = expected_checksum;
    block.import_written_until = 0;
    block.import_total_size = total_size;
    block.import_base_offset = import_base_offset;
    block.store()?;
    Ok(())
}

#[cfg(test)]
pub fn finish_import() -> Result<(), StableMemoryError> {
    reject_during_update()?;
    let mut block = Superblock::load()?;
    if !block.is_importing() {
        return Err(StableMemoryError::ImportNotStarted);
    }
    if block.import_written_until != block.import_total_size {
        return Err(StableMemoryError::ImportIncomplete {
            written_until: block.import_written_until,
            db_size: block.import_total_size,
        });
    }
    let checksum = checksum_physical_range(block.import_base_offset, block.import_total_size)?;
    if checksum != block.import_expected_checksum {
        let expected = block.import_expected_checksum;
        clear_import(&mut block)?;
        return Err(StableMemoryError::ChecksumMismatch {
            expected,
            actual: checksum,
        });
    }
    block.db_size = block.import_total_size;
    block.db_base_offset = block.import_base_offset;
    block.page_table_offset = 0;
    block.page_count = page_count_for_size(block.db_size)?;
    block.layout_version = CURRENT_LAYOUT_VERSION;
    block.schema_version = 0;
    block.flags &= !FLAG_IMPORTING;
    block.flags &= !FLAG_CHECKSUM_STALE;
    block.clear_zero_extents();
    block.clear_checksum_refresh();
    block.checksum = checksum;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()?;
    Ok(())
}

#[cfg(test)]
pub fn cancel_import() -> Result<(), StableMemoryError> {
    reject_during_update()?;
    let mut block = Superblock::load()?;
    if !block.is_importing() {
        return Err(StableMemoryError::ImportNotStarted);
    }
    clear_import(&mut block)
}

pub fn refresh_checksum() -> Result<u64, StableMemoryError> {
    reject_during_update()?;
    let checksum = checksum()?;
    let mut block = Superblock::load()?;
    block.checksum = checksum;
    block.flags &= !FLAG_CHECKSUM_STALE;
    block.clear_checksum_refresh();
    block.store()?;
    Ok(checksum)
}

pub fn refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, StableMemoryError> {
    reject_during_update()?;
    if max_bytes == 0 {
        return Err(StableMemoryError::ChecksumRefreshChunkEmpty);
    }

    let mut block = Superblock::load()?;
    if block.is_importing() {
        return Err(StableMemoryError::ImportAlreadyStarted);
    }
    if !block.is_checksum_refreshing() {
        block.flags |= FLAG_CHECKSUM_REFRESHING;
        block.checksum_refresh_offset = 0;
        block.checksum_refresh_hash = fnv1a64(&[]);
        block.checksum_refresh_tx_id = block.last_tx_id;
    }
    if block.checksum_refresh_tx_id != block.last_tx_id {
        block.clear_checksum_refresh();
        block.store()?;
        return refresh_checksum_chunk(max_bytes);
    }

    let start = block.checksum_refresh_offset;
    let end = block.db_size.min(start.saturating_add(max_bytes));
    let mut offset = start;
    let mut hash = block.checksum_refresh_hash;
    while offset < end {
        let len = (end - offset).min(CHECKSUM_CHUNK_LEN);
        let copied_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        read_logical_range(&block, offset, &mut bytes)?;
        hash = fold_fnv1a64(hash, &bytes);
        offset += len;
    }

    block.checksum_refresh_offset = offset;
    block.checksum_refresh_hash = hash;
    if offset == block.db_size {
        block.checksum = hash;
        block.flags &= !FLAG_CHECKSUM_STALE;
        block.clear_checksum_refresh();
    }
    let out = ChecksumRefresh {
        complete: offset == block.db_size,
        checksum: hash,
        scanned_bytes: offset,
        db_size: block.db_size,
    };
    block.store()?;
    Ok(out)
}

pub fn checksum() -> Result<u64, StableMemoryError> {
    reject_during_update()?;
    let block = Superblock::load()?;
    checksum_logical_range(&block, block.db_size)
}

#[allow(dead_code)]
pub fn compact() -> Result<(), StableMemoryError> {
    reject_during_update()?;
    ensure_current_layout()?;
    Ok(())
}

#[allow(dead_code)]
pub fn storage_stats() -> Result<StorageStats, StableMemoryError> {
    let block = Superblock::load()?;
    ensure_current_layout()?;
    let page_table_bytes = 0;
    let zero_extent_count =
        u64::try_from(block.zero_extent_count()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let active_bytes = SUPERBLOCK_SIZE
        .checked_add(block.db_size)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let allocated_bytes = memory::size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let orphan_bytes_estimate = allocated_bytes.saturating_sub(active_bytes);
    let orphan_ratio_basis_points = orphan_bytes_estimate
        .saturating_mul(10_000)
        .checked_div(active_bytes)
        .unwrap_or(0);
    Ok(StorageStats {
        layout_version: block.layout_version,
        page_count: active_page_count(&block)?,
        page_table_bytes,
        zero_extent_count,
        active_bytes,
        allocated_bytes,
        orphan_bytes_estimate,
        orphan_ratio_basis_points,
        compact_recommended: false,
    })
}

fn reject_during_update() -> Result<(), StableMemoryError> {
    if overlay::is_active() {
        Err(StableMemoryError::UpdateInProgress)
    } else {
        Ok(())
    }
}
