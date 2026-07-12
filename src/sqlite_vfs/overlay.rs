//! Per-message page overlay for the main SQLite database image.
//!
//! SQLite may write dirty pages before it knows whether a transaction will
//! commit. The overlay keeps full logical pages in heap memory so failed
//! transactions can discard them without publishing a new stable image.

use crate::config::SQLITE_PAGE_SIZE;
use crate::sqlite_vfs::stable_blob;
use crate::stable::memory::{self, ContextId, StableMemoryError};
use crate::stable::meta::ZeroExtent;
use std::cell::RefCell;

const CLEAN_PAGE_CACHE_CAPACITY: usize = 8;

pub(crate) trait BasePageSource {
    fn read_base_page(&mut self, page_no: u64) -> Result<Vec<u8>, StableMemoryError>;
}

struct StableBlobBasePageSource;

impl BasePageSource for StableBlobBasePageSource {
    fn read_base_page(&mut self, page_no: u64) -> Result<Vec<u8>, StableMemoryError> {
        stable_blob::read_base_page(page_no)
    }
}

#[derive(Debug)]
pub struct Overlay {
    base_size: u64,
    size: u64,
    pages: Vec<(u64, Vec<u8>)>,
    clean_pages: Vec<(u64, Vec<u8>)>,
    zero_extents: Vec<ZeroExtent>,
}

thread_local! {
    static OVERLAY: RefCell<Vec<(ContextId, Overlay)>> = const { RefCell::new(Vec::new()) };
}

impl Overlay {
    pub fn new(base_size: u64) -> Self {
        Self {
            base_size,
            size: base_size,
            pages: Vec::new(),
            clean_pages: Vec::new(),
            zero_extents: Vec::new(),
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn dirty_pages(&self) -> &[(u64, Vec<u8>)] {
        &self.pages
    }

    pub fn zero_extents(&self) -> &[ZeroExtent] {
        &self.zero_extents
    }

    pub fn is_empty(&self) -> bool {
        self.size == self.base_size && self.pages.is_empty() && self.zero_extents.is_empty()
    }

    pub fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
        let mut source = StableBlobBasePageSource;
        self.read_at_from(&mut source, offset, dst)
    }

    fn read_at_from<S: BasePageSource>(
        &mut self,
        source: &mut S,
        offset: u64,
        dst: &mut [u8],
    ) -> Result<bool, StableMemoryError> {
        if dst.is_empty() {
            return Ok(true);
        }
        if offset >= self.size {
            dst.fill(0);
            return Ok(false);
        }

        let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let copied = requested.min(self.size - offset);
        let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
        self.read_range(source, offset, &mut dst[..copied_len])?;
        if copied_len < dst.len() {
            dst[copied_len..].fill(0);
        }
        Ok(copied == requested)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
        let mut source = StableBlobBasePageSource;
        self.write_at_from(&mut source, offset, bytes)
    }

    fn write_at_from<S: BasePageSource>(
        &mut self,
        source: &mut S,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), StableMemoryError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let len = u64::try_from(bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let end = checked_add(offset, len)?;
        let old_size = self.size;
        if end > old_size {
            self.record_newly_active_zero_pages(old_size, end)?;
            self.zero_newly_active_partial_pages(source, old_size, end)?;
        }
        let full_page_no = page_no(offset);
        if bytes.len() == page_len() && offset.is_multiple_of(page_size()) {
            self.write_full_page(full_page_no, bytes)?;
            self.size = self.size.max(end);
            return Ok(());
        }
        let mut written = 0_usize;

        while written < bytes.len() {
            let absolute = checked_add(
                offset,
                u64::try_from(written).map_err(|_| StableMemoryError::OffsetOverflow)?,
            )?;
            let page_no = page_no(absolute);
            let page_offset = page_offset(absolute)?;
            let available = page_len() - page_offset;
            let remaining = bytes.len() - written;
            let copied = available.min(remaining);
            let page = self.load_dirty_page(source, page_no)?;
            page[page_offset..page_offset + copied]
                .copy_from_slice(&bytes[written..written + copied]);
            written += copied;
        }

        self.size = self.size.max(end);
        Ok(())
    }

    pub fn truncate(&mut self, size: u64) -> Result<(), StableMemoryError> {
        let mut source = StableBlobBasePageSource;
        self.truncate_with_source(&mut source, size)
    }

    fn truncate_with_source<S: BasePageSource>(
        &mut self,
        source: &mut S,
        size: u64,
    ) -> Result<(), StableMemoryError> {
        let old_size = self.size;
        if size > old_size {
            self.record_newly_active_zero_pages(old_size, size)?;
            self.zero_newly_active_partial_pages(source, old_size, size)?;
        } else if size < old_size {
            self.record_truncated_zero_pages(size, old_size)?;
        }
        self.size = size;
        let keep_pages = stable_blob::page_count_for_size(size)?;
        self.pages.retain(|(page_no, _)| *page_no < keep_pages);
        self.clean_pages
            .retain(|(page_no, _)| *page_no < keep_pages);

        if size == 0 || size.is_multiple_of(page_size()) {
            return Ok(());
        }

        let last_page = page_no(size);
        let tail = page_offset(size)?;
        let page = self.load_dirty_page(source, last_page)?;
        page[tail..].fill(0);
        Ok(())
    }

    fn record_newly_active_zero_pages(
        &mut self,
        old_size: u64,
        new_size: u64,
    ) -> Result<(), StableMemoryError> {
        let first_new_page = stable_blob::page_count_for_size(old_size)?;
        let final_page_count = stable_blob::page_count_for_size(new_size)?;
        self.record_zero_extent(first_new_page, final_page_count);
        Ok(())
    }

    fn record_truncated_zero_pages(
        &mut self,
        new_size: u64,
        old_size: u64,
    ) -> Result<(), StableMemoryError> {
        let first_zero_page = stable_blob::page_count_for_size(new_size)?;
        let old_page_count = stable_blob::page_count_for_size(old_size)?;
        self.record_zero_extent(first_zero_page, old_page_count);
        Ok(())
    }

    fn record_zero_extent(&mut self, start_page: u64, end_page: u64) {
        if start_page < end_page {
            self.zero_extents.push(ZeroExtent {
                start_page,
                end_page,
            });
        }
    }

    fn zero_newly_active_partial_pages<S: BasePageSource>(
        &mut self,
        source: &mut S,
        old_size: u64,
        new_size: u64,
    ) -> Result<(), StableMemoryError> {
        if old_size >= new_size {
            return Ok(());
        }

        let old_page_no = page_no(old_size);
        if !old_size.is_multiple_of(page_size()) {
            let old_page_start = old_page_no
                .checked_mul(page_size())
                .ok_or(StableMemoryError::OffsetOverflow)?;
            let old_page_end = checked_add(old_page_start, page_size())?;
            let start = page_offset(old_size)?;
            let end_absolute = new_size.min(old_page_end);
            let end = if end_absolute == old_page_end {
                page_len()
            } else {
                page_offset(end_absolute)?
            };
            let page = self.load_dirty_page(source, old_page_no)?;
            page[start..end].fill(0);
        }

        if !new_size.is_multiple_of(page_size()) {
            let new_page_no = page_no(new_size);
            if new_page_no != old_page_no {
                let end = page_offset(new_size)?;
                let page = self.load_dirty_page(source, new_page_no)?;
                page[..end].fill(0);
            }
        }

        Ok(())
    }

    fn load_dirty_page<S: BasePageSource>(
        &mut self,
        source: &mut S,
        page_no: u64,
    ) -> Result<&mut Vec<u8>, StableMemoryError> {
        if let Some(index) = self.dirty_page_index(page_no) {
            return Ok(&mut self.pages[index].1);
        }
        let page = if self.page_is_zero_masked(page_no) {
            vec![0_u8; page_len()]
        } else {
            match self.take_clean_page(page_no) {
                Some(page) => page,
                None => source.read_base_page(page_no)?,
            }
        };
        self.pages.push((page_no, page));
        self.pages
            .last_mut()
            .map(|(_, page)| page)
            .ok_or(StableMemoryError::OffsetOverflow)
    }

    fn write_full_page(&mut self, page_no: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
        if let Some(index) = self.dirty_page_index(page_no) {
            self.pages[index].1.copy_from_slice(bytes);
            return Ok(());
        }
        self.pages.push((page_no, bytes.to_vec()));
        Ok(())
    }

    fn dirty_page_index(&self, page_no: u64) -> Option<usize> {
        self.pages
            .iter()
            .position(|(cached_page, _)| *cached_page == page_no)
    }

    fn dirty_page(&self, page_no: u64) -> Option<&[u8]> {
        for (cached_page, page) in &self.pages {
            if *cached_page == page_no {
                return Some(page);
            }
        }
        None
    }

    fn clean_page(&self, page_no: u64) -> Option<&[u8]> {
        for (cached_page, page) in &self.clean_pages {
            if *cached_page == page_no {
                return Some(page);
            }
        }
        None
    }

    fn page_is_zero_masked(&self, page_no: u64) -> bool {
        self.zero_extents
            .iter()
            .any(|extent| page_no >= extent.start_page && page_no < extent.end_page)
    }

    fn take_clean_page(&mut self, page_no: u64) -> Option<Vec<u8>> {
        let index = self
            .clean_pages
            .iter()
            .position(|(cached_page, _)| *cached_page == page_no)?;
        Some(self.clean_pages.remove(index).1)
    }

    fn insert_clean_page(&mut self, page_no: u64, page: Vec<u8>) {
        if self.dirty_page_index(page_no).is_some()
            || self
                .clean_pages
                .iter()
                .any(|(cached_page, _)| *cached_page == page_no)
        {
            return;
        }
        if self.clean_pages.len() == CLEAN_PAGE_CACHE_CAPACITY {
            self.clean_pages.remove(0);
        }
        self.clean_pages.push((page_no, page));
    }

    fn read_range<S: BasePageSource>(
        &mut self,
        source: &mut S,
        offset: u64,
        dst: &mut [u8],
    ) -> Result<(), StableMemoryError> {
        let mut copied_total = 0_usize;
        while copied_total < dst.len() {
            let absolute = checked_add(
                offset,
                u64::try_from(copied_total).map_err(|_| StableMemoryError::OffsetOverflow)?,
            )?;
            let page_no = page_no(absolute);
            let page_offset = page_offset(absolute)?;
            let available = page_len() - page_offset;
            let remaining = dst.len() - copied_total;
            let copied = available.min(remaining);
            if let Some(page) = self.dirty_page(page_no) {
                dst[copied_total..copied_total + copied]
                    .copy_from_slice(&page[page_offset..page_offset + copied]);
            } else if self.page_is_zero_masked(page_no) {
                dst[copied_total..copied_total + copied].fill(0);
            } else if let Some(page) = self.clean_page(page_no) {
                dst[copied_total..copied_total + copied]
                    .copy_from_slice(&page[page_offset..page_offset + copied]);
            } else {
                let page = source.read_base_page(page_no)?;
                dst[copied_total..copied_total + copied]
                    .copy_from_slice(&page[page_offset..page_offset + copied]);
                self.insert_clean_page(page_no, page);
            }
            copied_total += copied;
        }
        Ok(())
    }
}

pub fn begin(base_size: u64) -> Result<(), StableMemoryError> {
    let context = memory::active_context_id()?;
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if overlay_index(&slot, context).is_some() {
            return Err(StableMemoryError::Failpoint("overlay already active"));
        }
        slot.push((context, Overlay::new(base_size)));
        Ok(())
    })
}

pub fn rollback() {
    let Ok(context) = memory::active_context_id() else {
        return;
    };
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(index) = overlay_index(&slot, context) {
            slot.swap_remove(index);
        }
    });
}

pub fn is_active() -> bool {
    let Ok(context) = memory::active_context_id() else {
        return false;
    };
    OVERLAY.with(|slot| overlay_index(&slot.borrow(), context).is_some())
}

pub fn take() -> Option<Overlay> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let index = overlay_index(&slot, context)?;
        Some(slot.swap_remove(index).1)
    })
}

pub fn read_at(offset: u64, dst: &mut [u8]) -> Option<Result<bool, StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let index = overlay_index(&slot, context)?;
        Some(slot[index].1.read_at(offset, dst))
    })
}

pub fn write_at(offset: u64, bytes: &[u8]) -> Option<Result<(), StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let index = overlay_index(&slot, context)?;
        Some(slot[index].1.write_at(offset, bytes))
    })
}

pub fn truncate(size: u64) -> Option<Result<(), StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let index = overlay_index(&slot, context)?;
        Some(slot[index].1.truncate(size))
    })
}

pub fn file_size() -> Option<u64> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    OVERLAY.with(|slot| {
        let slot = slot.borrow();
        let index = overlay_index(&slot, context)?;
        Some(slot[index].1.size())
    })
}

fn overlay_index(overlays: &[(ContextId, Overlay)], context: ContextId) -> Option<usize> {
    overlays
        .iter()
        .position(|(stored_context, _)| *stored_context == context)
}

fn page_size() -> u64 {
    u64::from(SQLITE_PAGE_SIZE)
}

fn page_len() -> usize {
    SQLITE_PAGE_SIZE as usize
}

fn page_no(offset: u64) -> u64 {
    offset / page_size()
}

fn page_offset(offset: u64) -> Result<usize, StableMemoryError> {
    usize::try_from(offset % page_size()).map_err(|_| StableMemoryError::OffsetOverflow)
}

fn checked_add(left: u64, right: u64) -> Result<u64, StableMemoryError> {
    left.checked_add(right)
        .ok_or(StableMemoryError::OffsetOverflow)
}

#[cfg(test)]
mod tests {
    use super::{page_len, page_size, BasePageSource, Overlay};
    use crate::sqlite_vfs::stable_blob;
    use crate::stable::memory::{self, StableMemoryError};

    struct TestBasePageSource {
        page: Vec<u8>,
        reads: Vec<u64>,
    }

    impl BasePageSource for TestBasePageSource {
        fn read_base_page(&mut self, page_no: u64) -> Result<Vec<u8>, StableMemoryError> {
            self.reads.push(page_no);
            Ok(self.page.clone())
        }
    }

    #[test]
    fn later_overlapping_write_wins_regardless_of_offset_order() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        let mut overlay = Overlay::new(0);

        overlay.write_at(4, b"AAAA").unwrap();
        overlay.write_at(2, b"bbbbbb").unwrap();

        let mut out = vec![0; 8];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out.as_slice(), b"\0\0bbbbbb");
    }

    #[test]
    fn truncate_then_sparse_extend_zero_fills_gap() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        let mut overlay = Overlay::new(0);

        overlay.write_at(0, b"abcd").unwrap();
        overlay.truncate(1).unwrap();
        overlay.write_at(3, b"z").unwrap();

        let mut out = vec![0; 4];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out.as_slice(), b"a\0\0z");
    }

    #[test]
    fn clean_base_reads_use_injected_source_and_cache() {
        let mut overlay = Overlay::new(page_size());
        let mut source = TestBasePageSource {
            page: vec![11_u8; page_len()],
            reads: Vec::new(),
        };
        let mut first = [0_u8; 16];
        let mut second = [0_u8; 16];

        assert!(overlay.read_at_from(&mut source, 0, &mut first).unwrap());
        assert!(overlay.read_at_from(&mut source, 8, &mut second).unwrap());

        assert_eq!(first, [11_u8; 16]);
        assert_eq!(second, [11_u8; 16]);
        assert_eq!(source.reads, vec![0]);
    }

    #[test]
    #[serial_test::serial]
    fn full_page_append_does_not_read_base_page() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        let mut overlay = Overlay::new(0);
        let page = vec![5_u8; page_len()];

        crate::read_metrics::reset_read_metrics();
        overlay.write_at(0, &page).unwrap();
        let metrics = crate::read_metrics::read_metrics_snapshot();

        let mut out = vec![0_u8; page_len()];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out, page);
        assert_eq!(metrics.stable_data_read_calls, 0);
        assert_eq!(metrics.stable_data_read_bytes, 0);
    }

    #[test]
    #[serial_test::serial]
    fn full_page_overwrite_does_not_read_base_page() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        stable_blob::write_at(0, &vec![3_u8; page_len()]).unwrap();
        let mut overlay = Overlay::new(page_size());
        let page = vec![9_u8; page_len()];

        crate::read_metrics::reset_read_metrics();
        overlay.write_at(0, &page).unwrap();
        let metrics = crate::read_metrics::read_metrics_snapshot();

        let mut out = vec![0_u8; page_len()];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out, page);
        assert_eq!(metrics.stable_data_read_calls, 0);
        assert_eq!(metrics.stable_data_read_bytes, 0);
    }

    #[test]
    #[serial_test::serial]
    fn repeated_clean_base_reads_reuse_overlay_cache() {
        memory::reset_for_tests();
        memory::init(memory::memory_for_tests()).unwrap();
        let page = vec![7_u8; page_len()];
        stable_blob::write_at(0, &page).unwrap();
        let mut overlay = Overlay::new(page_size());
        let mut first = [0_u8; 16];
        let mut second = [0_u8; 16];

        crate::read_metrics::reset_read_metrics();
        assert!(overlay.read_at(0, &mut first).unwrap());
        assert!(overlay.read_at(8, &mut second).unwrap());
        let metrics = crate::read_metrics::read_metrics_snapshot();

        assert_eq!(first, [7_u8; 16]);
        assert_eq!(second, [7_u8; 16]);
        assert_eq!(metrics.stable_data_read_calls, 1);
        assert_eq!(metrics.stable_data_read_bytes, page_size());
    }
}
