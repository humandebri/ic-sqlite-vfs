//! Offset-based access to the `/main.db` image in stable memory.
//! SQLite sees a single logical file. Physical stable memory grows by 64KiB
//! pages and never shrinks; `Superblock::db_size` is the logical file length.

use crate::config::STABLE_PAGE_SIZE;
use crate::sqlite_vfs::overlay;
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{
    fnv1a64, Superblock, FLAG_CHECKSUM_REFRESHING, FLAG_CHECKSUM_STALE, FLAG_IMPORTING,
};
use std::cell::RefCell;

const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
const ZERO_CHUNK_LEN: u64 = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChecksumRefresh {
    pub complete: bool,
    pub checksum: u64,
    pub scanned_bytes: u64,
    pub db_size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StableBlobFailpoint {
    OverlayWrite,
    OverlayTruncate,
    CommitCapacity,
    CommitChunkWrite,
    CommitSuperblockStore,
}

thread_local! {
    static FAILPOINT: RefCell<Option<StableBlobFailpoint>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_failpoint(failpoint: StableBlobFailpoint) {
    FAILPOINT.with(|slot| *slot.borrow_mut() = Some(failpoint));
}

#[cfg(test)]
pub(crate) fn clear_failpoint() {
    FAILPOINT.with(|slot| *slot.borrow_mut() = None);
}

pub(crate) fn begin_update() -> Result<(), StableMemoryError> {
    overlay::begin(Superblock::load()?.db_size)
}

pub(crate) fn rollback_update() {
    overlay::rollback();
}

pub(crate) fn commit_update() -> Result<(), StableMemoryError> {
    let Some(overlay) = overlay::take() else {
        return Superblock::record_committed_tx();
    };
    if overlay.is_empty() {
        return Superblock::record_committed_tx();
    }

    hit_failpoint(StableBlobFailpoint::CommitCapacity)?;
    let final_size = overlay.size();
    let shadow_base = append_base()?;
    memory::ensure_capacity(checked_add(shadow_base, overlay.max_end()?)?)?;

    let mut offset = 0_u64;
    while offset < final_size {
        let remaining = final_size - offset;
        let len = remaining.min(CHECKSUM_CHUNK_LEN);
        let copied_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        overlay.read_merged_chunk(offset, &mut bytes)?;
        hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
        memory::write(checked_add(shadow_base, offset)?, &bytes)?;
        offset += len;
    }

    hit_failpoint(StableBlobFailpoint::CommitSuperblockStore)?;
    Superblock::commit_db_image(shadow_base, final_size)
}

pub(crate) fn read_at(offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
    if let Some(result) = overlay::read_at(offset, dst) {
        return result;
    }
    read_base_at(offset, dst)
}

pub(crate) fn read_base_at(offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
    dst.fill(0);
    let block = Superblock::load()?;
    if dst.is_empty() {
        return Ok(true);
    }
    if offset >= block.db_size {
        return Ok(false);
    }
    let available = block.db_size - offset;
    let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let copied = requested.min(available);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    memory::read(active_offset(&block, offset)?, &mut dst[..copied_len])?;
    Ok(copied == requested)
}

pub(crate) fn write_at(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    if let Some(result) = overlay::write_at(offset, bytes) {
        hit_failpoint(StableBlobFailpoint::OverlayWrite)?;
        return result;
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let len = u64::try_from(bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let end = offset
        .checked_add(len)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let mut block = Superblock::load()?;
    if offset > block.db_size {
        zero_fill_range(block.db_size, offset)?;
    }
    memory::write(active_offset(&block, offset)?, bytes)?;
    if end > block.db_size {
        block.db_size = end;
        block.store()?;
    }
    Ok(())
}

pub(crate) fn truncate(size: u64) -> Result<(), StableMemoryError> {
    if let Some(result) = overlay::truncate(size) {
        hit_failpoint(StableBlobFailpoint::OverlayTruncate)?;
        return result;
    }
    let block = Superblock::load()?;
    if size > block.db_size {
        zero_fill_range(block.db_size, size)?;
    }
    Superblock::set_db_size(size)
}

pub(crate) fn file_size() -> Result<u64, StableMemoryError> {
    if let Some(size) = overlay::file_size() {
        return Ok(size);
    }
    Ok(Superblock::load()?.db_size)
}

pub fn export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, StableMemoryError> {
    let block = Superblock::load()?;
    if offset >= block.db_size {
        return Ok(Vec::new());
    }
    let available = block.db_size - offset;
    let copied = len.min(available);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let mut out = vec![0_u8; copied_len];
    memory::read(active_offset(&block, offset)?, &mut out)?;
    Ok(out)
}

pub fn import_chunk(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
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
    let end = offset
        .checked_add(len)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    if end > block.import_total_size {
        return Err(StableMemoryError::ImportOutOfBounds {
            offset,
            len,
            db_size: block.import_total_size,
        });
    }
    memory::write(import_offset(&block, offset)?, bytes)?;
    block.import_written_until = end;
    block.store()
}

pub fn begin_import(total_size: u64, expected_checksum: u64) -> Result<(), StableMemoryError> {
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
    block.store()
}

pub fn finish_import() -> Result<(), StableMemoryError> {
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
    let checksum = checksum_range(block.import_base_offset, block.import_total_size)?;
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
    block.flags &= !FLAG_IMPORTING;
    block.flags &= !FLAG_CHECKSUM_STALE;
    block.clear_checksum_refresh();
    block.checksum = checksum;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()
}

pub fn refresh_checksum() -> Result<u64, StableMemoryError> {
    let checksum = checksum()?;
    let mut block = Superblock::load()?;
    block.checksum = checksum;
    block.flags &= !FLAG_CHECKSUM_STALE;
    block.clear_checksum_refresh();
    block.store()?;
    Ok(checksum)
}

pub fn refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, StableMemoryError> {
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
    let mut offset = start;
    let mut hash = block.checksum_refresh_hash;
    let end = block.db_size.min(start.saturating_add(max_bytes));

    while offset < end {
        let remaining = end - offset;
        let len = remaining.min(CHECKSUM_CHUNK_LEN);
        let copied_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        memory::read(active_offset(&block, offset)?, &mut bytes)?;
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
    let block = Superblock::load()?;
    checksum_range(block.db_base_offset, block.db_size)
}

fn checksum_range(base_offset: u64, len: u64) -> Result<u64, StableMemoryError> {
    let mut offset = 0_u64;
    let mut hash = fnv1a64(&[]);
    while offset < len {
        let remaining = len - offset;
        let len = remaining.min(CHECKSUM_CHUNK_LEN);
        let copied_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        memory::read(checked_add(base_offset, offset)?, &mut bytes)?;
        hash = fold_fnv1a64(hash, &bytes);
        offset += len;
    }
    Ok(hash)
}

fn clear_import(block: &mut Superblock) -> Result<(), StableMemoryError> {
    block.flags &= !FLAG_IMPORTING;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()
}

fn zero_fill_range(start: u64, end: u64) -> Result<(), StableMemoryError> {
    let block = Superblock::load()?;
    let mut offset = start;
    while offset < end {
        let remaining = end - offset;
        let len = remaining.min(ZERO_CHUNK_LEN);
        let zero_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let zeros = vec![0_u8; zero_len];
        memory::write(active_offset(&block, offset)?, &zeros)?;
        offset += len;
    }
    Ok(())
}

fn import_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.import_base_offset, offset)
}

fn active_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.db_base_offset, offset)
}

fn append_base() -> Result<u64, StableMemoryError> {
    memory::size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)
}

fn checked_add(left: u64, right: u64) -> Result<u64, StableMemoryError> {
    left.checked_add(right)
        .ok_or(StableMemoryError::OffsetOverflow)
}

fn fold_fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn hit_failpoint(failpoint: StableBlobFailpoint) -> Result<(), StableMemoryError> {
    FAILPOINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if *slot == Some(failpoint) {
            *slot = None;
            Err(StableMemoryError::Failpoint(failpoint.name()))
        } else {
            Ok(())
        }
    })
}

impl StableBlobFailpoint {
    fn name(self) -> &'static str {
        match self {
            Self::OverlayWrite => "before overlay write",
            Self::OverlayTruncate => "before overlay truncate",
            Self::CommitCapacity => "before commit capacity",
            Self::CommitChunkWrite => "before commit chunk write",
            Self::CommitSuperblockStore => "before commit superblock store",
        }
    }
}
