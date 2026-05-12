//! Offset-based access to the `/main.db` image in stable memory.
//!
//! SQLite sees a single logical file. Physical stable memory grows by 64KiB
//! pages and never shrinks; `Superblock::db_size` is the logical file length.

use crate::config::DB_REGION_OFFSET;
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{fnv1a64, Superblock, FLAG_IMPORTING};

const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
const ZERO_CHUNK_LEN: u64 = 16 * 1024;

pub fn read_at(offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
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
    memory::read(db_offset(offset)?, &mut dst[..copied_len])?;
    Ok(copied == requested)
}

pub fn write_at(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
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
    memory::write(db_offset(offset)?, bytes)?;
    if end > block.db_size {
        block.db_size = end;
        block.store()?;
    }
    Ok(())
}

pub fn truncate(size: u64) -> Result<(), StableMemoryError> {
    let block = Superblock::load()?;
    if size > block.db_size {
        zero_fill_range(block.db_size, size)?;
    }
    Superblock::set_db_size(size)
}

pub fn file_size() -> Result<u64, StableMemoryError> {
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
    memory::read(db_offset(offset)?, &mut out)?;
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
    let import_base_offset = db_offset(block.db_size)?;
    checked_add(import_base_offset, total_size)?;
    block.flags |= FLAG_IMPORTING;
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
    copy_import_to_main(&block)?;
    block.db_size = block.import_total_size;
    block.flags &= !FLAG_IMPORTING;
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
    block.store()?;
    Ok(checksum)
}

pub fn checksum() -> Result<u64, StableMemoryError> {
    let block = Superblock::load()?;
    checksum_range(db_offset(0)?, block.db_size)
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

fn copy_import_to_main(block: &Superblock) -> Result<(), StableMemoryError> {
    let mut offset = 0_u64;
    while offset < block.import_total_size {
        let remaining = block.import_total_size - offset;
        let len = remaining.min(CHECKSUM_CHUNK_LEN);
        let copied_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        memory::read(import_offset(block, offset)?, &mut bytes)?;
        memory::write(db_offset(offset)?, &bytes)?;
        offset += len;
    }
    Ok(())
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
    let mut offset = start;
    while offset < end {
        let remaining = end - offset;
        let len = remaining.min(ZERO_CHUNK_LEN);
        let zero_len = usize::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let zeros = vec![0_u8; zero_len];
        memory::write(db_offset(offset)?, &zeros)?;
        offset += len;
    }
    Ok(())
}

fn import_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.import_base_offset, offset)
}

fn db_offset(offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(DB_REGION_OFFSET, offset)
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
