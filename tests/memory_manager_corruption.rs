use ic_sqlite_vfs::stable::memory_manager::{MemoryId, MemoryManager};
use ic_sqlite_vfs::stable::raw_memory::{DefaultMemoryImpl, Memory};

const MAGIC: &[u8; 3] = b"MGR";
const LAYOUT_VERSION: u8 = 1;
const MAX_NUM_MEMORIES: usize = 255;
const MAX_NUM_BUCKETS: usize = 32_768;
const UNALLOCATED_BUCKET_MARKER: u8 = MAX_NUM_MEMORIES as u8;
const HEADER_RESERVED_BYTES: usize = 32;
const HEADER_SIZE: usize = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES + MAX_NUM_MEMORIES * 8;

#[test]
#[should_panic(expected = "unallocated bucket has owner")]
fn rejects_committed_header_with_all_zero_allocation_table() {
    let backing = corrupt_backing(0, 1, &[], None);

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "unallocated bucket has owner")]
fn rejects_owner_byte_when_allocated_bucket_count_is_zero() {
    let backing = corrupt_backing(0, 1, &[], Some(vec![(0, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "unallocated bucket has owner")]
fn rejects_owner_byte_after_allocated_bucket_range() {
    let backing = corrupt_backing(1, 1, &[(0, 1)], Some(vec![(0, 0), (10, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "unallocated bucket has owner")]
fn rejects_partial_grow_state_with_table_updated_before_header() {
    // Header still commits one bucket, but grow already wrote the next owner.
    let backing = corrupt_backing(1, 1, &[(0, 1)], Some(vec![(0, 0), (1, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "bucket size is zero")]
fn rejects_zero_bucket_size_in_header() {
    let backing = corrupt_backing(0, 0, &[], Some(Vec::new()));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "allocated buckets exceeds maximum")]
fn rejects_allocated_bucket_count_above_maximum() {
    let backing = corrupt_backing((MAX_NUM_BUCKETS + 1) as u16, 1, &[], Some(Vec::new()));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "size and buckets mismatch")]
fn rejects_memory_size_and_bucket_count_mismatch() {
    let backing = corrupt_backing(1, 1, &[(0, 2)], Some(vec![(0, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "size overflows bytes")]
fn rejects_memory_size_that_overflows_byte_capacity() {
    let backing = corrupt_backing(0, 1, &[(0, u64::MAX)], Some(Vec::new()));

    let _manager = MemoryManager::init(backing);
}

#[test]
#[should_panic(expected = "backing memory truncated")]
fn rejects_valid_table_with_truncated_bucket_storage() {
    let backing = corrupt_backing(1, 1, &[(0, 1)], Some(vec![(0, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
fn reloads_valid_layout_after_hardening() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let memory = manager.get(MemoryId::new(7));

    assert_eq!(memory.grow(1), 0);
    memory.write(0, &[42]);

    let reloaded = MemoryManager::init(backing);
    let memory = reloaded.get(MemoryId::new(7));
    let mut byte = [0_u8; 1];
    memory.read(0, &mut byte);

    assert_eq!(byte, [42]);
}

#[test]
fn grow_overflow_returns_minus_one() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing, 1);
    let first = manager.get(MemoryId::new(0));
    let second = manager.get(MemoryId::new(1));

    assert_eq!(first.grow(1), 0);
    assert_eq!(second.grow(u64::MAX), -1);
}

fn corrupt_backing(
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes: &[(u8, u64)],
    allocation_table: Option<Vec<(usize, u8)>>,
) -> DefaultMemoryImpl {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, 1), 0);
    backing.write(
        0,
        &header(allocated_buckets, bucket_size_in_pages, memory_sizes),
    );

    if let Some(owners) = allocation_table {
        let mut table = vec![UNALLOCATED_BUCKET_MARKER; MAX_NUM_BUCKETS];
        for (bucket, owner) in owners {
            table[bucket] = owner;
        }
        backing.write(HEADER_SIZE as u64, &table);
    }

    backing
}

fn header(
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes: &[(u8, u64)],
) -> [u8; HEADER_SIZE] {
    let mut header = [0_u8; HEADER_SIZE];
    header[0..3].copy_from_slice(MAGIC);
    header[3] = LAYOUT_VERSION;
    header[4..6].copy_from_slice(&allocated_buckets.to_le_bytes());
    header[6..8].copy_from_slice(&bucket_size_in_pages.to_le_bytes());

    for (id, size) in memory_sizes {
        let offset = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES + usize::from(*id) * 8;
        header[offset..offset + 8].copy_from_slice(&size.to_le_bytes());
    }

    header
}
