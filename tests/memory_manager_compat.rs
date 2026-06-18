use ic_sqlite_vfs::config::STABLE_PAGE_SIZE;
use ic_sqlite_vfs::test_support::Memory;
use ic_sqlite_vfs::DefaultMemoryImpl;
use ic_sqlite_vfs::{MemoryId, MemoryManager};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use upstream_ic_stable_structures::memory_manager::{
    MemoryId as UpstreamMemoryId, MemoryManager as UpstreamMemoryManager,
};
use upstream_ic_stable_structures::Memory as UpstreamMemory;

#[test]
fn memory_manager_reloads_interleaved_bucket_layout() {
    let backing = DefaultMemoryImpl::default();
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let first = manager.get(MemoryId::new(1));
    let second = manager.get(MemoryId::new(2));

    assert_eq!(first.grow(2), 0);
    assert_eq!(second.grow(1), 0);
    first.write(STABLE_PAGE_SIZE - 1, &[1, 2, 3]);
    second.write(0, &[4, 5, 6]);

    let mut magic = [0_u8; 3];
    Memory::read(&backing, 0, &mut magic);
    assert_eq!(&magic, b"MGR");

    let reloaded = MemoryManager::init(backing);
    let first = reloaded.get(MemoryId::new(1));
    let second = reloaded.get(MemoryId::new(2));
    let mut first_bytes = [0_u8; 3];
    let mut second_bytes = [0_u8; 3];

    first.read(STABLE_PAGE_SIZE - 1, &mut first_bytes);
    second.read(0, &mut second_bytes);

    assert_eq!(first_bytes, [1, 2, 3]);
    assert_eq!(second_bytes, [4, 5, 6]);
}

#[test]
fn memory_manager_matches_upstream_layout_for_valid_operations() {
    assert_matches_upstream_layout(1);
    assert_matches_upstream_layout(128);
}

#[test]
fn pbt_memory_manager_matches_upstream_layout_for_random_operations() {
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
                assert_random_ops_match_upstream(bucket_size, &operations);
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
    let backing = FailingGrowMemory::new(1);
    let manager = MemoryManager::init_with_bucket_size(backing.clone(), 1);
    let memory = manager.get(MemoryId::new(0));

    assert_eq!(memory.grow(1), -1);
    assert_eq!(memory.size(), 0);
    assert_eq!(backing.allocation_owner(0), 255);

    backing.set_max_pages(2);
    let reloaded = MemoryManager::init(backing.clone());
    let reloaded_memory = reloaded.get(MemoryId::new(0));

    assert_eq!(reloaded_memory.size(), 0);
    assert_eq!(backing.allocation_owner(0), 255);
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
    assert_eq!(Memory::grow(&backing, 1), 0);
    Memory::write(&backing, 0, &[b'M', b'G', b'R', 1]);

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
        assert_eq!(backing.allocation_owner(0), 255);
    }
}

fn assert_matches_upstream_layout(bucket_size_in_pages: u16) {
    let local_backing = DefaultMemoryImpl::default();
    let upstream_backing = upstream_ic_stable_structures::VectorMemory::default();
    let local_manager =
        MemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let upstream_manager = UpstreamMemoryManager::init_with_bucket_size(
        upstream_backing.clone(),
        bucket_size_in_pages,
    );
    let local_memories: Vec<_> = (0_u8..5)
        .map(|id| local_manager.get(MemoryId::new(id)))
        .collect();
    let upstream_memories: Vec<_> = (0_u8..5)
        .map(|id| upstream_manager.get(UpstreamMemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; 5];

    assert_eq!(
        local_backing.borrow().as_slice(),
        upstream_backing.borrow().as_slice()
    );

    for step in 0..384_u64 {
        let id = usize::try_from(step % sizes.len() as u64).unwrap();
        match step % 4 {
            0 => {
                let pages = (step.wrapping_mul(7) % 5) + 1;
                let local_old = Memory::grow(&local_memories[id], pages);
                let upstream_old = UpstreamMemory::grow(&upstream_memories[id], pages);
                assert_eq!(
                    local_old, upstream_old,
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
                UpstreamMemory::write(&upstream_memories[id], offset, &bytes);
            }
            _ if sizes[id] > 0 => {
                let capacity = sizes[id] * STABLE_PAGE_SIZE;
                let len = ((step.wrapping_mul(17) % 2048) + 1).min(capacity) as usize;
                let offset = step.wrapping_mul(4_099) % (capacity - len as u64 + 1);
                let mut local = vec![0_u8; len];
                let mut upstream = vec![0_u8; len];

                Memory::read(&local_memories[id], offset, &mut local);
                UpstreamMemory::read(&upstream_memories[id], offset, &mut upstream);
                assert_eq!(local, upstream, "read mismatch at step {step}");
            }
            _ => {}
        }

        for memory_id in 0..sizes.len() {
            assert_eq!(
                Memory::size(&local_memories[memory_id]),
                UpstreamMemory::size(&upstream_memories[memory_id]),
                "memory size mismatch at step {step}"
            );
        }
        assert_eq!(
            local_backing.borrow().as_slice(),
            upstream_backing.borrow().as_slice(),
            "stable layout diverged at step {step}"
        );
    }

    assert_can_reload_with_either_manager(local_backing.clone(), upstream_backing.clone(), &sizes);
}

fn assert_can_reload_with_either_manager(
    local_backing: DefaultMemoryImpl,
    upstream_backing: upstream_ic_stable_structures::VectorMemory,
    sizes: &[u64; 5],
) {
    let local_from_upstream = MemoryManager::init(upstream_backing.clone());
    let upstream_from_local = UpstreamMemoryManager::init(local_backing.clone());

    for id in 0_u8..5 {
        let local = local_from_upstream.get(MemoryId::new(id));
        let upstream = upstream_from_local.get(UpstreamMemoryId::new(id));
        assert_eq!(Memory::size(&local), sizes[id as usize]);
        assert_eq!(UpstreamMemory::size(&upstream), sizes[id as usize]);
    }
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

fn assert_random_ops_match_upstream(bucket_size_in_pages: u16, operations: &[Operation]) {
    let local_backing = DefaultMemoryImpl::default();
    let upstream_backing = upstream_ic_stable_structures::VectorMemory::default();
    let local_manager =
        MemoryManager::init_with_bucket_size(local_backing.clone(), bucket_size_in_pages);
    let upstream_manager = UpstreamMemoryManager::init_with_bucket_size(
        upstream_backing.clone(),
        bucket_size_in_pages,
    );
    let local_memories: Vec<_> = (0_u8..5)
        .map(|id| local_manager.get(MemoryId::new(id)))
        .collect();
    let upstream_memories: Vec<_> = (0_u8..5)
        .map(|id| upstream_manager.get(UpstreamMemoryId::new(id)))
        .collect();
    let mut sizes = [0_u64; 5];

    for (step, operation) in operations.iter().enumerate() {
        match *operation {
            Operation::Grow { id, pages_seed } => {
                let pages = projected_grow_pages(pages_seed, bucket_size_in_pages);
                let local_old = Memory::grow(&local_memories[id], pages);
                let upstream_old = UpstreamMemory::grow(&upstream_memories[id], pages);
                assert_eq!(
                    local_old, upstream_old,
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
                UpstreamMemory::write(&upstream_memories[id], offset, &bytes);
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
                let mut upstream = vec![0_u8; len];

                Memory::read(&local_memories[id], offset, &mut local);
                UpstreamMemory::read(&upstream_memories[id], offset, &mut upstream);
                assert_eq!(local, upstream, "read mismatch at step {step}");
            }
            _ => {}
        }

        for memory_id in 0..sizes.len() {
            assert_eq!(
                Memory::size(&local_memories[memory_id]),
                UpstreamMemory::size(&upstream_memories[memory_id]),
                "memory size mismatch at step {step}"
            );
        }
        assert_eq!(
            local_backing.borrow().as_slice(),
            upstream_backing.borrow().as_slice(),
            "stable layout diverged at step {step}"
        );
    }

    assert_can_reload_with_either_manager(local_backing, upstream_backing, &sizes);
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

    fn allocation_owner(&self, bucket: u64) -> u8 {
        const HEADER_SIZE: u64 = 3 + 1 + 2 + 2 + 32 + 255 * 8;

        self.bytes.borrow()[(HEADER_SIZE + bucket) as usize]
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

    fn allocation_owner(&self, bucket: u64) -> u8 {
        const HEADER_SIZE: u64 = 3 + 1 + 2 + 2 + 32 + 255 * 8;

        self.bytes.borrow()[(HEADER_SIZE + bucket) as usize]
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
