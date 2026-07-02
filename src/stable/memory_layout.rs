//! Shared constants and helpers for the MemoryManager-compatible layout.
//!
//! Kept separate so the forked manager stays small and the byte format remains
//! visible in one place.

use crate::config::STABLE_PAGE_SIZE;
use crate::stable::raw_memory::Memory;
use std::cell::Cell;

pub(super) const MAGIC: &[u8; 3] = b"MGR";
pub(super) const LAYOUT_VERSION: u8 = 1;
pub(super) const MAX_NUM_MEMORIES: u8 = 255;
pub(super) const MAX_NUM_BUCKETS: u64 = 32_768;
pub(super) const BUCKET_SIZE_IN_PAGES: u64 = 128;
pub(super) const UNALLOCATED_BUCKET_MARKER: u8 = MAX_NUM_MEMORIES;
pub(super) const BUCKETS_OFFSET_IN_PAGES: u64 = 1;
pub(super) const BUCKETS_OFFSET_IN_BYTES: u64 = BUCKETS_OFFSET_IN_PAGES * STABLE_PAGE_SIZE;
pub(super) const HEADER_RESERVED_BYTES: usize = 32;
pub(super) const HEADER_SIZE: u64 = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES as u64 + 255 * 8;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MemoryId(pub(super) u8);

impl MemoryId {
    pub const fn new(id: u8) -> Self {
        // Invariant: 255 is the allocation-table marker, not an addressable memory.
        assert!(id != UNALLOCATED_BUCKET_MARKER);
        Self(id)
    }
}

#[derive(Clone, Copy)]
pub(super) struct BucketId(pub(super) u16);

#[derive(Clone, Copy)]
pub(super) struct VirtualSegment {
    address: u64,
    length: u64,
}

impl VirtualSegment {
    pub(super) fn new(address: u64, length: u64) -> Self {
        Self { address, length }
    }

    fn contains(self, other: Self) -> bool {
        virtual_segment_contains(self.address, self.length, other.address, other.length)
    }

    fn address(self) -> u64 {
        self.address
    }
}

#[derive(Clone)]
pub(super) struct BucketCache {
    bucket: Cell<VirtualSegment>,
    real_address: Cell<u64>,
}

impl BucketCache {
    pub(super) fn new() -> Self {
        Self {
            bucket: Cell::new(VirtualSegment::new(0, 0)),
            real_address: Cell::new(0),
        }
    }

    pub(super) fn get(&self, segment: VirtualSegment) -> Option<u64> {
        let bucket = self.bucket.get();
        bucket
            .contains(segment)
            .then(|| self.real_address.get() + (segment.address() - bucket.address()))
    }

    pub(super) fn store(&self, bucket: VirtualSegment, real_address: u64) {
        self.bucket.set(bucket);
        self.real_address.set(real_address);
    }
}

pub(super) fn bucket_allocations_address(id: BucketId) -> u64 {
    HEADER_SIZE + u64::from(id.0)
}

pub(super) fn virtual_segment_contains(
    address: u64,
    length: u64,
    other_address: u64,
    other_length: u64,
) -> bool {
    if length > u64::MAX - address {
        return false;
    }
    if other_length > u64::MAX - other_address {
        return false;
    }
    let end = address + length;
    let other_end = other_address + other_length;
    address <= other_address && other_end <= end
}

pub(super) fn write_growing<M: Memory>(memory: &M, offset: u64, bytes: &[u8]) {
    // Invariant: metadata writes use small fixed offsets and table sizes.
    let end = offset
        .checked_add(bytes.len() as u64)
        .expect("offset overflow");
    // Invariant: Memory::size is stable-page based and expected to fit bytes here.
    let size_bytes = memory
        .size()
        .checked_mul(STABLE_PAGE_SIZE)
        .expect("memory size overflow");
    if end > size_bytes {
        let pages = (end - size_bytes).div_ceil(STABLE_PAGE_SIZE);
        // Initialization cannot proceed if the backing memory refuses metadata growth.
        assert!(memory.grow(pages) >= 0, "stable memory grow failed");
    }
    memory.write(offset, bytes);
}

pub(super) fn read_u64(bytes: &[u8]) -> u64 {
    // Invariant: callers pass exact 8-byte header fields.
    u64::from_le_bytes(bytes.try_into().expect("u64 field has 8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::{virtual_segment_contains, BucketCache, VirtualSegment};

    #[test]
    fn virtual_segment_contains_rejects_overflowing_ranges() {
        let segment = VirtualSegment::new(100, 50);

        assert!(segment.contains(VirtualSegment::new(120, 10)));
        assert!(!segment.contains(VirtualSegment::new(u64::MAX - 4, 8)));
        assert!(
            !VirtualSegment::new(u64::MAX - 4, 8).contains(VirtualSegment::new(u64::MAX - 3, 1))
        );
        assert_eq!(
            segment.contains(VirtualSegment::new(120, 10)),
            virtual_segment_contains(100, 50, 120, 10)
        );
    }

    #[test]
    fn bucket_cache_rejects_overflowing_segment_hit() {
        let cache = BucketCache::new();

        cache.store(VirtualSegment::new(0, 128), 1_000);

        assert_eq!(cache.get(VirtualSegment::new(8, 8)), Some(1_008));
        assert_eq!(cache.get(VirtualSegment::new(u64::MAX - 4, 8)), None);
    }
}
