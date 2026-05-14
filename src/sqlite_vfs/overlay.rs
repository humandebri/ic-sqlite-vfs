//! Per-message page overlay for the main SQLite database image.
//!
//! SQLite may write dirty pages before it knows whether a transaction will
//! commit. The overlay keeps full logical pages in heap memory so failed
//! transactions can discard them without publishing a new page table.

use crate::config::SQLITE_PAGE_SIZE;
use crate::sqlite_vfs::stable_blob;
use crate::stable::memory::{self, ContextId, StableMemoryError};
use std::cell::RefCell;
use std::collections::{btree_map::Entry, BTreeMap};

#[derive(Debug)]
pub struct Overlay {
    base_size: u64,
    size: u64,
    pages: BTreeMap<u64, Vec<u8>>,
}

thread_local! {
    static OVERLAY: RefCell<BTreeMap<ContextId, Overlay>> = const { RefCell::new(BTreeMap::new()) };
}

impl Overlay {
    pub fn new(base_size: u64) -> Self {
        Self {
            base_size,
            size: base_size,
            pages: BTreeMap::new(),
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn dirty_pages(&self) -> &BTreeMap<u64, Vec<u8>> {
        &self.pages
    }

    pub fn is_empty(&self) -> bool {
        self.size == self.base_size && self.pages.is_empty()
    }

    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
        dst.fill(0);
        if dst.is_empty() {
            return Ok(true);
        }
        if offset >= self.size {
            return Ok(false);
        }

        let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let copied = requested.min(self.size - offset);
        let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
        self.read_range(offset, &mut dst[..copied_len])?;
        Ok(copied == requested)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let len = u64::try_from(bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let end = checked_add(offset, len)?;
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
            let page = self.load_dirty_page(page_no)?;
            page[page_offset..page_offset + copied]
                .copy_from_slice(&bytes[written..written + copied]);
            written += copied;
        }

        self.size = self.size.max(end);
        Ok(())
    }

    pub fn truncate(&mut self, size: u64) -> Result<(), StableMemoryError> {
        self.size = size;
        let keep_pages = stable_blob::page_count_for_size(size)?;
        self.pages.retain(|page_no, _| *page_no < keep_pages);

        if size == 0 || size.is_multiple_of(page_size()) {
            return Ok(());
        }

        let last_page = page_no(size);
        let tail = page_offset(size)?;
        let page = self.load_dirty_page(last_page)?;
        page[tail..].fill(0);
        Ok(())
    }

    fn load_dirty_page(&mut self, page_no: u64) -> Result<&mut Vec<u8>, StableMemoryError> {
        if let Entry::Vacant(entry) = self.pages.entry(page_no) {
            let page = stable_blob::read_base_page(page_no)?;
            entry.insert(page);
        }
        self.pages
            .get_mut(&page_no)
            .ok_or(StableMemoryError::OffsetOverflow)
    }

    fn read_range(&self, offset: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
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
            if let Some(page) = self.pages.get(&page_no) {
                dst[copied_total..copied_total + copied]
                    .copy_from_slice(&page[page_offset..page_offset + copied]);
            } else {
                let page = stable_blob::read_base_page(page_no)?;
                dst[copied_total..copied_total + copied]
                    .copy_from_slice(&page[page_offset..page_offset + copied]);
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
        if slot.contains_key(&context) {
            return Err(StableMemoryError::Failpoint("overlay already active"));
        }
        slot.insert(context, Overlay::new(base_size));
        Ok(())
    })
}

pub fn rollback() {
    let Ok(context) = memory::active_context_id() else {
        return;
    };
    OVERLAY.with(|slot| {
        slot.borrow_mut().remove(&context);
    });
}

pub fn is_active() -> bool {
    let Ok(context) = memory::active_context_id() else {
        return false;
    };
    OVERLAY.with(|slot| slot.borrow().contains_key(&context))
}

pub fn take() -> Option<Overlay> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    OVERLAY.with(|slot| slot.borrow_mut().remove(&context))
}

pub fn read_at(offset: u64, dst: &mut [u8]) -> Option<Result<bool, StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        slot.borrow()
            .get(&context)
            .map(|overlay| overlay.read_at(offset, dst))
    })
}

pub fn write_at(offset: u64, bytes: &[u8]) -> Option<Result<(), StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        slot.borrow_mut()
            .get_mut(&context)
            .map(|overlay| overlay.write_at(offset, bytes))
    })
}

pub fn truncate(size: u64) -> Option<Result<(), StableMemoryError>> {
    let context = match memory::active_context_id() {
        Ok(context) => context,
        Err(error) => return Some(Err(error)),
    };
    OVERLAY.with(|slot| {
        slot.borrow_mut()
            .get_mut(&context)
            .map(|overlay| overlay.truncate(size))
    })
}

pub fn file_size() -> Option<u64> {
    let Ok(context) = memory::active_context_id() else {
        return None;
    };
    OVERLAY.with(|slot| slot.borrow().get(&context).map(Overlay::size))
}

fn page_size() -> u64 {
    u64::from(SQLITE_PAGE_SIZE)
}

fn page_len() -> usize {
    usize::try_from(SQLITE_PAGE_SIZE).expect("SQLite page size fits usize")
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
    use super::Overlay;
    use crate::stable::memory;

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
}
