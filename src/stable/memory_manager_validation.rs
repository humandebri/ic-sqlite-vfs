//! `MemoryManager` load-time consistency checks.
//!
//! The stable layout stays byte-compatible with `ic-stable-structures`, but
//! corrupt images are rejected before they can become in-memory state.

use crate::config::STABLE_PAGE_SIZE;
use crate::stable::memory_layout::{
    bucket_allocations_address, read_u64, BucketId, HEADER_RESERVED_BYTES, MAX_NUM_BUCKETS,
    MAX_NUM_MEMORIES, UNALLOCATED_BUCKET_MARKER,
};
use crate::stable::raw_memory::Memory;

pub(super) struct LoadedMemoryManager {
    pub(super) allocated_buckets: u16,
    pub(super) bucket_size_in_pages: u16,
    pub(super) memory_sizes_in_pages: [u64; MAX_NUM_MEMORIES as usize],
    pub(super) memory_buckets: Vec<Vec<BucketId>>,
}

pub(super) fn load_validated_layout<M: Memory>(memory: &M, header: &[u8]) -> LoadedMemoryManager {
    let allocated_buckets = u16::from_le_bytes([header[4], header[5]]);
    let bucket_size_in_pages = u16::from_le_bytes([header[6], header[7]]);
    assert!(
        u64::from(allocated_buckets) <= MAX_NUM_BUCKETS,
        "invalid memory manager header: allocated buckets exceeds maximum"
    );
    assert!(
        bucket_size_in_pages != 0,
        "invalid memory manager header: bucket size is zero"
    );

    let memory_sizes_in_pages = read_memory_sizes(header);
    validate_memory_sizes_fit_bytes(&memory_sizes_in_pages);
    let memory_buckets = read_validated_buckets(memory, allocated_buckets);
    validate_bucket_counts(
        bucket_size_in_pages,
        &memory_sizes_in_pages,
        &memory_buckets,
    );

    LoadedMemoryManager {
        allocated_buckets,
        bucket_size_in_pages,
        memory_sizes_in_pages,
        memory_buckets,
    }
}

fn read_memory_sizes(header: &[u8]) -> [u64; MAX_NUM_MEMORIES as usize] {
    let mut sizes = [0_u64; MAX_NUM_MEMORIES as usize];
    let mut offset = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES;
    for size in &mut sizes {
        *size = read_u64(&header[offset..offset + 8]);
        offset += 8;
    }
    sizes
}

fn validate_memory_sizes_fit_bytes(sizes: &[u64; MAX_NUM_MEMORIES as usize]) {
    for (id, pages) in sizes.iter().enumerate() {
        assert!(
            pages.checked_mul(STABLE_PAGE_SIZE).is_some(),
            "invalid memory manager header: memory {id} size overflows bytes"
        );
    }
}

fn read_validated_buckets<M: Memory>(memory: &M, allocated_buckets: u16) -> Vec<Vec<BucketId>> {
    let mut buckets = vec![0_u8; MAX_NUM_BUCKETS as usize];
    memory.read(bucket_allocations_address(BucketId(0)), &mut buckets);

    let allocated = usize::from(allocated_buckets);
    let mut memory_buckets = vec![Vec::new(); MAX_NUM_MEMORIES as usize];
    for (bucket, owner) in buckets[..allocated].iter().copied().enumerate() {
        assert!(
            owner < MAX_NUM_MEMORIES,
            "invalid memory manager allocation table: allocated bucket has no owner"
        );
        memory_buckets[owner as usize].push(BucketId(bucket as u16));
    }
    for owner in &buckets[allocated..] {
        assert!(
            *owner == UNALLOCATED_BUCKET_MARKER,
            "invalid memory manager allocation table: unallocated bucket has owner"
        );
    }
    memory_buckets
}

fn validate_bucket_counts(
    bucket_size_in_pages: u16,
    memory_sizes_in_pages: &[u64; MAX_NUM_MEMORIES as usize],
    memory_buckets: &[Vec<BucketId>],
) {
    let bucket_size = u64::from(bucket_size_in_pages);
    for (id, size) in memory_sizes_in_pages.iter().enumerate() {
        let expected = size.div_ceil(bucket_size);
        assert_eq!(
            expected,
            memory_buckets[id].len() as u64,
            "invalid memory manager layout: memory {id} size and buckets mismatch"
        );
    }
}
