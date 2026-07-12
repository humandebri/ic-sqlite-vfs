#[cfg(test)]
use crate::stable::memory;
#[cfg(test)]
use crate::stable::memory::ContextId;
use crate::stable::memory::StableMemoryError;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::BTreeMap;

pub(super) const CHECKSUM_CHUNK_LEN: u64 = 16 * 1024;
#[cfg(test)]
pub(super) const FAR_PAGE_NO: u64 = 257;
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

    pub(super) fn get(&self, page_no: u64) -> Option<u64> {
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

    pub(super) fn insert(&mut self, page_no: u64, physical: u64) {
        if self.entries.len() == FILE_PAGE_OFFSET_CACHE_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push((page_no, physical));
    }

    #[inline(always)]
    pub(super) fn copy_page_slice(&self, page_no: u64, in_page: usize, dst: &mut [u8]) -> bool {
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

    pub(super) fn insert_page(&mut self, page_no: u64, page: Vec<u8>) {
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

#[cfg(test)]
pub(super) fn hit_failpoint(failpoint: StableBlobFailpoint) -> Result<(), StableMemoryError> {
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
pub(super) fn hit_failpoint(_failpoint: StableBlobFailpoint) -> Result<(), StableMemoryError> {
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
