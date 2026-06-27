use ic_sqlite_vfs::config::STABLE_PAGE_SIZE;
use ic_sqlite_vfs::test_support::Memory;
use ic_sqlite_vfs::DefaultMemoryImpl;
use ic_sqlite_vfs::{MemoryId, MemoryManager};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

const MAX_MEMORY_ID: u16 = 32_767;
const UNALLOCATED_BUCKET_MARKER: u16 = u16::MAX;
const HEADER_SIZE: u64 = 3 + 1 + 2 + 2 + 32 + 32_768 * 8;
const BUCKET_OWNER_SIZE: u64 = 2;
const TEST_MEMORY_IDS: [u16; 5] = [0, 1, 32_765, 32_766, 32_767];

#[test]
fn memory_manager_reloads_interleaved_bucket_layout() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let first = manager.get(MemoryId::new(MAX_MEMORY_ID - 1));
    let second = manager.get(MemoryId::new(MAX_MEMORY_ID));

    assert_eq!(first.grow(2), 0);
    assert_eq!(second.grow(1), 0);
    first.write(STABLE_PAGE_SIZE - 1, &[1, 2, 3]);
    second.write(0, &[4, 5, 6]);

    let mut magic = [0_u8; 3];
    Memory::read(&backing, 0, &mut magic);
    assert_eq!(&magic, b"MGR");

    let reloaded = MemoryManager::init(backing);
    let first = reloaded.get(MemoryId::new(MAX_MEMORY_ID - 1));
    let second = reloaded.get(MemoryId::new(MAX_MEMORY_ID));
    let mut first_bytes = [0_u8; 3];
    let mut second_bytes = [0_u8; 3];

    first.read(STABLE_PAGE_SIZE - 1, &mut first_bytes);
    second.read(0, &mut second_bytes);

    assert_eq!(first_bytes, [1, 2, 3]);
    assert_eq!(second_bytes, [4, 5, 6]);
}

#[test]
fn memory_manager_reloads_highest_memory_id() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let memory = manager.get(MemoryId::new(MAX_MEMORY_ID));

    assert_eq!(memory.grow(1), 0);
    memory.write(STABLE_PAGE_SIZE - 4, &[9, 8, 7, 6]);

    let reloaded = MemoryManager::init(backing);
    let memory = reloaded.get(MemoryId::new(MAX_MEMORY_ID));
    let mut bytes = [0_u8; 4];
    memory.read(STABLE_PAGE_SIZE - 4, &mut bytes);

    assert_eq!(bytes, [9, 8, 7, 6]);
}

#[test]
#[should_panic]
fn memory_id_rejects_first_reserved_id() {
    let _ = MemoryId::new(MAX_MEMORY_ID + 1);
}

#[test]
#[should_panic]
fn memory_id_rejects_unallocated_marker_id() {
    let _ = MemoryId::new(UNALLOCATED_BUCKET_MARKER);
}

#[test]
fn memory_manager_reloads_valid_operations() {
    assert_local_layout_roundtrip(1);
    assert_local_layout_roundtrip(128);
}

#[test]
fn pbt_memory_manager_reloads_random_operations() {
    let mut runner = TestRunner::new(Config {
        cases: 32,
        ..Config::default()
    });

    runner
        .run(
            &(
                prop::sample::select(vec![1_u16, 2, 7, 128]),
                operation_sequence(),
            ),
            |(bucket_size, operations)| {
                assert_random_ops_survive_reload(bucket_size, &operations);
                Ok(())
            },
        )
        .unwrap();
}

#[test]
#[should_panic(expected = "MemoryId(0): read out of bounds")]
fn cached_read_checks_logical_bounds() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init(backing);
    let memory = manager.get(MemoryId::new(0));

    assert_eq!(memory.grow(1), 0);
    memory.write(0, &[42]);

    let mut byte = [0_u8; 1];
    memory.read(STABLE_PAGE_SIZE, &mut byte);
}

#[test]
#[should_panic(expected = "MemoryId(0): write out of bounds")]
fn cached_write_checks_logical_bounds() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init(backing);
    let memory = manager.get(MemoryId::new(0));

    assert_eq!(memory.grow(1), 0);
    memory.write(0, &[42]);

    memory.write(STABLE_PAGE_SIZE, &[1]);
}

#[test]
#[should_panic(expected = "bucket size must be greater than zero")]
fn init_with_bucket_size_rejects_zero_bucket_size() {
    let backing = DefaultMemoryImpl::default();

    let _manager = MemoryManager::init_with_bucket_size(backing, 0);
}

#[test]
fn grow_failure_returns_minus_one_without_metadata_changes() {
    let backing = FailingGrowMemory::new(6);
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let memory = manager.get(MemoryId::new(0));

    assert_eq!(memory.grow(1), -1);
    assert_eq!(memory.size(), 0);
    assert_eq!(backing.allocation_owner(0), UNALLOCATED_BUCKET_MARKER);

    backing.set_max_pages(7);
    let reloaded = MemoryManager::init(backing.clone());
    let reloaded_memory = reloaded.get(MemoryId::new(0));

    assert_eq!(reloaded_memory.size(), 0);
    assert_eq!(backing.allocation_owner(0), UNALLOCATED_BUCKET_MARKER);
}

#[test]
fn strict_init_rejects_non_memory_manager_layout() {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, 1), 0);
    Memory::write(&backing, 0, b"not a memory manager");

    let error = match MemoryManager::init_strict(backing) {
        Ok(_) => panic!("strict init accepted non-memory-manager layout"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ic_sqlite_vfs::MemoryManagerInitError::NonMemoryManagerLayout
    ));
}

#[test]
fn strict_init_returns_error_for_invalid_memory_manager_layout() {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, 6), 0);
    Memory::write(&backing, 0, &[b'M', b'G', b'R', 2]);

    let error = match MemoryManager::init_strict(backing) {
        Ok(_) => panic!("strict init accepted invalid layout"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ic_sqlite_vfs::MemoryManagerInitError::InvalidLayout(message)
            if message.contains("bucket size is zero")
    ));
}

#[test]
fn strict_init_rejects_old_memory_manager_version() {
    let backing = DefaultMemoryImpl::default();
    assert_eq!(Memory::grow(&backing, 6), 0);
    Memory::write(&backing, 0, &[b'M', b'G', b'R', 1]);

    let error = match MemoryManager::init_strict(backing) {
        Ok(_) => panic!("strict init accepted old memory-manager layout"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ic_sqlite_vfs::MemoryManagerInitError::InvalidLayout(message)
            if message.contains("Unsupported version")
    ));
}

#[test]
fn grow_metadata_write_panic_rolls_back_allocation_table() {
    for fail_on_write in [1_u64, 2] {
        let backing = PanickingWriteMemory::default();
        let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
        let memory = manager.get(MemoryId::new(0));
        backing.reset_write_count();
        backing.set_fail_on_write(Some(fail_on_write));

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = memory.grow(1);
        }));

        assert!(result.is_err());
        backing.set_fail_on_write(None);
        let reloaded = MemoryManager::init(backing.clone());
        let reloaded_memory = reloaded.get(MemoryId::new(0));
        assert_eq!(reloaded_memory.size(), 0);
        assert_eq!(backing.allocation_owner(0), UNALLOCATED_BUCKET_MARKER);
    }
}

fn assert_local_layout_roundtrip(bucket_size_in_pages: u16) {
    let local_backing = DefaultMemoryImpl::default();
    let local_manager =
        MemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let local_memories: Vec<_> = TEST_MEMORY_IDS
        .iter()
        .copied()
        .map(|id| local_manager.get(MemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; 5];
    let mut writes = Vec::new();

    for step in 0..384_u64 {
        let id = usize::try_from(step % sizes.len() as u64).unwrap();
        match step % 4 {
            0 => {
                let pages = (step.wrapping_mul(7) % 5) + 1;
                let local_old = Memory::grow(&local_memories[id], pages);
                assert_eq!(
                    local_old, sizes[id] as i64,
                    "grow old size mismatch at step {step}"
                );
                sizes[id] += pages;
            }
            1 | 2 if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = ((step.wrapping_mul(13) % 4096) + 1).min(capacity) as usize;
                let offset = step.wrapping_mul(7_919) % (capacity - len as u64 + 1);
                let bytes = deterministic_bytes(step, len);

                Memory::write(&local_memories[id], offset, &bytes);
                writes.push((id, offset, bytes));
            }
            _ if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = ((step.wrapping_mul(17) % 2048) + 1).min(capacity) as usize;
                let offset = step.wrapping_mul(4_099) % (capacity - len as u64 + 1);
                let mut local = vec![0_u8; len];

                Memory::read(&local_memories[id], offset, &mut local);
            }
            _ => {}
        }

        for memory_id in 0..sizes.len() {
            assert_eq!(
                Memory::size(&local_memories[memory_id]),
                sizes[memory_id],
                "memory size mismatch at step {step}"
            );
        }
    }

    assert_can_reload_manager(local_backing, &sizes, &writes);
}

fn assert_can_reload_manager(
    local_backing: DefaultMemoryImpl,
    sizes: &[u64; 5],
    writes: &[(usize, u64, Vec<u8>)],
) {
    let reloaded = MemoryManager::init(local_backing);
    let memories: Vec<_> = TEST_MEMORY_IDS
        .iter()
        .copied()
        .map(|id| reloaded.get(MemoryId::new(id)))
        .collect();

    for id in 0..sizes.len() {
        assert_eq!(Memory::size(&memories[id]), sizes[id]);
    }
    for (write_index, (id, offset, bytes)) in writes.iter().enumerate() {
        let expected = expected_bytes_after_later_writes(writes, write_index);
        let mut reloaded_bytes = vec![0_u8; bytes.len()];
        Memory::read(&memories[*id], *offset, &mut reloaded_bytes);
        assert!(
            reloaded_bytes == expected,
            "reloaded bytes mismatch for id {id} offset {offset} len {}",
            bytes.len()
        );
    }
}

fn expected_bytes_after_later_writes(
    writes: &[(usize, u64, Vec<u8>)],
    write_index: usize,
) -> Vec<u8> {
    let (id, offset, bytes) = &writes[write_index];
    let mut expected = bytes.clone();
    let end = *offset + u64::try_from(bytes.len()).unwrap();
    for (later_id, later_offset, later_bytes) in &writes[write_index + 1..] {
        if later_id != id {
            continue;
        }
        let later_end = *later_offset + u64::try_from(later_bytes.len()).unwrap();
        let overlap_start = (*offset).max(*later_offset);
        let overlap_end = end.min(later_end);
        if overlap_start >= overlap_end {
            continue;
        }
        let expected_start = usize::try_from(overlap_start - *offset).unwrap();
        let later_start = usize::try_from(overlap_start - *later_offset).unwrap();
        let overlap_len = usize::try_from(overlap_end - overlap_start).unwrap();
        expected[expected_start..expected_start + overlap_len]
            .copy_from_slice(&later_bytes[later_start..later_start + overlap_len]);
    }
    expected
}

#[derive(Clone, Debug)]
enum Operation {
    Grow {
        id: usize,
        pages_seed: u64,
    },
    Write {
        id: usize,
        offset_seed: u64,
        len_seed: u64,
        byte_seed: u64,
    },
    Read {
        id: usize,
        offset_seed: u64,
        len_seed: u64,
    },
}

fn operation_sequence() -> impl Strategy<Value = Vec<Operation>> {
    let grow =
        (0_usize..5, any::<u64>()).prop_map(|(id, pages_seed)| Operation::Grow { id, pages_seed });
    let write = (0_usize..5, any::<u64>(), any::<u64>(), any::<u64>()).prop_map(
        |(id, offset_seed, len_seed, byte_seed)| Operation::Write {
            id,
            offset_seed,
            len_seed,
            byte_seed,
        },
    );
    let read = (0_usize..5, any::<u64>(), any::<u64>()).prop_map(|(id, offset_seed, len_seed)| {
        Operation::Read {
            id,
            offset_seed,
            len_seed,
        }
    });
    proptest::collection::vec(prop_oneof![grow, write, read], 0..160)
}

fn assert_random_ops_survive_reload(bucket_size_in_pages: u16, operations: &[Operation]) {
    let local_backing = DefaultMemoryImpl::default();
    let local_manager =
        MemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let local_memories: Vec<_> = TEST_MEMORY_IDS
        .iter()
        .copied()
        .map(|id| local_manager.get(MemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; 5];
    let mut writes = Vec::new();

    for (step, operation) in operations.iter().enumerate() {
        match *operation {
            Operation::Grow { id, pages_seed } => {
                let pages = projected_grow_pages(pages_seed, bucket_size_in_pages);
                let local_old = Memory::grow(&local_memories[id], pages);
                assert_eq!(
                    local_old, sizes[id] as i64,
                    "grow old size mismatch at step {step}"
                );
                if local_old >= 0 {
                    sizes[id] = sizes[id].checked_add(pages).expect("test grow stays small");
                }
            }
            Operation::Write {
                id,
                offset_seed,
                len_seed,
                byte_seed,
            } if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = projected_len(len_seed, capacity, bucket_size_in_pages);
                let offset = projected_offset(offset_seed, capacity, len, bucket_size_in_pages);
                let bytes = deterministic_bytes(byte_seed, len);

                Memory::write(&local_memories[id], offset, &bytes);
                writes.push((id, offset, bytes));
            }
            Operation::Read {
                id,
                offset_seed,
                len_seed,
            } if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = projected_len(len_seed, capacity, bucket_size_in_pages);
                let offset = projected_offset(offset_seed, capacity, len, bucket_size_in_pages);
                let mut local = vec![0_u8; len];

                Memory::read(&local_memories[id], offset, &mut local);
            }
            _ => {}
        }

        for memory_id in 0..sizes.len() {
            assert_eq!(
                Memory::size(&local_memories[memory_id]),
                sizes[memory_id],
                "memory size mismatch at step {step}"
            );
        }
    }

    assert_can_reload_manager(local_backing, &sizes, &writes);
}

fn projected_grow_pages(seed: u64, bucket_size_in_pages: u16) -> u64 {
    let bucket = u64::from(bucket_size_in_pages);
    let candidates = [
        0,
        1,
        bucket.saturating_sub(1),
        bucket,
        bucket.saturating_add(1),
        seed % 5,
    ];
    candidates[usize::try_from(seed % candidates.len() as u64).unwrap()]
}

fn projected_len(seed: u64, capacity: u64, bucket_size_in_pages: u16) -> usize {
    let candidates = [
        1,
        2,
        STABLE_PAGE_SIZE.saturating_sub(1),
        STABLE_PAGE_SIZE,
        STABLE_PAGE_SIZE.saturating_add(1),
        u64::from(bucket_size_in_pages).saturating_add(1),
        (seed % 4096).saturating_add(1),
    ];
    let index = usize::try_from(seed % candidates.len() as u64).unwrap();
    usize::try_from(candidates[index].min(capacity)).unwrap()
}

fn projected_offset(seed: u64, capacity: u64, len: usize, bucket_size_in_pages: u16) -> u64 {
    let max = capacity - u64::try_from(len).unwrap();
    let bucket_bytes = u64::from(bucket_size_in_pages) * STABLE_PAGE_SIZE;
    let candidates = [
        0,
        1,
        STABLE_PAGE_SIZE.saturating_sub(1),
        STABLE_PAGE_SIZE,
        STABLE_PAGE_SIZE.saturating_add(1),
        bucket_bytes.saturating_sub(1),
        bucket_bytes,
        bucket_bytes.saturating_add(1),
        max.saturating_sub(1),
        max,
        seed % (max + 1),
    ];
    let index = usize::try_from(seed % candidates.len() as u64).unwrap();
    candidates[index].min(max)
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add(index as u64).wrapping_mul(31) as u8)
        .collect()
}

#[derive(Clone)]
struct FailingGrowMemory {
    bytes: Rc<RefCell<Vec<u8>>>,
    max_pages: Rc<Cell<u64>>,
}

impl FailingGrowMemory {
    fn new(max_pages: u64) -> Self {
        Self {
            bytes: Rc::new(RefCell::new(Vec::new())),
            max_pages: Rc::new(Cell::new(max_pages)),
        }
    }

    fn set_max_pages(&self, max_pages: u64) {
        self.max_pages.set(max_pages);
    }

    fn allocation_owner(&self, bucket: u64) -> u16 {
        let offset = usize::try_from(HEADER_SIZE + bucket * BUCKET_OWNER_SIZE).unwrap();
        u16::from_le_bytes(self.bytes.borrow()[offset..offset + 2].try_into().unwrap())
    }
}

impl Memory for FailingGrowMemory {
    fn size(&self) -> u64 {
        self.bytes.borrow().len() as u64 / STABLE_PAGE_SIZE
    }

    fn grow(&self, pages: u64) -> i64 {
        let size = self.size();
        let Some(next_size) = size.checked_add(pages) else {
            return -1;
        };
        if next_size > self.max_pages.get() {
            return -1;
        }
        let Some(next_bytes) = next_size.checked_mul(STABLE_PAGE_SIZE) else {
            return -1;
        };
        if next_bytes > usize::MAX as u64 {
            return -1;
        }
        self.bytes.borrow_mut().resize(next_bytes as usize, 0);
        size as i64
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        let end = checked_end(offset, dst.len(), "read");
        dst.copy_from_slice(&self.bytes.borrow()[offset as usize..end]);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        let end = checked_end(offset, src.len(), "write");
        self.bytes.borrow_mut()[offset as usize..end].copy_from_slice(src);
    }
}

#[derive(Clone, Default)]
struct PanickingWriteMemory {
    bytes: Rc<RefCell<Vec<u8>>>,
    fail_on_write: Rc<Cell<Option<u64>>>,
    write_count: Rc<Cell<u64>>,
}

impl PanickingWriteMemory {
    fn set_fail_on_write(&self, ordinal: Option<u64>) {
        self.fail_on_write.set(ordinal);
    }

    fn reset_write_count(&self) {
        self.write_count.set(0);
    }

    fn allocation_owner(&self, bucket: u64) -> u16 {
        let offset = usize::try_from(HEADER_SIZE + bucket * BUCKET_OWNER_SIZE).unwrap();
        u16::from_le_bytes(self.bytes.borrow()[offset..offset + 2].try_into().unwrap())
    }
}

impl Memory for PanickingWriteMemory {
    fn size(&self) -> u64 {
        self.bytes.borrow().len() as u64 / STABLE_PAGE_SIZE
    }

    fn grow(&self, pages: u64) -> i64 {
        let size = self.size();
        let Some(next_size) = size.checked_add(pages) else {
            return -1;
        };
        let Some(next_bytes) = next_size.checked_mul(STABLE_PAGE_SIZE) else {
            return -1;
        };
        if next_bytes > usize::MAX as u64 {
            return -1;
        }
        self.bytes.borrow_mut().resize(next_bytes as usize, 0);
        size as i64
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        let end = checked_end(offset, dst.len(), "read");
        dst.copy_from_slice(&self.bytes.borrow()[offset as usize..end]);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        let count = self.write_count.get().saturating_add(1);
        self.write_count.set(count);
        if self.fail_on_write.get() == Some(count) {
            panic!("write failpoint");
        }
        let end = checked_end(offset, src.len(), "write");
        self.bytes.borrow_mut()[offset as usize..end].copy_from_slice(src);
    }
}

fn checked_end(offset: u64, len: usize, operation: &str) -> usize {
    let end = offset
        .checked_add(len as u64)
        .unwrap_or_else(|| panic!("{operation}: out of bounds"));
    assert!(end <= usize::MAX as u64, "{operation}: out of bounds");
    end as usize
}
