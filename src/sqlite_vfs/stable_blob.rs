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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};
    use std::collections::BTreeSet;

    #[test]
    fn layout_math_matches_expected_boundaries() {
        assert_eq!(page_count_for_size(0).unwrap(), 0);
        assert_eq!(page_count_for_size(1).unwrap(), 1);
        assert_eq!(page_count_for_size(page_size()).unwrap(), 1);
        assert_eq!(page_count_for_size(page_size() + 1).unwrap(), 2);
    }

    #[test]
    fn layout_math_rejects_u64_max_overflow_boundaries() {
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

        block.db_base_offset = u64::MAX - page_size() + 1;
        assert!(matches!(
            page_physical_offset(&block, 1),
            Err(StableMemoryError::OffsetOverflow)
        ));
    }

    #[test]
    fn pbt_layout_math_matches_verus_model() {
        let mut runner = TestRunner::new(Config {
            cases: 512,
            ..Config::default()
        });

        runner
            .run(
                &(boundary_size_strategy(), boundary_page_strategy()),
                |(size, page_no)| {
                    let page_count = page_count_for_size(size).unwrap();
                    let page_size = u128::from(page_size());
                    if size == 0 {
                        prop_assert_eq!(page_count, 0);
                    } else {
                        prop_assert!(u128::from(page_count - 1) * page_size < u128::from(size));
                        prop_assert!(u128::from(size) <= u128::from(page_count) * page_size);
                    }

                    let mut block = Superblock::fresh();
                    block.db_base_offset = SUPERBLOCK_SIZE;
                    let expected = u128::from(SUPERBLOCK_SIZE) + u128::from(page_no) * page_size;
                    match page_physical_offset(&block, page_no) {
                        Ok(physical) => prop_assert_eq!(u128::from(physical), expected),
                        Err(StableMemoryError::OffsetOverflow) => {
                            prop_assert!(expected > u128::from(u64::MAX));
                        }
                        Err(error) => return Err(TestCaseError::fail(error.to_string())),
                    }
                    Ok(())
                },
            )
            .unwrap();
    }

    fn boundary_size_strategy() -> impl Strategy<Value = u64> {
        let page = page_size();
        let far_page_bytes = FAR_PAGE_NO * page;
        prop_oneof![
            any::<u64>(),
            prop::sample::select(boundary_values(&[
                0,
                1,
                page - 1,
                page,
                page + 1,
                far_page_bytes - 1,
                far_page_bytes,
                far_page_bytes + 1,
                u64::MAX,
            ])),
        ]
    }

    fn boundary_page_strategy() -> impl Strategy<Value = u64> {
        prop_oneof![
            any::<u64>(),
            prop::sample::select(boundary_values(&[
                0,
                1,
                FAR_PAGE_NO - 1,
                FAR_PAGE_NO,
                FAR_PAGE_NO + 1,
                u64::MAX,
            ])),
        ]
    }

    fn boundary_values(values: &[u64]) -> Vec<u64> {
        values
            .iter()
            .flat_map(|value| [value.saturating_sub(1), *value, value.saturating_add(1)])
            .collect()
    }

    #[test]
    fn fnv_fold_matches_one_pass_for_multiple_partitions() {
        let bytes: Vec<u8> = (0..97)
            .map(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
            .collect();
        let expected = fnv1a64(&bytes);

        for split in [0_usize, 1, 2, 7, 31, 64, bytes.len()] {
            let split = split.min(bytes.len());
            let mut hash = fnv1a64(&[]);
            hash = fold_fnv1a64(hash, &bytes[..split]);
            hash = fold_fnv1a64(hash, &bytes[split..]);
            assert_eq!(hash, expected);
        }

        let mut hash = fnv1a64(&[]);
        for chunk in bytes.chunks(13) {
            hash = fold_fnv1a64(hash, chunk);
        }
        assert_eq!(hash, expected);
    }

    #[test]
    #[serial_test::serial]
    fn in_place_commit_keeps_dirty_page_offsets_stable() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let page_zero = vec![1_u8; page_len()];
        let page_later = vec![2_u8; page_len()];
        let later_page_no = FAR_PAGE_NO;
        write_at(0, &page_zero).unwrap();
        write_at(later_page_no * page_size(), &page_later).unwrap();

        let block = Superblock::load().unwrap();
        let old_page_zero_offset = page_physical_offset(&block, 0).unwrap();
        let old_later_offset = page_physical_offset(&block, later_page_no).unwrap();

        let updated_page_zero = vec![3_u8; page_len()];
        write_at(0, &updated_page_zero).unwrap();
        let updated_block = Superblock::load().unwrap();
        let mut out = vec![0_u8; page_len()];
        read_base_at(0, &mut out).unwrap();

        assert_eq!(
            page_physical_offset(&updated_block, 0).unwrap(),
            old_page_zero_offset
        );
        assert_eq!(
            page_physical_offset(&updated_block, later_page_no).unwrap(),
            old_later_offset
        );
        assert_eq!(out, updated_page_zero);
    }

    #[test]
    #[serial_test::serial]
    fn in_place_commit_tracks_multi_segment_dirty_and_clean_pages() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let clean_page_no = 1;
        let later_page_no = FAR_PAGE_NO;
        write_at(0, &vec![1_u8; page_len()]).unwrap();
        write_at(clean_page_no * page_size(), &vec![2_u8; page_len()]).unwrap();
        write_at(later_page_no * page_size(), &vec![3_u8; page_len()]).unwrap();

        let before = Superblock::load().unwrap();
        let before_first = page_physical_offset(&before, 0).unwrap();
        let before_clean = page_physical_offset(&before, clean_page_no).unwrap();
        let before_later = page_physical_offset(&before, later_page_no).unwrap();

        begin_update().unwrap();
        write_at(0, &vec![4_u8; page_len()]).unwrap();
        write_at(later_page_no * page_size(), &vec![5_u8; page_len()]).unwrap();
        commit_update().unwrap();

        let after = Superblock::load().unwrap();

        assert_eq!(after.page_count, active_page_count(&after).unwrap());
        assert_eq!(page_physical_offset(&after, 0).unwrap(), before_first);
        assert_eq!(
            page_physical_offset(&after, clean_page_no).unwrap(),
            before_clean
        );
        assert_eq!(
            page_physical_offset(&after, later_page_no).unwrap(),
            before_later
        );
    }

    #[test]
    #[serial_test::serial]
    fn in_place_truncate_hides_old_tail_pages() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        write_at(0, &vec![1_u8; page_len()]).unwrap();
        write_at(page_size(), &vec![2_u8; page_len()]).unwrap();
        write_at(2 * page_size(), &vec![3_u8; page_len()]).unwrap();
        truncate(page_size()).unwrap();

        let block = Superblock::load().unwrap();
        let mut hidden = vec![1_u8; page_len()];
        read_base_at(page_size(), &mut hidden).unwrap();

        assert_eq!(block.db_size, page_size());
        assert_eq!(block.page_count, 1);
        assert_eq!(block.zero_extent_count(), 1);
        assert_eq!(hidden, vec![0_u8; page_len()]);
    }

    #[test]
    #[serial_test::serial]
    fn same_overlay_truncate_reextend_zeroes_active_gap() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        write_at(0, &vec![1_u8; page_len()]).unwrap();
        write_at(page_size(), &vec![0xAA; page_len()]).unwrap();
        write_at(2 * page_size(), &vec![3_u8; page_len()]).unwrap();

        begin_update().unwrap();
        truncate(page_size()).unwrap();
        truncate(2 * page_size()).unwrap();
        let mut overlay_gap = vec![1_u8; page_len()];
        read_at(page_size(), &mut overlay_gap).unwrap();
        commit_update().unwrap();

        let committed_gap = export_chunk(page_size(), page_size()).unwrap();

        assert_eq!(overlay_gap, vec![0_u8; page_len()]);
        assert_eq!(committed_gap, vec![0_u8; page_len()]);
    }

    #[test]
    #[serial_test::serial]
    fn compact_is_noop_for_in_place_layout() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let later_page_no = FAR_PAGE_NO;
        let first_page = vec![7_u8; page_len()];
        let later_page = vec![9_u8; page_len()];
        write_at(0, &first_page).unwrap();
        write_at(later_page_no * page_size(), &later_page).unwrap();

        compact().unwrap();

        let block = Superblock::load().unwrap();
        let mut first_out = vec![0_u8; page_len()];
        let mut later_out = vec![0_u8; page_len()];

        read_base_at(0, &mut first_out).unwrap();
        read_base_at(later_page_no * page_size(), &mut later_out).unwrap();

        assert_eq!(
            page_physical_offset(&block, later_page_no).unwrap(),
            block.db_base_offset + later_page_no * page_size()
        );
        assert_eq!(first_out, first_page);
        assert_eq!(later_out, later_page);
    }

    #[test]
    #[serial_test::serial]
    fn in_place_expand_only_commit_zeroes_new_logical_tail() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        write_at(0, &[0]).unwrap();
        truncate(page_size() * 4).unwrap();
        truncate(page_size() * 4 + 1).unwrap();

        let block = Superblock::load().unwrap();
        let mut first = [1_u8; 1];
        let mut expanded_tail = [1_u8; 1];

        read_base_at(0, &mut first).unwrap();
        read_base_at(page_size() * 4, &mut expanded_tail).unwrap();

        assert_eq!(block.db_size, page_size() * 4 + 1);
        assert_eq!(
            page_physical_offset(&block, 0).unwrap(),
            block.db_base_offset
        );
        assert_eq!(
            page_physical_offset(&block, 4).unwrap(),
            block.db_base_offset + 4 * page_size()
        );
        assert_eq!(first, [0]);
        assert_eq!(expanded_tail, [0]);
    }

    #[test]
    #[serial_test::serial]
    fn truncate_grow_zeroes_stale_physical_gap() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let old_size = page_size() + 17;
        let grow_len = page_size() * 2 + 111;
        let grow_len_usize = usize::try_from(grow_len).unwrap();
        write_at(old_size - 1, b"a").unwrap();
        let block = Superblock::load().unwrap();
        crate::stable::memory::write(block.db_base_offset + old_size, &vec![0xAA; grow_len_usize])
            .unwrap();

        truncate(old_size + grow_len).unwrap();
        let gap = export_chunk(old_size, grow_len).unwrap();

        assert!(gap.iter().all(|byte| *byte == 0));
    }

    #[test]
    #[serial_test::serial]
    fn sparse_write_zeroes_stale_physical_gap() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let old_size = page_size() + 17;
        let write_offset = old_size + page_size() * 2 + 33;
        let gap_len = write_offset - old_size;
        let gap_len_usize = usize::try_from(gap_len).unwrap();
        write_at(old_size - 1, b"a").unwrap();
        let block = Superblock::load().unwrap();
        crate::stable::memory::write(block.db_base_offset + old_size, &vec![0xAA; gap_len_usize])
            .unwrap();

        write_at(write_offset, b"z").unwrap();
        let gap = export_chunk(old_size, gap_len).unwrap();
        let written = export_chunk(write_offset, 1).unwrap();

        assert!(gap.iter().all(|byte| *byte == 0));
        assert_eq!(written, b"z");
    }

    #[test]
    #[serial_test::serial]
    fn failed_import_slack_stays_hidden_after_normal_grow() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        write_at(0, b"a").unwrap();
        begin_import(page_size(), 0).unwrap();
        let importing = Superblock::load().unwrap();
        import_chunk(0, &vec![0xAA; page_len()]).unwrap();
        assert!(finish_import().is_err());

        let block = Superblock::load().unwrap();
        let logical_import_offset = importing.import_base_offset - block.db_base_offset;
        truncate(logical_import_offset + page_size()).unwrap();
        let grown_over_import_slack = export_chunk(logical_import_offset, page_size()).unwrap();

        assert!(grown_over_import_slack.iter().all(|byte| *byte == 0));
    }

    #[test]
    #[serial_test::serial]
    fn zero_mask_hides_truncated_tail_without_moving_base() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        write_at(0, &vec![b'a'; page_len()]).unwrap();
        write_at(page_size(), &vec![b'b'; page_len()]).unwrap();
        let old_base = Superblock::load().unwrap().db_base_offset;

        truncate(1).unwrap();
        let mut stale_tail = [0_u8; 1];
        crate::stable::memory::read_preallocated(old_base + page_size(), &mut stale_tail).unwrap();
        assert_eq!(stale_tail, [b'b']);

        let grow_offset = 2 * page_size() + 2;
        write_at(grow_offset, b"z").unwrap();

        let block = Superblock::load().unwrap();
        let image = export_chunk(0, block.db_size).unwrap();
        let z_index = usize::try_from(grow_offset).unwrap();

        assert_eq!(block.db_base_offset, old_base);
        assert_eq!(block.db_size, grow_offset + 1);
        assert_eq!(block.zero_extent_count(), 1);
        assert_eq!(image[0], b'a');
        assert!(image[1..z_index].iter().all(|byte| *byte == 0));
        assert_eq!(image[z_index], b'z');
    }

    #[test]
    fn zero_extent_add_subtract_merges_and_splits_ranges() {
        let mut extents = Vec::new();
        add_zero_extent(&mut extents, 10, 20).unwrap();
        add_zero_extent(&mut extents, 30, 40).unwrap();
        add_zero_extent(&mut extents, 20, 30).unwrap();
        assert_eq!(
            extents,
            vec![ZeroExtent {
                start_page: 10,
                end_page: 40
            }]
        );

        subtract_zero_extent(&mut extents, 15, 17).unwrap();
        assert_eq!(
            extents,
            vec![
                ZeroExtent {
                    start_page: 10,
                    end_page: 15
                },
                ZeroExtent {
                    start_page: 17,
                    end_page: 40
                }
            ]
        );

        subtract_zero_extent(&mut extents, 8, 18).unwrap();
        assert_eq!(
            extents,
            vec![ZeroExtent {
                start_page: 18,
                end_page: 40
            }]
        );
    }

    #[test]
    fn zero_extent_truncate_tail_merges_with_existing_range() {
        let mut extents = vec![ZeroExtent {
            start_page: 10,
            end_page: 20,
        }];

        add_zero_extent(&mut extents, 10, 40).unwrap();

        assert_eq!(
            extents,
            vec![ZeroExtent {
                start_page: 10,
                end_page: 40
            }]
        );
    }

    #[test]
    fn zero_extent_commit_allows_temporary_limit_excess_removed_by_dirty_page() {
        let mut block = Superblock::fresh();
        block.db_size = (MAX_ZERO_EXTENTS as u64) * 2 * page_size();
        block.zero_extents = (0..MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: (index as u64) * 2,
                end_page: (index as u64) * 2 + 1,
            })
            .collect();

        let mut overlay = Overlay::new(block.db_size);
        overlay
            .write_at(block.db_size, &vec![0xAA; page_len()])
            .unwrap();
        let final_page_count = page_count_for_size(overlay.size()).unwrap();

        let extents = zero_extents_after_commit(&block, &overlay, final_page_count).unwrap();

        assert_eq!(extents.len(), MAX_ZERO_EXTENTS);
        assert_eq!(extents, block.zero_extents);
    }

    #[test]
    fn zero_extent_commit_rejects_final_limit_excess() {
        let mut block = Superblock::fresh();
        block.db_size = (MAX_ZERO_EXTENTS as u64) * 2 * page_size();
        block.zero_extents = (0..MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: (index as u64) * 2,
                end_page: (index as u64) * 2 + 1,
            })
            .collect();

        let mut overlay = Overlay::new(block.db_size);
        overlay.truncate(block.db_size + page_size()).unwrap();
        let final_page_count = page_count_for_size(overlay.size()).unwrap();

        assert!(matches!(
            zero_extents_after_commit(&block, &overlay, final_page_count),
            Err(StableMemoryError::ZeroExtentLimitExceeded {
                limit: MAX_ZERO_EXTENTS
            })
        ));
    }

    #[test]
    #[serial_test::serial]
    fn zero_extent_limit_error_keeps_active_image_and_metadata() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let mut block = Superblock::fresh();
        let max_zero_extents = u64::try_from(MAX_ZERO_EXTENTS).unwrap();
        block.db_size = max_zero_extents * 2 * page_size();
        block.page_count = page_count_for_size(block.db_size).unwrap();
        block.last_tx_id = 41;
        block.flags = FLAG_CHECKSUM_STALE | FLAG_CHECKSUM_REFRESHING;
        block.checksum = 0xA5A5;
        block.checksum_refresh_offset = 17;
        block.checksum_refresh_hash = 0x5A5A;
        block.checksum_refresh_tx_id = 40;
        block.zero_extents = (0..MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: u64::try_from(index).unwrap() * 2,
                end_page: u64::try_from(index).unwrap() * 2 + 1,
            })
            .collect();

        let sample_page_no = 1_u64;
        let sample_offset = page_physical_offset(&block, sample_page_no).unwrap();
        let sample = b"active-before";
        let sample_len = u64::try_from(sample.len()).unwrap();
        crate::stable::memory::ensure_capacity(checked_add(sample_offset, sample_len).unwrap())
            .unwrap();
        crate::stable::memory::write(sample_offset, sample).unwrap();
        block.store().unwrap();

        let before_block = Superblock::load().unwrap();
        let mut before_sample = vec![0_u8; sample.len()];
        read_base_at(sample_page_no * page_size(), &mut before_sample).unwrap();
        assert_eq!(before_sample, sample);

        begin_update().unwrap();
        truncate(block.db_size + page_size()).unwrap();
        let result = commit_update();

        assert!(matches!(
            result,
            Err(StableMemoryError::ZeroExtentLimitExceeded {
                limit: MAX_ZERO_EXTENTS
            })
        ));
        assert_eq!(Superblock::load().unwrap(), before_block);

        let mut after_sample = vec![0_u8; sample.len()];
        read_base_at(sample_page_no * page_size(), &mut after_sample).unwrap();
        assert_eq!(after_sample, before_sample);
    }

    #[test]
    fn zero_extent_temporary_limit_allows_only_dirty_page_slack() {
        let extents = (0..=MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: index as u64,
                end_page: index as u64 + 1,
            })
            .collect::<Vec<_>>();

        enforce_temporary_zero_extent_limit(&extents, MAX_ZERO_EXTENTS + 1).unwrap();
        assert!(matches!(
            enforce_temporary_zero_extent_limit(&extents, MAX_ZERO_EXTENTS),
            Err(StableMemoryError::ZeroExtentLimitExceeded {
                limit: MAX_ZERO_EXTENTS
            })
        ));
    }

    #[test]
    fn zero_extent_limit_counts_normalized_ranges() {
        let mut extents = (0..MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: (index as u64) * 2,
                end_page: (index as u64) * 2 + 1,
            })
            .collect::<Vec<_>>();

        add_zero_extent(
            &mut extents,
            (MAX_ZERO_EXTENTS as u64) * 2 - 2,
            (MAX_ZERO_EXTENTS as u64) * 2,
        )
        .unwrap();

        assert_eq!(extents.len(), MAX_ZERO_EXTENTS);
        assert_eq!(
            extents[MAX_ZERO_EXTENTS - 1],
            ZeroExtent {
                start_page: (MAX_ZERO_EXTENTS as u64) * 2 - 2,
                end_page: (MAX_ZERO_EXTENTS as u64) * 2
            }
        );
    }

    #[test]
    fn zero_extent_limit_is_rejected_without_fallback() {
        let mut extents = (0..MAX_ZERO_EXTENTS)
            .map(|index| ZeroExtent {
                start_page: (index as u64) * 2,
                end_page: (index as u64) * 2 + 1,
            })
            .collect::<Vec<_>>();
        let result = add_zero_extent(
            &mut extents,
            (MAX_ZERO_EXTENTS as u64) * 2,
            (MAX_ZERO_EXTENTS as u64) * 2 + 1,
        );
        assert!(matches!(
            result,
            Err(StableMemoryError::ZeroExtentLimitExceeded {
                limit: MAX_ZERO_EXTENTS
            })
        ));
    }

    #[test]
    fn pbt_zero_extent_normalizer_matches_independent_model() {
        let mut runner = TestRunner::new(Config {
            cases: 512,
            ..Config::default()
        });

        runner
            .run(&zero_extent_vec_strategy(), |mut extents| {
                let expected = model_normalize_zero_extents(&extents);
                let result = normalize_zero_extents(&mut extents);

                if expected.len() > MAX_ZERO_EXTENTS {
                    let rejected = matches!(
                        result,
                        Err(StableMemoryError::ZeroExtentLimitExceeded {
                            limit: MAX_ZERO_EXTENTS
                        })
                    );
                    prop_assert!(rejected);
                } else {
                    result.map_err(|error| TestCaseError::fail(error.to_string()))?;
                    prop_assert_eq!(extents, expected);
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn pbt_zero_extent_add_subtract_matches_independent_model() {
        let mut runner = TestRunner::new(Config {
            cases: 256,
            ..Config::default()
        });

        runner
            .run(&zero_extent_operation_sequence(), |operations| {
                let mut production = Vec::new();
                let mut model = Vec::new();

                for operation in operations {
                    let result = match operation {
                        ZeroExtentOp::Add {
                            start_page,
                            end_page,
                        } => {
                            model_add_zero_extent(&mut model, start_page, end_page);
                            add_zero_extent(&mut production, start_page, end_page)
                        }
                        ZeroExtentOp::Subtract {
                            start_page,
                            end_page,
                        } => {
                            model_subtract_zero_extent(&mut model, start_page, end_page);
                            subtract_zero_extent(&mut production, start_page, end_page)
                        }
                    };

                    let expected = model_normalize_zero_extents(&model);
                    if expected.len() > MAX_ZERO_EXTENTS {
                        let rejected = matches!(
                            result,
                            Err(StableMemoryError::ZeroExtentLimitExceeded {
                                limit: MAX_ZERO_EXTENTS
                            })
                        );
                        prop_assert!(rejected);
                        return Ok(());
                    }

                    result.map_err(|error| TestCaseError::fail(error.to_string()))?;
                    prop_assert_eq!(&production, &expected);
                    model = expected;
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn pbt_noop_compact_preserves_sparse_page_model() {
        let mut runner = TestRunner::new(Config {
            cases: 32,
            ..Config::default()
        });

        runner
            .run(
                &proptest::collection::vec(prop::option::of(any::<u8>()), 0..=300),
                |pages| {
                    crate::stable::memory::reset_for_tests();
                    crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

                    let active_len = pages
                        .iter()
                        .rposition(Option::is_some)
                        .map(|index| index + 1)
                        .unwrap_or(0);
                    for (page_no, byte) in pages.iter().take(active_len).enumerate() {
                        if let Some(byte) = byte {
                            write_at(
                                u64::try_from(page_no).unwrap() * page_size(),
                                &vec![*byte; page_len()],
                            )
                            .unwrap();
                        }
                    }

                    compact().unwrap();
                    let block = Superblock::load().unwrap();
                    prop_assert_eq!(
                        block.db_size,
                        u64::try_from(active_len).unwrap() * page_size()
                    );
                    prop_assert_eq!(block.page_count, active_page_count(&block).unwrap());

                    for (page_no, byte) in pages.iter().take(active_len).enumerate() {
                        let mut page = vec![0_u8; page_len()];
                        read_base_at(u64::try_from(page_no).unwrap() * page_size(), &mut page)
                            .unwrap();

                        if let Some(byte) = byte {
                            prop_assert_eq!(page, vec![*byte; page_len()]);
                        } else {
                            prop_assert_eq!(page, vec![0_u8; page_len()]);
                        }
                    }
                    Ok(())
                },
            )
            .unwrap();
    }

    #[derive(Clone, Debug)]
    enum BlobOp {
        Write { offset: u64, len: usize, byte: u8 },
        Truncate { size: u64 },
        Compact,
    }

    #[derive(Clone, Debug)]
    enum ZeroExtentOp {
        Add { start_page: u64, end_page: u64 },
        Subtract { start_page: u64, end_page: u64 },
    }

    fn zero_extent_vec_strategy() -> impl Strategy<Value = Vec<ZeroExtent>> {
        let page_limit = zero_extent_model_page_limit();
        proptest::collection::vec(
            (0_u64..=page_limit, 0_u64..=page_limit).prop_map(|(left, right)| ZeroExtent {
                start_page: left.min(right),
                end_page: left.max(right),
            }),
            0..=MAX_ZERO_EXTENTS + 8,
        )
    }

    fn zero_extent_operation_sequence() -> impl Strategy<Value = Vec<ZeroExtentOp>> {
        let page_limit = zero_extent_model_page_limit();
        let range = (0_u64..=page_limit, 0_u64..=page_limit)
            .prop_map(|(left, right)| (left.min(right), left.max(right)));
        proptest::collection::vec(
            prop_oneof![
                range
                    .clone()
                    .prop_map(|(start_page, end_page)| ZeroExtentOp::Add {
                        start_page,
                        end_page,
                    }),
                range.prop_map(|(start_page, end_page)| ZeroExtentOp::Subtract {
                    start_page,
                    end_page,
                }),
            ],
            0..=96,
        )
    }

    fn zero_extent_model_page_limit() -> u64 {
        (u64::try_from(MAX_ZERO_EXTENTS).unwrap() + 8) * 2
    }

    fn model_add_zero_extent(extents: &mut Vec<ZeroExtent>, start_page: u64, end_page: u64) {
        if start_page < end_page {
            extents.push(ZeroExtent {
                start_page,
                end_page,
            });
        }
    }

    fn model_subtract_zero_extent(extents: &mut Vec<ZeroExtent>, start_page: u64, end_page: u64) {
        if start_page >= end_page {
            return;
        }

        let mut next = Vec::new();
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
    }

    fn model_normalize_zero_extents(extents: &[ZeroExtent]) -> Vec<ZeroExtent> {
        let page_limit = usize::try_from(zero_extent_model_page_limit()).unwrap();
        let mut mask = vec![false; page_limit + 1];
        for extent in extents {
            let start = usize::try_from(extent.start_page).unwrap().min(mask.len());
            let end = usize::try_from(extent.end_page).unwrap().min(mask.len());
            for page in &mut mask[start..end] {
                *page = true;
            }
        }

        let mut normalized = Vec::new();
        let mut page = 0_usize;
        while page < mask.len() {
            if !mask[page] {
                page += 1;
                continue;
            }
            let start = page;
            while page < mask.len() && mask[page] {
                page += 1;
            }
            normalized.push(ZeroExtent {
                start_page: u64::try_from(start).unwrap(),
                end_page: u64::try_from(page).unwrap(),
            });
        }
        normalized
    }

    #[test]
    #[serial_test::serial]
    fn pbt_blob_operations_match_logical_model_across_noop_compact() {
        let mut runner = TestRunner::new(Config {
            cases: 48,
            ..Config::default()
        });

        runner
            .run(&blob_operation_sequence(), |operations| {
                crate::stable::memory::reset_for_tests();
                crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

                let mut model = Vec::new();
                let mut materialized = BTreeSet::new();
                assert_blob_model(&model, &materialized, false)?;

                for operation in operations {
                    let compacted = apply_blob_op(operation, &mut model, &mut materialized)?;
                    assert_blob_model(&model, &materialized, compacted)?;
                }
                Ok(())
            })
            .unwrap();
    }

    fn blob_operation_sequence() -> impl Strategy<Value = Vec<BlobOp>> {
        let write = (blob_offset_strategy(), blob_len_strategy(), any::<u8>())
            .prop_map(|(offset, len, byte)| BlobOp::Write { offset, len, byte });
        let truncate = blob_offset_strategy().prop_map(|size| BlobOp::Truncate { size });
        proptest::collection::vec(prop_oneof![write, truncate, Just(BlobOp::Compact)], 0..=48)
    }

    fn blob_offset_strategy() -> impl Strategy<Value = u64> {
        let limit = blob_model_limit();
        let page = page_size();
        let far_page = FAR_PAGE_NO * page;
        prop_oneof![
            0_u64..=limit,
            prop::sample::select(boundary_values(&[
                0,
                1,
                page - 1,
                page,
                page + 1,
                far_page - 1,
                far_page,
                far_page + 1,
                limit - 1,
                limit,
            ]))
            .prop_map(move |value| value.min(limit)),
        ]
    }

    fn blob_len_strategy() -> impl Strategy<Value = usize> {
        prop_oneof![
            0_usize..=(page_len() * 2 + 17),
            prop::sample::select(vec![
                0,
                1,
                page_len() - 1,
                page_len(),
                page_len() + 1,
                page_len() * 2 + 1,
            ]),
        ]
    }

    fn blob_model_limit() -> u64 {
        (FAR_PAGE_NO + 3) * page_size()
    }

    fn apply_blob_op(
        operation: BlobOp,
        model: &mut Vec<u8>,
        materialized: &mut BTreeSet<u64>,
    ) -> Result<bool, TestCaseError> {
        match operation {
            BlobOp::Write { offset, len, byte } => {
                let len = len.min(usize::try_from(blob_model_limit() - offset).unwrap());
                let bytes = vec![byte; len];
                write_at(offset, &bytes).map_err(|error| TestCaseError::fail(error.to_string()))?;
                if len == 0 {
                    return Ok(false);
                }

                let start = usize::try_from(offset).unwrap();
                let end = start + len;
                if model.len() < start {
                    model.resize(start, 0);
                }
                if model.len() < end {
                    model.resize(end, 0);
                }
                model[start..end].copy_from_slice(&bytes);
                mark_materialized_range(offset, len, materialized);
                Ok(false)
            }
            BlobOp::Truncate { size } => {
                truncate(size).map_err(|error| TestCaseError::fail(error.to_string()))?;
                let new_len = usize::try_from(size).unwrap();
                model.resize(new_len, 0);
                let active_pages = page_count_for_size(size)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                materialized.retain(|page_no| *page_no < active_pages);
                if size > 0 && !size.is_multiple_of(page_size()) {
                    materialized.insert(size / page_size());
                }
                Ok(false)
            }
            BlobOp::Compact => {
                compact().map_err(|error| TestCaseError::fail(error.to_string()))?;
                Ok(true)
            }
        }
    }

    fn mark_materialized_range(offset: u64, len: usize, materialized: &mut BTreeSet<u64>) {
        let end = offset + u64::try_from(len).unwrap();
        let first_page = offset / page_size();
        let last_page = (end - 1) / page_size();
        for page_no in first_page..=last_page {
            materialized.insert(page_no);
        }
    }

    fn assert_blob_model(
        model: &[u8],
        _materialized: &BTreeSet<u64>,
        _expect_compacted: bool,
    ) -> Result<(), TestCaseError> {
        let block = Superblock::load().map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(block.db_size, u64::try_from(model.len()).unwrap());

        if !model.is_empty() {
            let mut out = vec![0_u8; model.len()];
            read_base_at(0, &mut out).map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(out, model);
        }

        let mut tail = vec![1_u8; 32];
        read_base_at(u64::try_from(model.len()).unwrap(), &mut tail)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(tail, vec![0_u8; 32]);

        let active_pages = page_count_for_size(u64::try_from(model.len()).unwrap())
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(block.page_count, active_pages);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn read_metrics_track_in_place_data_reads() {
        crate::stable::memory::reset_for_tests();
        crate::stable::memory::init(crate::stable::memory::memory_for_tests()).unwrap();

        let page = vec![7_u8; page_len()];
        write_at(0, &page).unwrap();
        crate::read_metrics::reset_read_metrics();

        let first = read_base_page(0).unwrap();
        let second = read_base_page(0).unwrap();
        let metrics = crate::read_metrics::read_metrics_snapshot();

        assert_eq!(first, page);
        assert_eq!(second, page);
        assert!(metrics.stable_data_read_calls >= 2);
        assert!(metrics.stable_data_read_bytes >= page_size() * 2);
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
