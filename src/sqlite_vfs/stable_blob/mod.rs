//! Logical `/main.db` access backed by an in-place stable-memory image.
//!
//! SQLite sees a contiguous file. Internally, logical page `n` lives at
//! `db_base_offset + n * SQLITE_PAGE_SIZE`.

use crate::config::{SQLITE_PAGE_SIZE, STABLE_PAGE_SIZE, SUPERBLOCK_SIZE};
use crate::sqlite_vfs::overlay::{self, Overlay};
#[cfg(test)]
use crate::stable::memory::ContextId;
use crate::stable::memory::{self, StableMemoryError};
use crate::stable::meta::{
    fnv1a64, Superblock, ZeroExtent, CURRENT_LAYOUT_VERSION, FLAG_CHECKSUM_REFRESHING,
    FLAG_CHECKSUM_STALE, FLAG_IMPORTING, MAX_ZERO_EXTENTS,
};
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::BTreeMap;

const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
#[cfg(test)]
const FAR_PAGE_NO: u64 = 257;
const FILE_PAGE_OFFSET_CACHE_CAPACITY: usize = 64;
const FILE_PAGE_DATA_CACHE_CAPACITY: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChecksumRefresh {
    pub complete: bool,
    pub checksum: u64,
    pub scanned_bytes: u64,
    pub db_size: u64,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageStats {
    pub layout_version: u64,
    pub page_count: u64,
    pub page_table_bytes: u64,
    pub zero_extent_count: u64,
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
    CommitSuperblockStore,
}

thread_local! {
    #[cfg(test)]
    static FAILPOINTS: RefCell<BTreeMap<ContextId, StableBlobFailpoint>> = const { RefCell::new(BTreeMap::new()) };
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

pub(crate) fn page_count_for_size(size: u64) -> Result<u64, StableMemoryError> {
    Ok(size.div_ceil(page_size()))
}

fn commit_overlay(overlay: Overlay, advance_tx: bool) -> Result<(), StableMemoryError> {
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

fn zero_extents_after_commit(
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

fn enforce_temporary_zero_extent_limit(
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
fn add_zero_extent(
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
fn subtract_zero_extent(
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
fn normalize_zero_extents(extents: &mut Vec<ZeroExtent>) -> Result<(), StableMemoryError> {
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

fn page_physical_offset(block: &Superblock, page_no: u64) -> Result<u64, StableMemoryError> {
    checked_add(
        block.db_base_offset,
        page_no
            .checked_mul(page_size())
            .ok_or(StableMemoryError::OffsetOverflow)?,
    )
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

#[allow(dead_code)]
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

#[allow(dead_code)]
fn clear_import(block: &mut Superblock) -> Result<(), StableMemoryError> {
    block.flags &= !FLAG_IMPORTING;
    block.import_expected_checksum = 0;
    block.import_written_until = 0;
    block.import_total_size = 0;
    block.import_base_offset = 0;
    block.store()?;
    Ok(())
}

#[allow(dead_code)]
fn import_offset(block: &Superblock, offset: u64) -> Result<u64, StableMemoryError> {
    checked_add(block.import_base_offset, offset)
}

fn active_page_count(block: &Superblock) -> Result<u64, StableMemoryError> {
    page_count_for_size(block.db_size)
}

#[allow(dead_code)]
fn append_base() -> Result<u64, StableMemoryError> {
    // Fresh bases must append at the high-water mark; never reuse stale physical gaps.
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
            Self::CommitSuperblockStore => "before commit superblock store",
        }
    }
}

#[cfg(test)]
mod tests;
