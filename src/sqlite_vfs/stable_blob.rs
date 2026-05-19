//! Logical `/main.db` access backed by segmented stable-memory page mapping.
//!
//! SQLite sees a contiguous file. Internally, the active superblock points to a
//! root table. Each root entry points to a 256-page segment table.

use crate::config::{SQLITE_PAGE_SIZE, STABLE_PAGE_SIZE, SUPERBLOCK_SIZE};
use crate::sqlite_vfs::overlay::{self, Overlay};
use crate::stable::memory::{self, ContextId, StableMemoryError};
use crate::stable::meta::{
    fnv1a64, Superblock, FLAG_CHECKSUM_REFRESHING, FLAG_CHECKSUM_STALE, FLAG_IMPORTING,
    PAGE_MAP_LAYOUT_VERSION,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::mem::MaybeUninit;

const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
const PAGE_TABLE_ENTRY_LEN: u64 = 8;
const SEGMENT_PAGE_COUNT: u64 = 256;
const SEGMENT_TABLE_BYTES: u64 = SEGMENT_PAGE_COUNT * PAGE_TABLE_ENTRY_LEN;
const SINGLE_SEGMENT_PAGE_TABLE_BYTES: u64 = SEGMENT_TABLE_BYTES + PAGE_TABLE_ENTRY_LEN;
const READ_SEGMENT_CACHE_CAPACITY: usize = 8;
const FILE_PAGE_OFFSET_CACHE_CAPACITY: usize = 64;
const FILE_PAGE_DATA_CACHE_CAPACITY: usize = 8;
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
    #[cfg(test)]
    static FAILPOINTS: RefCell<BTreeMap<ContextId, StableBlobFailpoint>> = const { RefCell::new(BTreeMap::new()) };
    static READ_TABLE_CACHE: RefCell<Vec<(ContextId, ReadTableCache)>> = const { RefCell::new(Vec::new()) };
    static COMMIT_SEGMENT_CACHE: RefCell<Vec<(ContextId, CommitSegmentCache)>> = const { RefCell::new(Vec::new()) };
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
    segments: Vec<CachedSegment>,
}

#[derive(Debug)]
struct CachedSegment {
    segment_no: u64,
    table: Vec<u64>,
}

#[derive(Debug)]
struct CommitSegmentCache {
    segment_no: u64,
    segment_offset: u64,
    table: Vec<u64>,
}

impl ReadTableCache {
    fn new() -> Self {
        Self {
            key: None,
            root: Vec::new(),
            segments: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.key = None;
        self.root.clear();
        self.segments.clear();
    }

    fn ensure_key(&mut self, key: ReadCacheKey) {
        if self.key == Some(key) {
            return;
        }
        self.clear();
        self.key = Some(key);
    }

    #[inline(always)]
    fn segment_page_offset(&mut self, segment_no: u64, index: usize) -> Option<u64> {
        if self.segments.is_empty() {
            return None;
        }
        if self.segments.len() == 1 {
            let segment = &self.segments[0];
            if segment.segment_no == segment_no {
                return Some(segment.table[index]);
            }
            return None;
        }
        let position = self
            .segments
            .iter()
            .position(|segment| segment.segment_no == segment_no)?;
        let offset = Some(self.segments[position].table[index]);
        if position + 1 != self.segments.len() {
            let segment = self.segments.remove(position);
            self.segments.push(segment);
        }
        offset
    }

    fn insert_segment(&mut self, segment_no: u64, table: Vec<u64>) {
        if let Some(position) = self
            .segments
            .iter()
            .position(|segment| segment.segment_no == segment_no)
        {
            self.segments.remove(position);
        }
        self.segments.push(CachedSegment { segment_no, table });
        while self.segments.len() > READ_SEGMENT_CACHE_CAPACITY {
            self.segments.remove(0);
        }
    }
}

#[derive(Debug)]
pub(crate) struct PageOffsetCache {
    entries: Vec<(u64, u64)>,
    pages: Vec<(u64, Vec<u8>)>,
}

impl PageOffsetCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(FILE_PAGE_OFFSET_CACHE_CAPACITY),
            pages: Vec::new(),
        }
    }

    fn get(&self, page_no: u64) -> Option<u64> {
        match self.entries.as_slice() {
            [] => None,
            [(cached_page, physical)] => (*cached_page == page_no).then_some(*physical),
            entries => {
                for (cached_page, physical) in entries {
                    if *cached_page == page_no {
                        return Some(*physical);
                    }
                }
                None
            }
        }
    }

    fn insert(&mut self, page_no: u64, physical: u64) {
        if self.entries.len() == FILE_PAGE_OFFSET_CACHE_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push((page_no, physical));
    }

    #[inline(always)]
    fn copy_page_slice(&self, page_no: u64, in_page: usize, dst: &mut [u8]) -> bool {
        if self.pages.is_empty() {
            return false;
        }
        if self.pages.len() == 1 {
            let (cached_page, page) = &self.pages[0];
            if *cached_page == page_no {
                let end = in_page + dst.len();
                dst.copy_from_slice(&page[in_page..end]);
                return true;
            }
            return false;
        }
        for (cached_page, page) in &self.pages {
            if *cached_page == page_no {
                let end = in_page + dst.len();
                dst.copy_from_slice(&page[in_page..end]);
                return true;
            }
        }
        false
    }

    fn insert_page(&mut self, page_no: u64, page: Vec<u8>) {
        if self.pages.len() == FILE_PAGE_DATA_CACHE_CAPACITY {
            self.pages.remove(0);
        }
        self.pages.push((page_no, page));
    }
}

#[cfg(test)]
pub(crate) fn set_failpoint(failpoint: StableBlobFailpoint) {
    if let Ok(context) = memory::active_context_id() {
        FAILPOINTS.with(|slot| {
            slot.borrow_mut().insert(context, failpoint);
        });
    }
}

#[cfg(test)]
pub(crate) fn clear_failpoint() {
    FAILPOINTS.with(|slot| slot.borrow_mut().clear());
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

pub(crate) fn begin_update() -> Result<u64, StableMemoryError> {
    let block = Superblock::load()?;
    if block.layout_version < PAGE_MAP_LAYOUT_VERSION {
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

#[doc(hidden)]
pub fn invalidate_read_cache() {
    READ_TABLE_CACHE.with(|cache| cache.borrow_mut().clear());
    COMMIT_SEGMENT_CACHE.with(|cache| cache.borrow_mut().clear());
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
    if page_no >= active_page_count(&block)? {
        return Ok(page);
    }
    let physical = page_offset_for(&block, page_no)?;
    if physical != 0 {
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_stable_data_read(page.len());
        memory::read_preallocated(physical, &mut page)?;
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
    let non_zero_pages = table.iter().filter(|offset| **offset != 0).count();
    let data_bytes = u64::try_from(non_zero_pages)
        .map_err(|_| StableMemoryError::OffsetOverflow)?
        .checked_mul(page_size())
        .ok_or(StableMemoryError::OffsetOverflow)?;
    memory::ensure_capacity(checked_add(cursor, data_bytes)?)?;

    for offset in table {
        if offset == 0 {
            compacted.push(0);
            continue;
        }
        let mut page = zero_page();
        memory::read_preallocated(offset, &mut page)?;
        memory::write_preallocated(cursor, &page)?;
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
    let profile_enabled = commit_profile_enabled();
    let block = Superblock::load()?;
    let overlay_size = overlay.size();
    let final_page_count = page_count_for_size(overlay_size)?;
    let data_cursor = append_base()?;
    debug_assert!(overlay
        .dirty_pages()
        .iter()
        .all(|(page_no, _)| *page_no < final_page_count));
    let dirty_pages = overlay.dirty_pages();
    if let [(page_no, page)] = dirty_pages {
        if overlay_size >= block.db_size
            && *page_no < final_page_count
            && final_page_count <= SEGMENT_PAGE_COUNT
        {
            let build_profile_start = commit_profile_start(profile_enabled);
            let options = SinglePageCommitOptions {
                advance_tx,
                overlay_size,
                data_cursor,
                profile_enabled,
                build_profile_start,
            };
            return commit_single_segment_page_overlay(&block, *page_no, page, options);
        }
    }

    let final_segment_count = segment_count_for_pages(final_page_count)?;
    let profile_start = commit_profile_start(profile_enabled);
    let mut root = read_commit_root_table(&block, final_segment_count)?;
    commit_profile_record_load(profile_start);

    let build_profile_start = commit_profile_start(profile_enabled);
    let root_len =
        usize::try_from(final_segment_count).map_err(|_| StableMemoryError::OffsetOverflow)?;
    if root.len() != root_len {
        root.resize(root_len, 0);
    }

    if let [(page_no, page)] = dirty_pages {
        if overlay_size >= block.db_size && *page_no < final_page_count {
            let options = SinglePageCommitOptions {
                advance_tx,
                overlay_size,
                data_cursor,
                profile_enabled,
                build_profile_start,
            };
            return commit_single_page_overlay(
                &block,
                final_segment_count,
                root,
                *page_no,
                page,
                options,
            );
        }
    }

    let mut segment_updates = BTreeMap::<u64, Vec<u64>>::new();
    let mut page_cursor = data_cursor;

    for (page_no, _) in dirty_pages {
        if *page_no >= final_page_count {
            continue;
        }
        let segment_no = segment_no(*page_no);
        let index = segment_index(*page_no)?;
        let table = load_segment_for_update(&block, &root, &mut segment_updates, segment_no)?;
        table[index] = page_cursor;
        page_cursor = checked_add(page_cursor, page_size())?;
    }

    if overlay_size < block.db_size {
        clear_truncated_tail(&block, &root, &mut segment_updates, final_page_count)?;
    }
    commit_profile_record_build_segments(build_profile_start);

    let mut table_cursor = page_cursor;
    let root_entries_len = final_segment_count;
    let segment_table_writes = segment_updates.len();
    let segment_table_bytes = u64::try_from(segment_table_writes)
        .map_err(|_| StableMemoryError::OffsetOverflow)?
        .checked_mul(segment_table_bytes()?)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let page_table_bytes = checked_add(segment_table_bytes, root_table_bytes(root_entries_len)?)?;
    let profile_start = commit_profile_start(profile_enabled);
    memory::ensure_capacity(checked_add(table_cursor, page_table_bytes)?)?;
    commit_profile_record_capacity(profile_start);

    let profile_start = commit_profile_start(profile_enabled);
    let mut cursor = data_cursor;
    for (_, page) in dirty_pages {
        hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
        write_commit_page(cursor, page, profile_enabled)?;
        cursor = checked_add(cursor, page_size())?;
    }
    commit_profile_record_page_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitPageTableWrite)?;
    let profile_start = commit_profile_start(profile_enabled);
    for (segment_no, table) in segment_updates {
        let offset = write_commit_segment_table_at(&table, &mut table_cursor, profile_enabled)?;
        let index = usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
        root[index] = offset;
    }
    let root_offset = write_commit_root_table_at(&root, &mut table_cursor, profile_enabled)?;
    commit_profile_record_table_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitSuperblockStore)?;
    let profile_start = commit_profile_start(profile_enabled);
    let result = store_commit_page_map(
        advance_tx,
        root_offset,
        root_entries_len,
        overlay_size,
        profile_enabled,
    );
    commit_profile_record_superblock_store(profile_start);
    result
}

#[derive(Clone, Copy)]
struct SinglePageCommitOptions {
    advance_tx: bool,
    overlay_size: u64,
    data_cursor: u64,
    profile_enabled: bool,
    build_profile_start: Option<u64>,
}

fn commit_single_page_overlay(
    block: &Superblock,
    final_segment_count: u64,
    mut root: Vec<u64>,
    page_no: u64,
    page: &[u8],
    options: SinglePageCommitOptions,
) -> Result<(), StableMemoryError> {
    let segment_no = segment_no(page_no);
    let index = segment_index(page_no)?;
    let mut table = read_commit_segment_table(block, &root, segment_no)?;
    table[index] = options.data_cursor;
    let page_cursor = checked_add(options.data_cursor, page_size())?;
    commit_profile_record_build_segments(options.build_profile_start);

    let root_entries_len = final_segment_count;
    let page_table_bytes =
        checked_add(segment_table_bytes()?, root_table_bytes(root_entries_len)?)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    memory::ensure_capacity(checked_add(page_cursor, page_table_bytes)?)?;
    commit_profile_record_capacity(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    write_commit_page(options.data_cursor, page, options.profile_enabled)?;
    commit_profile_record_page_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitPageTableWrite)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    let mut table_cursor = page_cursor;
    let offset = write_commit_segment_table_at(&table, &mut table_cursor, options.profile_enabled)?;
    let root_offset = if final_segment_count == 1 {
        write_commit_root_table_at(&[offset], &mut table_cursor, options.profile_enabled)?
    } else {
        let root_index =
            usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
        root[root_index] = offset;
        write_commit_root_table_at(&root, &mut table_cursor, options.profile_enabled)?
    };
    commit_profile_record_table_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitSuperblockStore)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    let result = store_commit_page_map(
        options.advance_tx,
        root_offset,
        root_entries_len,
        options.overlay_size,
        options.profile_enabled,
    );
    commit_profile_record_superblock_store(profile_start);
    if result.is_ok() {
        cache_commit_segment_table(segment_no, offset, table);
    }
    result
}

fn commit_single_segment_page_overlay(
    block: &Superblock,
    page_no: u64,
    page: &[u8],
    options: SinglePageCommitOptions,
) -> Result<(), StableMemoryError> {
    let index = segment_index(page_no)?;
    let segment_offset = if block.page_table_offset != 0 {
        block
            .page_table_offset
            .checked_sub(SEGMENT_TABLE_BYTES)
            .ok_or(StableMemoryError::OffsetOverflow)?
    } else {
        0
    };
    let mut table = read_single_commit_segment_table_at(segment_offset)?;
    table[index] = options.data_cursor;
    let page_cursor = checked_add(options.data_cursor, page_size())?;
    commit_profile_record_build_segments(options.build_profile_start);

    let profile_start = commit_profile_start(options.profile_enabled);
    memory::ensure_capacity(checked_add(page_cursor, SINGLE_SEGMENT_PAGE_TABLE_BYTES)?)?;
    commit_profile_record_capacity(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitChunkWrite)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    memory::write_prechecked(options.data_cursor, page)?;
    commit_profile_record_page_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitPageTableWrite)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    let mut table_cursor = page_cursor;
    let offset = write_commit_segment_table_at(&table, &mut table_cursor, options.profile_enabled)?;
    let root_offset =
        write_commit_root_table_at(&[offset], &mut table_cursor, options.profile_enabled)?;
    commit_profile_record_table_write(profile_start);

    hit_failpoint(StableBlobFailpoint::CommitSuperblockStore)?;
    let profile_start = commit_profile_start(options.profile_enabled);
    let result = store_commit_page_map(
        options.advance_tx,
        root_offset,
        1,
        options.overlay_size,
        options.profile_enabled,
    );
    commit_profile_record_superblock_store(profile_start);
    if result.is_ok() {
        cache_single_commit_segment_table(offset, table);
    }
    result
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

#[inline(always)]
fn commit_profile_start(enabled: bool) -> Option<u64> {
    if enabled {
        Some(crate::read_metrics::instruction_counter())
    } else {
        None
    }
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

commit_profile_recorder!(commit_profile_record_load, record_commit_load);
commit_profile_recorder!(
    commit_profile_record_build_segments,
    record_commit_build_segments
);
commit_profile_recorder!(commit_profile_record_capacity, record_commit_capacity);
commit_profile_recorder!(commit_profile_record_page_write, record_commit_page_write);
commit_profile_recorder!(commit_profile_record_table_write, record_commit_table_write);
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

fn store_commit_page_map(
    advance_tx: bool,
    root_offset: u64,
    root_entries_len: u64,
    overlay_size: u64,
    profile_enabled: bool,
) -> Result<(), StableMemoryError> {
    match (advance_tx, profile_enabled) {
        (true, true) => Superblock::commit_page_map(root_offset, root_entries_len, overlay_size),
        (true, false) => {
            Superblock::commit_page_map_unmetered(root_offset, root_entries_len, overlay_size)
        }
        (false, true) => {
            Superblock::store_page_map_without_tx(root_offset, root_entries_len, overlay_size)
        }
        (false, false) => Superblock::store_page_map_without_tx_unmetered(
            root_offset,
            root_entries_len,
            overlay_size,
        ),
    }
}

fn load_segment_for_update<'a>(
    block: &Superblock,
    root: &[u64],
    updates: &'a mut BTreeMap<u64, Vec<u64>>,
    segment_no: u64,
) -> Result<&'a mut Vec<u64>, StableMemoryError> {
    match updates.entry(segment_no) {
        std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::btree_map::Entry::Vacant(entry) => {
            let table = read_segment_table(block, root, segment_no)?;
            Ok(entry.insert(table))
        }
    }
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

fn read_logical_range_with_page_cache(
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
    if dst.len() < page_len() && page_offsets.copy_page_slice(page_no, in_page, dst) {
        return Ok(());
    }
    let physical = match page_offsets.get(page_no) {
        Some(physical) => physical,
        None => {
            let physical = if block.page_table_offset == 0 {
                0
            } else {
                cached_page_offset_for(block, page_no)?
            };
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
    let context = memory::active_context_id()?;
    let key = read_cache_key(block);
    let segment_no = segment_no(page_no);
    let index = segment_index(page_no)?;
    READ_TABLE_CACHE.with(|cache| {
        let mut caches = cache.borrow_mut();
        let cache = match read_table_cache_index(&caches, context) {
            Some(index) => &mut caches[index].1,
            None => {
                caches.push((context, ReadTableCache::new()));
                &mut caches
                    .last_mut()
                    .ok_or(StableMemoryError::OffsetOverflow)?
                    .1
            }
        };
        cache.ensure_key(key);
        if cache.root.is_empty() {
            #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
            crate::read_metrics::record_page_table_root_miss();
            cache.root = read_root_table(block)?;
        } else {
            #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
            crate::read_metrics::record_page_table_root_hit();
        }
        let root_index =
            usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let segment_offset = cache.root[root_index];
        if segment_offset == 0 {
            return Ok(0);
        }
        if let Some(offset) = cache.segment_page_offset(segment_no, index) {
            #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
            crate::read_metrics::record_page_table_segment_hit();
            return Ok(offset);
        }
        #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
        crate::read_metrics::record_page_table_segment_miss();
        let table = read_segment_table_at(segment_offset)?;
        let offset = table[index];
        cache.insert_segment(segment_no, table);
        Ok(offset)
    })
}

fn read_table_cache_index(
    caches: &[(ContextId, ReadTableCache)],
    context: ContextId,
) -> Option<usize> {
    caches
        .iter()
        .position(|(stored_context, _)| *stored_context == context)
}

fn read_root_table(block: &Superblock) -> Result<Vec<u64>, StableMemoryError> {
    if block.page_count == 0 {
        return Ok(Vec::new());
    }
    let entries_len =
        usize::try_from(block.page_count).map_err(|_| StableMemoryError::OffsetOverflow)?;
    read_u64_table_at(block.page_table_offset, entries_len)
}

fn read_commit_root_table(
    block: &Superblock,
    final_segment_count: u64,
) -> Result<Vec<u64>, StableMemoryError> {
    if final_segment_count == 1 && block.page_count == 1 && block.page_table_offset != 0 {
        let segment_offset = block
            .page_table_offset
            .checked_sub(segment_table_bytes()?)
            .ok_or(StableMemoryError::OffsetOverflow)?;
        return Ok(vec![segment_offset]);
    }
    read_root_table(block)
}

fn read_segment_table(
    _block: &Superblock,
    root: &[u64],
    segment_no: u64,
) -> Result<Vec<u64>, StableMemoryError> {
    let index = usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let Some(offset) = root.get(index).copied() else {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    };
    if offset == 0 {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    }
    read_segment_table_at(offset)
}

fn read_commit_segment_table(
    _block: &Superblock,
    root: &[u64],
    segment_no: u64,
) -> Result<Vec<u64>, StableMemoryError> {
    let index = usize::try_from(segment_no).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let Some(offset) = root.get(index).copied() else {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    };
    if offset == 0 {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    }
    read_commit_segment_table_at(segment_no, offset)
}

fn read_commit_segment_table_at(
    segment_no: u64,
    offset: u64,
) -> Result<Vec<u64>, StableMemoryError> {
    if offset == 0 {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    }
    if let Some(table) = take_commit_segment_table(segment_no, offset) {
        return Ok(table);
    }
    read_segment_table_at(offset)
}

#[inline(always)]
fn read_single_commit_segment_table_at(offset: u64) -> Result<Vec<u64>, StableMemoryError> {
    if offset == 0 {
        return Ok(vec![0_u64; segment_page_count_usize()]);
    }
    if let Some(table) = take_single_commit_segment_table(offset) {
        return Ok(table);
    }
    read_segment_table_at(offset)
}

fn take_commit_segment_table(segment_no: u64, segment_offset: u64) -> Option<Vec<u64>> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    COMMIT_SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() == 1 {
            let (stored_context, cached) = &cache[0];
            if *stored_context == context
                && cached.segment_no == segment_no
                && cached.segment_offset == segment_offset
            {
                return cache.pop().map(|(_, cached)| cached.table);
            }
            return None;
        }
        cache
            .iter()
            .position(|(stored_context, cached)| {
                *stored_context == context
                    && cached.segment_no == segment_no
                    && cached.segment_offset == segment_offset
            })
            .map(|position| cache.remove(position).1.table)
    })
}

#[inline(always)]
fn take_single_commit_segment_table(segment_offset: u64) -> Option<Vec<u64>> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    COMMIT_SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() == 1 {
            let (stored_context, cached) = &cache[0];
            if *stored_context == context && cached.segment_offset == segment_offset {
                return cache.pop().map(|(_, cached)| cached.table);
            }
            return None;
        }
        cache
            .iter()
            .position(|(stored_context, cached)| {
                *stored_context == context && cached.segment_offset == segment_offset
            })
            .map(|position| cache.remove(position).1.table)
    })
}

fn cache_commit_segment_table(segment_no: u64, segment_offset: u64, table: Vec<u64>) {
    let Ok(context) = memory::active_context_id() else {
        return;
    };
    COMMIT_SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.is_empty() {
            cache.push((
                context,
                CommitSegmentCache {
                    segment_no,
                    segment_offset,
                    table,
                },
            ));
            return;
        }
        if cache.len() == 1 {
            let (stored_context, cached) = &mut cache[0];
            if *stored_context == context {
                cached.segment_no = segment_no;
                cached.segment_offset = segment_offset;
                cached.table = table;
                return;
            }
        } else if let Some((_, cached)) = cache
            .iter_mut()
            .find(|(stored_context, _)| *stored_context == context)
        {
            cached.segment_no = segment_no;
            cached.segment_offset = segment_offset;
            cached.table = table;
            return;
        }
        cache.push((
            context,
            CommitSegmentCache {
                segment_no,
                segment_offset,
                table,
            },
        ));
    });
}

#[inline(always)]
fn cache_single_commit_segment_table(segment_offset: u64, table: Vec<u64>) {
    let Ok(context) = memory::active_context_id() else {
        return;
    };
    COMMIT_SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.is_empty() {
            cache.push((
                context,
                CommitSegmentCache {
                    segment_no: 0,
                    segment_offset,
                    table,
                },
            ));
            return;
        }
        if cache.len() == 1 {
            let (stored_context, cached) = &mut cache[0];
            if *stored_context == context {
                cached.segment_no = 0;
                cached.segment_offset = segment_offset;
                cached.table = table;
                return;
            }
        } else if let Some((_, cached)) = cache
            .iter_mut()
            .find(|(stored_context, _)| *stored_context == context)
        {
            cached.segment_no = 0;
            cached.segment_offset = segment_offset;
            cached.table = table;
            return;
        }
        cache.push((
            context,
            CommitSegmentCache {
                segment_no: 0,
                segment_offset,
                table,
            },
        ));
    });
}

fn read_segment_table_at(offset: u64) -> Result<Vec<u64>, StableMemoryError> {
    read_u64_table_at(offset, segment_page_count_usize())
}

fn write_segmented_tables(entries: &[u64]) -> Result<(u64, u64), StableMemoryError> {
    if entries.is_empty() {
        return Ok((0, 0));
    }
    let root_len = segment_count_for_pages(entries_len_u64(entries)?)?;
    let mut cursor = append_base()?;
    let segment_bytes = root_len
        .checked_mul(segment_table_bytes()?)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let page_table_bytes = checked_add(segment_bytes, root_table_bytes(root_len)?)?;
    memory::ensure_capacity(checked_add(cursor, page_table_bytes)?)?;
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
        root.push(write_segment_table_at(&table, &mut cursor)?);
    }
    let root_offset = write_root_table_at(&root, &mut cursor)?;
    Ok((root_offset, entries_len_u64(&root)?))
}

#[inline(always)]
fn write_segment_table_at(entries: &[u64], cursor: &mut u64) -> Result<u64, StableMemoryError> {
    if entries.len() == segment_page_count_usize() {
        return write_u64_table_at(entries, cursor);
    }

    let mut table = vec![0_u64; segment_page_count_usize()];
    for (index, entry) in entries.iter().take(segment_page_count_usize()).enumerate() {
        table[index] = *entry;
    }
    write_u64_table_at(&table, cursor)
}

fn write_root_table_at(entries: &[u64], cursor: &mut u64) -> Result<u64, StableMemoryError> {
    write_u64_table_at(entries, cursor)
}

#[inline(always)]
fn write_commit_segment_table_at(
    entries: &[u64],
    cursor: &mut u64,
    profile_enabled: bool,
) -> Result<u64, StableMemoryError> {
    if profile_enabled {
        write_segment_table_at(entries, cursor)
    } else {
        write_segment_table_at_unmetered(entries, cursor)
    }
}

#[inline(always)]
fn write_commit_root_table_at(
    entries: &[u64],
    cursor: &mut u64,
    profile_enabled: bool,
) -> Result<u64, StableMemoryError> {
    if profile_enabled {
        write_root_table_at(entries, cursor)
    } else {
        write_u64_table_at_unmetered(entries, cursor)
    }
}

fn write_segment_table_at_unmetered(
    entries: &[u64],
    cursor: &mut u64,
) -> Result<u64, StableMemoryError> {
    if entries.len() == segment_page_count_usize() {
        return write_u64_table_at_unmetered(entries, cursor);
    }

    let mut table = vec![0_u64; segment_page_count_usize()];
    for (index, entry) in entries.iter().take(segment_page_count_usize()).enumerate() {
        table[index] = *entry;
    }
    write_u64_table_at_unmetered(&table, cursor)
}

fn write_u64_table_at(entries: &[u64], cursor: &mut u64) -> Result<u64, StableMemoryError> {
    if entries.is_empty() {
        return Ok(0);
    }
    let offset = *cursor;
    let byte_len = entries
        .len()
        .checked_mul(8)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    #[cfg(target_endian = "little")]
    {
        // SAFETY: page-table encoding is little-endian u64 and the target is little-endian.
        let bytes = unsafe { std::slice::from_raw_parts(entries.as_ptr().cast::<u8>(), byte_len) };
        memory::write_prechecked(offset, bytes)?;
        *cursor = checked_add(
            offset,
            u64::try_from(byte_len).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        Ok(offset)
    }

    #[cfg(not(target_endian = "little"))]
    {
        let mut bytes = vec![0_u8; byte_len];
        for (chunk, entry) in bytes.chunks_exact_mut(8).zip(entries) {
            chunk.copy_from_slice(&entry.to_le_bytes());
        }
        memory::write_prechecked(offset, &bytes)?;
        *cursor = checked_add(
            offset,
            u64::try_from(byte_len).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        Ok(offset)
    }
}

fn read_u64_table_at(offset: u64, entries_len: usize) -> Result<Vec<u64>, StableMemoryError> {
    if entries_len == 0 {
        return Ok(Vec::new());
    }
    let byte_len = entries_len
        .checked_mul(8)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    #[cfg(target_endian = "little")]
    {
        let mut entries = Vec::<MaybeUninit<u64>>::with_capacity(entries_len);
        unsafe {
            entries.set_len(entries_len);
        }
        // SAFETY: the buffer has `entries_len` u64 slots. The stable-memory read
        // fills every byte before conversion to initialized `u64` values.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(entries.as_mut_ptr().cast::<u8>(), byte_len) };
        memory::read_preallocated(offset, bytes)?;
        let ptr = entries.as_mut_ptr().cast::<u64>();
        let len = entries.len();
        let capacity = entries.capacity();
        std::mem::forget(entries);
        // SAFETY: all bytes were just initialized by `read_preallocated`, and
        // every bit pattern is valid for `u64`.
        unsafe { Ok(Vec::from_raw_parts(ptr, len, capacity)) }
    }

    #[cfg(not(target_endian = "little"))]
    {
        let mut bytes = vec![0_u8; byte_len];
        memory::read_preallocated(offset, &mut bytes)?;
        decode_u64_table(&bytes)
    }
}

fn write_u64_table_at_unmetered(
    entries: &[u64],
    cursor: &mut u64,
) -> Result<u64, StableMemoryError> {
    if entries.is_empty() {
        return Ok(0);
    }
    let offset = *cursor;
    let byte_len = entries
        .len()
        .checked_mul(8)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    #[cfg(target_endian = "little")]
    {
        // SAFETY: page-table encoding is little-endian u64 and the target is little-endian.
        let bytes = unsafe { std::slice::from_raw_parts(entries.as_ptr().cast::<u8>(), byte_len) };
        memory::write_prechecked_unmetered(offset, bytes)?;
        *cursor = checked_add(
            offset,
            u64::try_from(byte_len).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        Ok(offset)
    }

    #[cfg(not(target_endian = "little"))]
    {
        let mut bytes = vec![0_u8; byte_len];
        for (chunk, entry) in bytes.chunks_exact_mut(8).zip(entries) {
            chunk.copy_from_slice(&entry.to_le_bytes());
        }
        memory::write_prechecked_unmetered(offset, &bytes)?;
        *cursor = checked_add(
            offset,
            u64::try_from(byte_len).map_err(|_| StableMemoryError::OffsetOverflow)?,
        )?;
        Ok(offset)
    }
}

#[cfg(not(target_endian = "little"))]
fn decode_u64_table(bytes: &[u8]) -> Result<Vec<u64>, StableMemoryError> {
    if !bytes.len().is_multiple_of(8) {
        return Err(StableMemoryError::OffsetOverflow);
    }
    let mut entries = Vec::with_capacity(bytes.len() / 8);
    for chunk in bytes.chunks_exact(8) {
        entries.push(u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]));
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
        memory::read_preallocated(checked_add(base_offset, offset)?, &mut bytes)?;
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

#[cfg(test)]
fn hit_failpoint(failpoint: StableBlobFailpoint) -> Result<(), StableMemoryError> {
    let Ok(context) = memory::active_context_id() else {
        return Ok(());
    };
    FAILPOINTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.get(&context).copied() == Some(failpoint) {
            slot.remove(&context);
            Err(StableMemoryError::Failpoint(failpoint.name()))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
fn hit_failpoint(_failpoint: StableBlobFailpoint) -> Result<(), StableMemoryError> {
    Ok(())
}

#[cfg(test)]
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

    #[test]
    #[serial_test::serial]
    fn read_metrics_separate_table_cache_from_data_reads() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();
        invalidate_read_cache();

        let page = vec![7_u8; page_len()];
        write_at(0, &page).unwrap();
        invalidate_read_cache();
        crate::read_metrics::reset_read_metrics();

        let first = read_base_page(0).unwrap();
        let second = read_base_page(0).unwrap();
        let metrics = crate::read_metrics::read_metrics_snapshot();

        assert_eq!(first, page);
        assert_eq!(second, page);
        assert!(metrics.stable_data_read_calls >= 2);
        assert!(metrics.stable_data_read_bytes >= page_size() * 2);
        assert!(metrics.page_table_root_misses >= 1);
        assert!(metrics.page_table_root_hits >= 1);
        assert!(metrics.page_table_segment_misses >= 1);
        assert!(metrics.page_table_segment_hits >= 1);
        #[cfg(feature = "bench-profile")]
        assert!(metrics.superblock_loads <= 1);
        #[cfg(not(feature = "bench-profile"))]
        assert_eq!(metrics.superblock_loads, 0);
    }

    #[test]
    #[serial_test::serial]
    fn page_offset_cache_reuses_page_data_for_small_reads() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();
        invalidate_read_cache();

        let page = vec![9_u8; page_len()];
        write_at(0, &page).unwrap();
        let block = Superblock::load().unwrap();
        let mut cache = PageOffsetCache::new();
        let mut first = [0_u8; 16];
        let mut second = [0_u8; 16];

        crate::read_metrics::reset_read_metrics();
        read_base_at_with_page_cache(&block, 0, &mut first, &mut cache).unwrap();
        read_base_at_with_page_cache(&block, 8, &mut second, &mut cache).unwrap();
        let metrics = crate::read_metrics::read_metrics_snapshot();

        assert_eq!(first, [9_u8; 16]);
        assert_eq!(second, [9_u8; 16]);
        assert_eq!(metrics.stable_data_read_calls, 1);
        assert_eq!(metrics.stable_data_read_bytes, page_size());
    }
}
