use super::state::{PageOffsetCache, CHECKSUM_CHUNK_LEN};
use crate::config::{SQLITE_PAGE_SIZE, STABLE_PAGE_SIZE};
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{fnv1a64, Superblock, FLAG_IMPORTING};

pub(super) fn read_logical_range(
    block: &Superblock,
    offset: u64,
    dst: &mut [u8],
) -> Result<(), StableMemoryError> {
    if dst.is_empty() {
        return Ok(());
    }
    let in_page =
        usize::try_from(offset % page_size()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if dst.len() <= page_len() - in_page {
        return read_logical_page_slice(block, offset / page_size(), in_page, dst);
    }

    let mut copied_total = 0_usize;
    while copied_total < dst.len() {
        let absolute = checked_add(
            offset,
            u64::try_from(copied_total).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        let page_no = absolute / page_size();
        let in_page = usize::try_from(absolute % page_size())
            .map_err(|_| StableMemoryError::OffsetOverflow)?;
        let copied = (page_len() - in_page).min(dst.len() - copied_total);
        read_logical_page_slice(
            block,
            page_no,
            in_page,
            &mut dst[copied_total..copied_total + copied],
        )?;
        copied_total += copied;
    }
    Ok(())
}

pub(super) fn read_logical_range_with_page_cache(
    block: &Superblock,
    offset: u64,
    dst: &mut [u8],
    page_offsets: &mut PageOffsetCache,
) -> Result<(), StableMemoryError> {
    let in_page =
        usize::try_from(offset % page_size()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if dst.len() <= page_len() - in_page {
        return read_logical_page_slice_with_page_cache(
            block,
            offset / page_size(),
            in_page,
            dst,
            page_offsets,
        );
    }

    let mut copied_total = 0_usize;
    while copied_total < dst.len() {
        let absolute = checked_add(
            offset,
            u64::try_from(copied_total).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        let page_no = absolute / page_size();
        let in_page = usize::try_from(absolute % page_size())
            .map_err(|_| StableMemoryError::OffsetOverflow)?;
        let copied = (page_len() - in_page).min(dst.len() - copied_total);
        read_logical_page_slice_with_page_cache(
            block,
            page_no,
            in_page,
            &mut dst[copied_total..copied_total + copied],
            page_offsets,
        )?;
        copied_total += copied;
    }
    Ok(())
}

fn read_logical_page_slice(
    block: &Superblock,
    page_no: u64,
    in_page: usize,
    dst: &mut [u8],
) -> Result<(), StableMemoryError> {
    if page_is_zero_masked(block, page_no) {
        dst.fill(0);
        return Ok(());
    }
    let physical = page_offset_for(block, page_no)?;
    if physical == 0 {
        dst.fill(0);
        return Ok(());
    }
    let stable_offset = checked_add(
        physical,
        u64::try_from(in_page).map_err(|_| StableMemoryError::OffsetOverflow)?,
    )?;
    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    crate::read_metrics::record_stable_data_read(dst.len());
    memory::read_preallocated(stable_offset, dst)
}

#[inline(always)]
fn read_logical_page_slice_with_page_cache(
    block: &Superblock,
    page_no: u64,
    in_page: usize,
    dst: &mut [u8],
    page_offsets: &mut PageOffsetCache,
) -> Result<(), StableMemoryError> {
    if page_is_zero_masked(block, page_no) {
        dst.fill(0);
        return Ok(());
    }
    if dst.len() < page_len() && page_offsets.copy_page_slice(page_no, in_page, dst) {
        return Ok(());
    }
    let physical = match page_offsets.get(page_no) {
        Some(physical) => physical,
        None => {
            let physical = page_offset_for(block, page_no)?;
            page_offsets.insert(page_no, physical);
            physical
        }
    };
    if physical == 0 {
        dst.fill(0);
        return Ok(());
    }
    if in_page == 0 && dst.len() == page_len() {
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_stable_data_read(dst.len());
        return memory::read_preallocated(physical, dst);
    }
    if dst.len() < page_len() {
        let mut page = zero_page();
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_stable_data_read(page.len());
        memory::read_preallocated(physical, &mut page)?;
        let end = in_page + dst.len();
        dst.copy_from_slice(&page[in_page..end]);
        page_offsets.insert_page(page_no, page);
        return Ok(());
    }
    let stable_offset = checked_add(
        physical,
        u64::try_from(in_page).map_err(|_| StableMemoryError::OffsetOverflow)?,
    )?;
    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    crate::read_metrics::record_stable_data_read(dst.len());
    memory::read_preallocated(stable_offset, dst)
}

fn page_offset_for(block: &Superblock, page_no: u64) -> Result<u64, StableMemoryError> {
    if page_no >= active_page_count(block)? {
        return Ok(0);
    }
    page_physical_offset(block, page_no)
}

fn page_is_zero_masked(block: &Superblock, page_no: u64) -> bool {
    block
        .zero_extents()
        .iter()
        .any(|extent| page_no >= extent.start_page && page_no < extent.end_page)
}

pub(super) fn page_physical_offset(
    block: &Superblock,
    page_no: u64,
) -> Result<u64, StableMemoryError> {
    checked_add(
        block.db_base_offset,
        page_no
            .checked_mul(page_size())
            .ok_or(StableMemoryError::OffsetOverflow)?,
    )
}

pub(super) fn checksum_logical_range(
    block: &Superblock,
    len: u64,
) -> Result<u64, StableMemoryError> {
    let mut offset = 0_u64;
    let mut hash = fnv1a64(&[]);
    while offset < len {
        let chunk_len = (len - offset).min(CHECKSUM_CHUNK_LEN);
        let copied_len =
            usize::try_from(chunk_len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        read_logical_range(block, offset, &mut bytes)?;
        hash = fold_fnv1a64(hash, &bytes);
        offset += chunk_len;
    }
    Ok(hash)
}

#[allow(dead_code)]
pub(super) fn checksum_physical_range(
    base_offset: u64,
    len: u64,
) -> Result<u64, StableMemoryError> {
    let mut offset = 0_u64;
    let mut hash = fnv1a64(&[]);
    while offset < len {
        let chunk_len = (len - offset).min(CHECKSUM_CHUNK_LEN);
        let copied_len =
            usize::try_from(chunk_len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        memory::read_preallocated(checked_add(base_offset, offset)?, &mut bytes)?;
        hash = fold_fnv1a64(hash, &bytes);
        offset += chunk_len;
    }
    Ok(hash)
}

#[allow(dead_code)]
pub(super) fn clear_import(block: &mut Superblock) -> Result<(), StableMemoryError> {
    block.flags &= !FLAG_IMPORTING;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()?;
    Ok(())
}

#[allow(dead_code)]
pub(super) fn import_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.import_base_offset, offset)
}

pub(super) fn active_page_count(block: &Superblock) -> Result<u64, StableMemoryError> {
    page_count_for_size(block.db_size)
}

pub(crate) fn page_count_for_size(size: u64) -> Result<u64, StableMemoryError> {
    Ok(size.div_ceil(page_size()))
}

#[allow(dead_code)]
pub(super) fn append_base() -> Result<u64, StableMemoryError> {
    // Fresh bases must append at the high-water mark; never reuse stale physical gaps.
    memory::size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)
}

pub(super) fn page_size() -> u64 {
    u64::from(SQLITE_PAGE_SIZE)
}

pub(super) fn page_len() -> usize {
    usize::try_from(SQLITE_PAGE_SIZE).expect("SQLite page size fits usize")
}

pub(super) fn zero_page() -> Vec<u8> {
    vec![0_u8; page_len()]
}

pub(super) fn checked_add(left: u64, right: u64) -> Result<u64, StableMemoryError> {
    left.checked_add(right)
        .ok_or(StableMemoryError::OffsetOverflow)
}

pub(super) fn fold_fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
