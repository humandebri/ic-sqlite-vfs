//! Logical `/main.db` access backed by segmented stable-memory page mapping.
//!
//! SQLite sees a contiguous file. Internally, the active superblock points to a
//! root table. Each root entry points to a 256-page segment table.

use crate::config::{SQLITE_PAGE_SIZE, STABLE_PAGE_SIZE, SUPERBLOCK_SIZE};
use crate::sqlite_vfs::overlay::{self, Overlay};
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{
    fnv1a64, Superblock, FLAG_CHECKSUM_REFRESHING, FLAG_CHECKSUM_STALE, FLAG_IMPORTING,
    PAGE_MAP_LAYOUT_VERSION,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
const PAGE_TABLE_ENTRY_LEN: u64 = 8;
const SEGMENT_PAGE_COUNT: u64 = 256;
const READ_SEGMENT_CACHE_CAPACITY: usize = 8;
const COMPACT_MIN_ORPHAN_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChecksumRefresh {
    pub complete: bool,
    pub checksum: u64,
    pub scanned_bytes: u64,
    pub db_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageStats {
    pub layout_version: u64,
    pub page_count: u64,
    pub page_table_bytes: u64,
    pub active_bytes: u64,
    pub allocated_bytes: u64,
    pub orphan_bytes_estimate: u64,
    pub orphan_ratio_basis_points: u64,
    pub compact_recommended: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StableBlobFailpoint {
    OverlayWrite,
    OverlayTruncate,
    CommitCapacity,
    CommitChunkWrite,
    CommitPageTableWrite,
    CommitSuperblockStore,
}

thread_local! {
    static FAILPOINT: RefCell<Option<StableBlobFailpoint>> = const { RefCell::new(None) };
    static READ_TABLE_CACHE: RefCell<ReadTableCache> = RefCell::new(ReadTableCache::new());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadCacheKey {
    page_table_offset: u64,
    page_count: u64,
    db_size: u64,
    last_tx_id: u64,
}

#[derive(Debug)]
struct ReadTableCache {
    key: Option<ReadCacheKey>,
    root: Vec<u64>,
    segments: BTreeMap<u64, Vec<u64>>,
    segment_lru: VecDeque<u64>,
}

impl ReadTableCache {
    fn new() -> Self {
        Self {
            key: None,
            root: Vec::new(),
            segments: BTreeMap::new(),
            segment_lru: VecDeque::new(),
        }
    }

    fn clear(&mut self) {
        self.key = None;
        self.root.clear();
        self.segments.clear();
        self.segment_lru.clear();
    }

    fn ensure_key(&mut self, key: ReadCacheKey) {
        if self.key == Some(key) {
            return;
        }
        self.clear();
        self.key = Some(key);
    }

    fn touch_segment(&mut self, segment_no: u64) {
        self.segment_lru.retain(|cached| *cached != segment_no);
        self.segment_lru.push_back(segment_no);
    }

    fn insert_segment(&mut self, segment_no: u64, table: Vec<u64>) {
        self.segments.insert(segment_no, table);
        self.touch_segment(segment_no);
        while self.segments.len() > READ_SEGMENT_CACHE_CAPACITY {
            let Some(evicted) = self.segment_lru.pop_front() else {
                return;
            };
            self.segments.remove(&evicted);
        }
    }
}

#[cfg(test)]
pub(crate) fn set_failpoint(failpoint: StableBlobFailpoint) {
    FAILPOINT.with(|slot| *slot.borrow_mut() = Some(failpoint));
}

#[cfg(test)]
pub(crate) fn clear_failpoint() {
    FAILPOINT.with(|slot| *slot.borrow_mut() = None);
}

pub(crate) fn ensure_page_map_layout() -> Result<(), StableMemoryError> {
    let block = Superblock::load()?;
    if block.layout_version >= PAGE_MAP_LAYOUT_VERSION {
        return Ok(());
    }
    Err(StableMemoryError::UnsupportedLayoutVersion(
        block.layout_version,
    ))
}

pub(crate) fn begin_update() -> Result<(), StableMemoryError> {
    ensure_page_map_layout()?;
    overlay::begin(Superblock::load()?.db_size)
}

pub(crate) fn rollback_update() {
    overlay::rollback();
}

#[doc(hidden)]
pub fn invalidate_read_cache() {
    READ_TABLE_CACHE.with(|cache| cache.borrow_mut().clear());
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
    dst.fill(0);
    let block = Superblock::load()?;
    if dst.is_empty() {
        return Ok(true);
    }
    if offset >= block.db_size {
        return Ok(false);
    }
    let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let copied = requested.min(block.db_size - offset);
    let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
    read_logical_range(&block, offset, &mut dst[..copied_len])?;
    Ok(copied == requested)
}

pub(crate) fn read_base_page(page_no: u64) -> Result<Vec<u8>, StableMemoryError> {
    let block = Superblock::load()?;
    let mut page = zero_page();
    if page_no >= active_page_count(&block)? {
        return Ok(page);
    }
    let physical = page_offset_for(&block, page_no)?;
    if physical != 0 {
        memory::read(physical, &mut page)?;
    }
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
    ensure_page_map_layout()?;
    let mut direct = Overlay::new(Superblock::load()?.db_size);
    direct.write_at(offset, bytes)?;
    commit_overlay(direct, false)
}

pub(crate) fn truncate(size: u64) -> Result<(), StableMemoryError> {
    if let Some(result) = overlay::truncate(size) {
        hit_failpoint(StableBlobFailpoint::OverlayTruncate)?;
        return result;
    }
    ensure_page_map_layout()?;
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
    invalidate_read_cache();
    Ok(())
}

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
    invalidate_read_cache();
    Ok(())
}

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
    let entries = imported_page_table(&block)?;
    let (root_offset, root_len) = write_segmented_tables(&entries)?;
    block.db_size = block.import_total_size;
    block.db_base_offset = block.import_base_offset;
    block.page_table_offset = root_offset;
    block.page_count = root_len;
    block.layout_version = PAGE_MAP_LAYOUT_VERSION;
    block.flags &= !FLAG_IMPORTING;
    block.flags &= !FLAG_CHECKSUM_STALE;
    block.clear_checksum_refresh();
    block.checksum = checksum;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()?;
    invalidate_read_cache();
    Ok(())
}

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
    invalidate_read_cache();
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
        invalidate_read_cache();
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
    invalidate_read_cache();
    Ok(out)
}

pub fn checksum() -> Result<u64, StableMemoryError> {
    reject_during_update()?;
    let block = Superblock::load()?;
    checksum_logical_range(&block, block.db_size)
}

pub fn compact() -> Result<(), StableMemoryError> {
    reject_during_update()?;
    ensure_page_map_layout()?;
    let block = Superblock::load()?;
    let table = read_page_table(&block)?;
    let mut compacted = Vec::with_capacity(table.len());
    let mut cursor = append_base()?;

    for offset in table {
        if offset == 0 {
            compacted.push(0);
            continue;
        }
        let mut page = zero_page();
        memory::read(offset, &mut page)?;
        memory::write(cursor, &page)?;
        compacted.push(cursor);
        cursor = checked_add(cursor, page_size())?;
    }

    let (root_offset, root_len) = write_segmented_tables(&compacted)?;
    Superblock::store_page_map_without_tx(root_offset, root_len, block.db_size)?;
    invalidate_read_cache();
    Ok(())
}

pub fn storage_stats() -> Result<StorageStats, StableMemoryError> {
    let block = Superblock::load()?;
    let table = read_page_table(&block)?;
    let non_zero_pages = u64::try_from(table.iter().filter(|offset| **offset != 0).count())
        .map_err(|_| StableMemoryError::OffsetOverflow)?;
    let segment_count = active_segment_count(&block)?;
    let root_bytes = root_table_bytes(segment_count)?;
    let segment_bytes = segment_count
        .checked_mul(segment_table_bytes()?)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let page_table_bytes = checked_add(root_bytes, segment_bytes)?;
    let active_bytes = SUPERBLOCK_SIZE
        .checked_add(non_zero_pages.saturating_mul(page_size()))
        .and_then(|value| value.checked_add(page_table_bytes))
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let allocated_bytes = memory::size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let orphan_bytes_estimate = allocated_bytes.saturating_sub(active_bytes);
    let orphan_ratio_basis_points = if active_bytes == 0 {
        0
    } else {
        orphan_bytes_estimate.saturating_mul(10_000) / active_bytes
    };
    Ok(StorageStats {
        layout_version: block.layout_version,
        page_count: active_page_count(&block)?,
        page_table_bytes,
        active_bytes,
        allocated_bytes,
        orphan_bytes_estimate,
        orphan_ratio_basis_points,
        compact_recommended: orphan_bytes_estimate >= active_bytes
            && orphan_bytes_estimate >= COMPACT_MIN_ORPHAN_BYTES,
    })
}

pub(crate) fn page_count_for_size(size: u64) -> Result<u64, StableMemoryError> {
    Ok(size.div_ceil(page_size()))
}

#[cfg(test)]
pub(crate) fn debug_root_table_for_tests() -> Result<Vec<u64>, StableMemoryError> {
    let block = Superblock::load()?;
    read_root_table(&block)
}

fn commit_overlay(overlay: Overlay, advance_tx: bool) -> Result<(), StableMemoryError> {
    hit_failpoint(StableBlobFailpoint::CommitCapacity)?;
    let block = Superblock::load()?;
    let mut root = read_root_table(&block)?;
    let final_page_count = page_count_for_size(overlay.size())?;
    let final_segment_count = segment_count_for_pages(final_page_count)?;
    let root_len =
        usize::try_from(final_segment_count).map_err(|_| StableMemoryError::OffsetOverflow)?;
    root.resize(root_len, 0);
    root.truncate(root_len);

    let mut segment_updates = BTreeMap::<u64, Vec<u64>>::new();
    let mut cursor = append_base()?;
    for (page_no, page) in overlay.dirty_pages() {
        if *page_no >= final_page_count {
            continue;
        }
        hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
        memory::write(cursor, page)?;
        let segment_no = segment_no(*page_no);
        let index = segment_index(*page_no)?;
        let table = load_segment_for_update(&block, &root, &mut segment_updates, segment_no)?;
        table[index] = cursor;
        cursor = checked_add(cursor, page_size())?;
    }

    clear_truncated_tail(&block, &root, &mut segment_updates, final_page_count)?;

    hit_failpoint(StableBlobFailpoint::CommitPageTableWrite)?;
    for (segment_no, table) in segment_updates {
        if segment_no >= final_segment_count {
            continue;
        }
        let offset = write_segment_table(&table)?;
        let index = usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
        root[index] = offset;
    }
    let root_offset = write_root_table(&root)?;
    hit_failpoint(StableBlobFailpoint::CommitSuperblockStore)?;
    let result = if advance_tx {
        Superblock::commit_page_map(root_offset, entries_len_u64(&root)?, overlay.size())
    } else {
        Superblock::store_page_map_without_tx(root_offset, entries_len_u64(&root)?, overlay.size())
    };
    if result.is_ok() {
        invalidate_read_cache();
    }
    result
}

fn load_segment_for_update<'a>(
    block: &Superblock,
    root: &[u64],
    updates: &'a mut BTreeMap<u64, Vec<u64>>,
    segment_no: u64,
) -> Result<&'a mut Vec<u64>, StableMemoryError> {
    if !updates.contains_key(&segment_no) {
        let table = read_segment_table(block, root, segment_no)?;
        updates.insert(segment_no, table);
    }
    updates
        .get_mut(&segment_no)
        .ok_or(StableMemoryError::OffsetOverflow)
}

fn clear_truncated_tail(
    block: &Superblock,
    root: &[u64],
    updates: &mut BTreeMap<u64, Vec<u64>>,
    final_page_count: u64,
) -> Result<(), StableMemoryError> {
    let old_page_count = active_page_count(block)?;
    if final_page_count >= old_page_count || final_page_count == 0 {
        return Ok(());
    }
    let boundary_segment = segment_no(final_page_count);
    if boundary_segment >= segment_count_for_pages(final_page_count)? {
        return Ok(());
    }
    let start = segment_index(final_page_count)?;
    if start == 0 {
        return Ok(());
    }
    let table = load_segment_for_update(block, root, updates, boundary_segment)?;
    table[start..].fill(0);
    Ok(())
}

fn reject_during_update() -> Result<(), StableMemoryError> {
    if overlay::is_active() {
        Err(StableMemoryError::UpdateInProgress)
    } else {
        Ok(())
    }
}

fn read_logical_range(
    block: &Superblock,
    offset: u64,
    dst: &mut [u8],
) -> Result<(), StableMemoryError> {
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
        let physical = page_offset_for(block, page_no)?;
        if physical == 0 {
            dst[copied_total..copied_total + copied].fill(0);
        } else {
            let stable_offset = checked_add(
                physical,
                u64::try_from(in_page).map_err(|_| StableMemoryError::OffsetOverflow)?,
            )?;
            memory::read(stable_offset, &mut dst[copied_total..copied_total + copied])?;
        }
        copied_total += copied;
    }
    Ok(())
}

fn page_offset_for(block: &Superblock, page_no: u64) -> Result<u64, StableMemoryError> {
    if page_no >= active_page_count(block)? || block.page_table_offset == 0 {
        return Ok(0);
    }
    cached_page_offset_for(block, page_no)
}

fn read_page_table(block: &Superblock) -> Result<Vec<u64>, StableMemoryError> {
    let root = read_root_table(block)?;
    let count = active_page_count(block)?;
    let capacity = usize::try_from(count).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let mut entries = Vec::with_capacity(capacity);
    for segment_no in 0..segment_count_for_pages(count)? {
        let table = read_segment_table(block, &root, segment_no)?;
        for entry in table {
            if entries.len() == capacity {
                break;
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn cached_page_offset_for(block: &Superblock, page_no: u64) -> Result<u64, StableMemoryError> {
    let key = read_cache_key(block);
    let segment_no = segment_no(page_no);
    let index = segment_index(page_no)?;
    READ_TABLE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.ensure_key(key);
        if cache.root.is_empty() {
            cache.root = read_root_table(block)?;
        }
        let Some(segment_offset) = cache
            .root
            .get(usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?)
            .copied()
        else {
            return Ok(0);
        };
        if segment_offset == 0 {
            return Ok(0);
        }
        if cache.segments.contains_key(&segment_no) {
            cache.touch_segment(segment_no);
        } else {
            let table = read_segment_table_at(segment_offset)?;
            cache.insert_segment(segment_no, table);
        }
        Ok(cache
            .segments
            .get(&segment_no)
            .and_then(|table| table.get(index))
            .copied()
            .unwrap_or(0))
    })
}

fn read_root_table(block: &Superblock) -> Result<Vec<u64>, StableMemoryError> {
    if block.page_count == 0 {
        return Ok(Vec::new());
    }
    let bytes_len = usize::try_from(root_table_bytes(block.page_count)?)
        .map_err(|_| StableMemoryError::OffsetOverflow)?;
    let mut bytes = vec![0_u8; bytes_len];
    memory::read(block.page_table_offset, &mut bytes)?;
    decode_u64_table(&bytes)
}

fn read_segment_table(
    _block: &Superblock,
    root: &[u64],
    segment_no: u64,
) -> Result<Vec<u64>, StableMemoryError> {
    let table = vec![0_u64; segment_page_count_usize()];
    let index = usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let Some(offset) = root.get(index).copied() else {
        return Ok(table);
    };
    if offset == 0 {
        return Ok(table);
    }
    read_segment_table_at(offset)
}

fn read_segment_table_at(offset: u64) -> Result<Vec<u64>, StableMemoryError> {
    let mut bytes = vec![0_u8; segment_table_len()];
    memory::read(offset, &mut bytes)?;
    let mut table = decode_u64_table(&bytes)?;
    table.resize(segment_page_count_usize(), 0);
    Ok(table)
}

fn write_segmented_tables(entries: &[u64]) -> Result<(u64, u64), StableMemoryError> {
    if entries.is_empty() {
        return Ok((0, 0));
    }
    let root_len = segment_count_for_pages(entries_len_u64(entries)?)?;
    let mut root = Vec::with_capacity(
        usize::try_from(root_len).map_err(|_| StableMemoryError::OffsetOverflow)?,
    );
    for segment_no in 0..root_len {
        let start = usize::try_from(
            segment_no
                .checked_mul(SEGMENT_PAGE_COUNT)
                .ok_or(StableMemoryError::OffsetOverflow)?,
        )
        .map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut table = vec![0_u64; segment_page_count_usize()];
        for (offset, entry) in entries[start..]
            .iter()
            .take(segment_page_count_usize())
            .enumerate()
        {
            table[offset] = *entry;
        }
        root.push(write_segment_table(&table)?);
    }
    let root_offset = write_root_table(&root)?;
    Ok((root_offset, entries_len_u64(&root)?))
}

fn write_segment_table(entries: &[u64]) -> Result<u64, StableMemoryError> {
    let mut table = vec![0_u64; segment_page_count_usize()];
    for (index, entry) in entries.iter().take(segment_page_count_usize()).enumerate() {
        table[index] = *entry;
    }
    write_u64_table(&table)
}

fn write_root_table(entries: &[u64]) -> Result<u64, StableMemoryError> {
    write_u64_table(entries)
}

fn write_u64_table(entries: &[u64]) -> Result<u64, StableMemoryError> {
    if entries.is_empty() {
        return Ok(0);
    }
    let offset = append_base()?;
    let mut bytes = Vec::with_capacity(entries.len() * 8);
    for entry in entries {
        bytes.extend_from_slice(&entry.to_le_bytes());
    }
    memory::write(offset, &bytes)?;
    Ok(offset)
}

fn decode_u64_table(bytes: &[u8]) -> Result<Vec<u64>, StableMemoryError> {
    if bytes.len() % 8 != 0 {
        return Err(StableMemoryError::OffsetOverflow);
    }
    let mut entries = Vec::with_capacity(bytes.len() / 8);
    for chunk in bytes.chunks_exact(8) {
        let mut entry = [0_u8; 8];
        entry.copy_from_slice(chunk);
        entries.push(u64::from_le_bytes(entry));
    }
    Ok(entries)
}

fn imported_page_table(block: &Superblock) -> Result<Vec<u64>, StableMemoryError> {
    let count = page_count_for_size(block.import_total_size)?;
    let capacity = usize::try_from(count).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let mut entries = Vec::with_capacity(capacity);
    for page_no in 0..count {
        entries.push(checked_add(
            block.import_base_offset,
            page_no
                .checked_mul(page_size())
                .ok_or(StableMemoryError::OffsetOverflow)?,
        )?);
    }
    Ok(entries)
}

fn checksum_logical_range(block: &Superblock, len: u64) -> Result<u64, StableMemoryError> {
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

fn checksum_physical_range(base_offset: u64, len: u64) -> Result<u64, StableMemoryError> {
    let mut offset = 0_u64;
    let mut hash = fnv1a64(&[]);
    while offset < len {
        let chunk_len = (len - offset).min(CHECKSUM_CHUNK_LEN);
        let copied_len =
            usize::try_from(chunk_len).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let mut bytes = vec![0_u8; copied_len];
        memory::read(checked_add(base_offset, offset)?, &mut bytes)?;
        hash = fold_fnv1a64(hash, &bytes);
        offset += chunk_len;
    }
    Ok(hash)
}

fn clear_import(block: &mut Superblock) -> Result<(), StableMemoryError> {
    block.flags &= !FLAG_IMPORTING;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()?;
    invalidate_read_cache();
    Ok(())
}

fn import_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.import_base_offset, offset)
}

fn active_page_count(block: &Superblock) -> Result<u64, StableMemoryError> {
    page_count_for_size(block.db_size)
}

fn active_segment_count(block: &Superblock) -> Result<u64, StableMemoryError> {
    Ok(block.page_count)
}

fn read_cache_key(block: &Superblock) -> ReadCacheKey {
    ReadCacheKey {
        page_table_offset: block.page_table_offset,
        page_count: block.page_count,
        db_size: block.db_size,
        last_tx_id: block.last_tx_id,
    }
}

fn segment_count_for_pages(page_count: u64) -> Result<u64, StableMemoryError> {
    Ok(page_count.div_ceil(SEGMENT_PAGE_COUNT))
}

fn segment_no(page_no: u64) -> u64 {
    page_no / SEGMENT_PAGE_COUNT
}

fn segment_index(page_no: u64) -> Result<usize, StableMemoryError> {
    usize::try_from(page_no % SEGMENT_PAGE_COUNT).map_err(|_| StableMemoryError::OffsetOverflow)
}

fn segment_page_count_usize() -> usize {
    usize::try_from(SEGMENT_PAGE_COUNT).expect("segment page count fits usize")
}

fn segment_table_len() -> usize {
    segment_page_count_usize() * 8
}

fn segment_table_bytes() -> Result<u64, StableMemoryError> {
    u64::try_from(segment_table_len()).map_err(|_| StableMemoryError::OffsetOverflow)
}

fn root_table_bytes(entry_count: u64) -> Result<u64, StableMemoryError> {
    entry_count
        .checked_mul(PAGE_TABLE_ENTRY_LEN)
        .ok_or(StableMemoryError::OffsetOverflow)
}

fn entries_len_u64<T>(entries: &[T]) -> Result<u64, StableMemoryError> {
    u64::try_from(entries.len()).map_err(|_| StableMemoryError::OffsetOverflow)
}

fn append_base() -> Result<u64, StableMemoryError> {
    memory::size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)
}

fn page_size() -> u64 {
    u64::from(SQLITE_PAGE_SIZE)
}

fn page_len() -> usize {
    usize::try_from(SQLITE_PAGE_SIZE).expect("SQLite page size fits usize")
}

fn zero_page() -> Vec<u8> {
    vec![0_u8; page_len()]
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
            Self::CommitChunkWrite => "before commit page write",
            Self::CommitPageTableWrite => "before commit page table write",
            Self::CommitSuperblockStore => "before commit superblock store",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_math_matches_expected_boundaries() {
        assert_eq!(page_count_for_size(0).unwrap(), 0);
        assert_eq!(page_count_for_size(1).unwrap(), 1);
        assert_eq!(page_count_for_size(page_size()).unwrap(), 1);
        assert_eq!(page_count_for_size(page_size() + 1).unwrap(), 2);

        assert_eq!(segment_count_for_pages(0).unwrap(), 0);
        assert_eq!(segment_count_for_pages(1).unwrap(), 1);
        assert_eq!(segment_count_for_pages(SEGMENT_PAGE_COUNT).unwrap(), 1);
        assert_eq!(segment_count_for_pages(SEGMENT_PAGE_COUNT + 1).unwrap(), 2);

        assert_eq!(segment_no(SEGMENT_PAGE_COUNT), 1);
        assert_eq!(segment_index(SEGMENT_PAGE_COUNT - 1).unwrap(), 255);
        assert_eq!(segment_index(SEGMENT_PAGE_COUNT).unwrap(), 0);
        assert_eq!(root_table_bytes(2).unwrap(), 16);
    }

    #[test]
    fn layout_math_rejects_u64_max_overflow_boundaries() {
        assert!(matches!(
            root_table_bytes(u64::MAX),
            Err(StableMemoryError::OffsetOverflow)
        ));
        assert!(matches!(
            checked_add(u64::MAX, 1),
            Err(StableMemoryError::OffsetOverflow)
        ));

        let mut block = Superblock::fresh();
        block.import_base_offset = u64::MAX;
        assert!(matches!(
            import_offset(&block, 1),
            Err(StableMemoryError::OffsetOverflow)
        ));

        block.import_base_offset = u64::MAX - page_size() + 1;
        block.import_total_size = page_size() + 1;
        assert!(matches!(
            imported_page_table(&block),
            Err(StableMemoryError::OffsetOverflow)
        ));
    }
}
