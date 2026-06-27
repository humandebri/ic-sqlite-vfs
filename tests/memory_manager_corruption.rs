use ic_sqlite_vfs::test_support::Memory;
use ic_sqlite_vfs::DefaultMemoryImpl;
use ic_sqlite_vfs::{MemoryId, MemoryManager};

const MAGIC: &[u8; 3] = b"MGR";
const LAYOUT_VERSION: u8 = 2;
const MAX_NUM_MEMORIES: usize = 32_768;
const MAX_NUM_BUCKETS: usize = 32_768;
const UNALLOCATED_BUCKET_MARKER: u16 = u16::MAX;
const RESERVED_BUCKET_OWNER: u16 = 32_768;
const HEADER_RESERVED_BYTES: usize = 32;
const HEADER_SIZE: usize = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES + MAX_NUM_MEMORIES * 8;
const BUCKET_OWNER_SIZE: usize = 2;
const BUCKET_ALLOCATIONS_SIZE: usize = MAX_NUM_BUCKETS * BUCKET_OWNER_SIZE;
const METADATA_PAGES: u64 = 6;

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
#[should_panic(expected = "reserved owner")]
fn rejects_reserved_owner_for_allocated_bucket() {
    let backing = corrupt_backing(1, 1, &[(0, 1)], Some(vec![(0, RESERVED_BUCKET_OWNER)]));

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
    let backing = truncated_bucket_storage_backing(1, 1, &[(0, 1)], Some(vec![(0, 0)]));

    let _manager = MemoryManager::init(backing);
}

#[test]
fn strict_init_returns_error_for_truncated_metadata_before_owner_table_read() {
    let backing = truncated_metadata_backing(1, 1, &[(0, 1)]);

    let error = match MemoryManager::init_strict(backing) {
        Ok(_) => panic!("strict init accepted truncated metadata"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ic_sqlite_vfs::MemoryManagerInitError::InvalidLayout(message)
            if message.contains("backing memory truncated")
    ));
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
    memory_sizes: &[(u16, u64)],
    allocation_table: Option<Vec<(usize, u16)>>,
) -> DefaultMemoryImpl {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(
        Memory::grow(
            &backing,
            backing_pages(allocated_buckets, bucket_size_in_pages)
        ),
        0
    );
    backing.write(
        0,
        &header(allocated_buckets, bucket_size_in_pages, memory_sizes),
    );

    if let Some(owners) = allocation_table {
        let mut table = vec![0_u8; BUCKET_ALLOCATIONS_SIZE];
        for owner in table.chunks_exact_mut(BUCKET_OWNER_SIZE) {
            owner.copy_from_slice(&UNALLOCATED_BUCKET_MARKER.to_le_bytes());
        }
        for (bucket, owner) in owners {
            let offset = bucket * BUCKET_OWNER_SIZE;
            table[offset..offset + BUCKET_OWNER_SIZE].copy_from_slice(&owner.to_le_bytes());
        }
        backing.write(HEADER_SIZE as u64, &table);
    }

    backing
}

fn backing_pages(allocated_buckets: u16, bucket_size_in_pages: u16) -> u64 {
    if bucket_size_in_pages == 0 || usize::from(allocated_buckets) > MAX_NUM_BUCKETS {
        return METADATA_PAGES;
    }
    METADATA_PAGES + u64::from(allocated_buckets) * u64::from(bucket_size_in_pages)
}

fn truncated_bucket_storage_backing(
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes: &[(u16, u64)],
    allocation_table: Option<Vec<(usize, u16)>>,
) -> DefaultMemoryImpl {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, METADATA_PAGES), 0);
    backing.write(
        0,
        &header(allocated_buckets, bucket_size_in_pages, memory_sizes),
    );

    if let Some(owners) = allocation_table {
        let mut table = vec![0_u8; BUCKET_ALLOCATIONS_SIZE];
        for owner in table.chunks_exact_mut(BUCKET_OWNER_SIZE) {
            owner.copy_from_slice(&UNALLOCATED_BUCKET_MARKER.to_le_bytes());
        }
        for (bucket, owner) in owners {
            let offset = bucket * BUCKET_OWNER_SIZE;
            table[offset..offset + BUCKET_OWNER_SIZE].copy_from_slice(&owner.to_le_bytes());
        }
        backing.write(HEADER_SIZE as u64, &table);
    }

    backing
}

fn truncated_metadata_backing(
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes: &[(u16, u64)],
) -> DefaultMemoryImpl {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, METADATA_PAGES - 1), 0);
    backing.write(
        0,
        &header(allocated_buckets, bucket_size_in_pages, memory_sizes),
    );
    backing
}

fn header(
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes: &[(u16, u64)],
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
