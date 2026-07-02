//! `MemoryManager` load-time consistency checks.
//!
//! The stable layout stays byte-compatible with `ic-stable-structures`, but
//! corrupt images are rejected before they can become in-memory state.

use crate::config::STABLE_PAGE_SIZE;
use crate::stable::memory_layout::{
    bucket_allocations_address, read_u64, BucketId, BUCKETS_OFFSET_IN_PAGES, HEADER_RESERVED_BYTES,
    MAX_NUM_BUCKETS, MAX_NUM_MEMORIES, UNALLOCATED_BUCKET_MARKER,
};
use crate::stable::raw_memory::Memory;

pub(super) struct LoadedMemoryManager {
    pub(super) allocated_buckets: u16,
    pub(super) bucket_size_in_pages: u16,
    pub(super) memory_sizes_in_pages: [u64; MAX_NUM_MEMORIES as usize],
    pub(super) memory_buckets: Vec<Vec<BucketId>>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MemoryManagerLayoutError {
    #[error("invalid memory manager header: allocated buckets exceeds maximum")]
    AllocatedBucketsExceedsMaximum,
    #[error("invalid memory manager header: bucket size is zero")]
    BucketSizeIsZero,
    #[error("invalid memory manager header: bucket range overflows")]
    BucketRangeOverflows,
    #[error("invalid memory manager layout: backing memory truncated")]
    BackingMemoryTruncated,
    #[error("invalid memory manager header: memory {0} size overflows bytes")]
    MemorySizeOverflowsBytes(usize),
    #[error("invalid memory manager allocation table: allocated bucket has no owner")]
    AllocatedBucketHasNoOwner,
    #[error("invalid memory manager allocation table: unallocated bucket has owner")]
    UnallocatedBucketHasOwner,
    #[error("invalid memory manager layout: memory {0} size and buckets mismatch")]
    MemorySizeAndBucketsMismatch(usize),
}

pub(super) fn load_validated_layout<M: Memory>(memory: &M, header: &[u8]) -> LoadedMemoryManager {
    // Legacy infallible MemoryManager::init reports corrupt committed layouts by
    // panic; MemoryManager::init_strict uses try_load_validated_layout for errors.
    try_load_validated_layout(memory, header).unwrap_or_else(|error| panic!("{error}"))
}

pub(super) fn try_load_validated_layout<M: Memory>(
    memory: &M,
    header: &[u8],
) -> Result<LoadedMemoryManager, MemoryManagerLayoutError> {
    let allocated_buckets = u16::from_le_bytes([header[4], header[5]]);
    let bucket_size_in_pages = u16::from_le_bytes([header[6], header[7]]);
    if u64::from(allocated_buckets) > MAX_NUM_BUCKETS {
        return Err(MemoryManagerLayoutError::AllocatedBucketsExceedsMaximum);
    }
    if bucket_size_in_pages == 0 {
        return Err(MemoryManagerLayoutError::BucketSizeIsZero);
    }

    let memory_sizes_in_pages = read_memory_sizes(header);
    validate_memory_sizes_fit_bytes(&memory_sizes_in_pages)?;
    let memory_buckets = read_validated_buckets(memory, allocated_buckets)?;
    validate_bucket_counts(
        bucket_size_in_pages,
        &memory_sizes_in_pages,
        &memory_buckets,
    )?;
    validate_backing_memory_size(memory, allocated_buckets, bucket_size_in_pages)?;

    Ok(LoadedMemoryManager {
        allocated_buckets,
        bucket_size_in_pages,
        memory_sizes_in_pages,
        memory_buckets,
    })
}

fn validate_backing_memory_size<M: Memory>(
    memory: &M,
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
) -> Result<(), MemoryManagerLayoutError> {
    let required_data_pages = u64::from(bucket_size_in_pages)
        .checked_mul(u64::from(allocated_buckets))
        .ok_or(MemoryManagerLayoutError::BucketRangeOverflows)?;
    let required_pages = BUCKETS_OFFSET_IN_PAGES
        .checked_add(required_data_pages)
        .ok_or(MemoryManagerLayoutError::BucketRangeOverflows)?;
    if memory.size() < required_pages {
        return Err(MemoryManagerLayoutError::BackingMemoryTruncated);
    }
    Ok(())
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

fn validate_memory_sizes_fit_bytes(
    sizes: &[u64; MAX_NUM_MEMORIES as usize],
) -> Result<(), MemoryManagerLayoutError> {
    for (id, pages) in sizes.iter().enumerate() {
        if pages.checked_mul(STABLE_PAGE_SIZE).is_none() {
            return Err(MemoryManagerLayoutError::MemorySizeOverflowsBytes(id));
        }
    }
    Ok(())
}

fn read_validated_buckets<M: Memory>(
    memory: &M,
    allocated_buckets: u16,
) -> Result<Vec<Vec<BucketId>>, MemoryManagerLayoutError> {
    let mut buckets = vec![0_u8; MAX_NUM_BUCKETS as usize];
    memory.read(bucket_allocations_address(BucketId(0)), &mut buckets);

    let allocated = usize::from(allocated_buckets);
    let mut memory_buckets = vec![Vec::new(); MAX_NUM_MEMORIES as usize];
    for (bucket, owner) in buckets[..allocated].iter().copied().enumerate() {
        if owner == MAX_NUM_MEMORIES {
            return Err(MemoryManagerLayoutError::AllocatedBucketHasNoOwner);
        }
        memory_buckets[owner as usize].push(BucketId(bucket as u16));
    }
    for owner in &buckets[allocated..] {
        if *owner != UNALLOCATED_BUCKET_MARKER {
            return Err(MemoryManagerLayoutError::UnallocatedBucketHasOwner);
        }
    }
    Ok(memory_buckets)
}

fn validate_bucket_counts(
    bucket_size_in_pages: u16,
    memory_sizes_in_pages: &[u64; MAX_NUM_MEMORIES as usize],
    memory_buckets: &[Vec<BucketId>],
) -> Result<(), MemoryManagerLayoutError> {
    let bucket_size = u64::from(bucket_size_in_pages);
    for (id, size) in memory_sizes_in_pages.iter().enumerate() {
        let expected = size.div_ceil(bucket_size);
        if expected != memory_buckets[id].len() as u64 {
            return Err(MemoryManagerLayoutError::MemorySizeAndBucketsMismatch(id));
        }
    }
    Ok(())
}
