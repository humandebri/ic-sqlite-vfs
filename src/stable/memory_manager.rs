//! Minimal fork of `ic-stable-structures` MemoryManager 0.7 layout.
//!
//! The fork keeps the existing on-stable-memory format, but removes unrelated
//! stable data structures from this crate's dependency graph.

use crate::config::STABLE_PAGE_SIZE;
pub use crate::stable::memory_layout::MemoryId;
use crate::stable::memory_layout::{
    bucket_allocations_address, write_growing, BucketCache, BucketId, VirtualSegment,
    BUCKETS_OFFSET_IN_BYTES, BUCKETS_OFFSET_IN_PAGES, BUCKET_SIZE_IN_PAGES, HEADER_RESERVED_BYTES,
    HEADER_SIZE, LAYOUT_VERSION, MAGIC, MAX_NUM_BUCKETS, MAX_NUM_MEMORIES,
    UNALLOCATED_BUCKET_MARKER,
};
use crate::stable::memory_manager_validation::{load_validated_layout, try_load_validated_layout};
use crate::stable::raw_memory::{Memory, MemoryBackendIdentity};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemoryIdentity {
    backend: MemoryBackendIdentity,
    id: MemoryId,
}

#[derive(Clone)]
pub struct MemoryManager<M: Memory> {
    inner: Rc<RefCell<MemoryManagerInner<M>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryManagerInitError {
    #[error("bucket size must be greater than zero")]
    BucketSizeIsZero,
    #[error("non-empty memory does not contain a MemoryManager layout")]
    NonMemoryManagerLayout,
    #[error("{0}")]
    InvalidLayout(String),
}

impl<M: Memory> MemoryManager<M> {
    pub fn init(memory: M) -> Self {
        Self::init_with_bucket_size(memory, BUCKET_SIZE_IN_PAGES as u16)
    }

    pub fn init_strict(memory: M) -> Result<Self, MemoryManagerInitError> {
        Self::init_strict_with_bucket_size(memory, BUCKET_SIZE_IN_PAGES as u16)
    }

    pub fn init_with_bucket_size(memory: M, bucket_size_in_pages: u16) -> Self {
        if bucket_size_in_pages == 0 {
            panic!("bucket size must be greater than zero");
        }
        Self {
            inner: Rc::new(RefCell::new(MemoryManagerInner::init(
                memory,
                bucket_size_in_pages,
            ))),
        }
    }

    pub fn init_strict_with_bucket_size(
        memory: M,
        bucket_size_in_pages: u16,
    ) -> Result<Self, MemoryManagerInitError> {
        if bucket_size_in_pages == 0 {
            return Err(MemoryManagerInitError::BucketSizeIsZero);
        }
        Ok(Self {
            inner: Rc::new(RefCell::new(MemoryManagerInner::init_strict(
                memory,
                bucket_size_in_pages,
            )?)),
        })
    }

    pub fn get(&self, id: MemoryId) -> VirtualMemory<M> {
        VirtualMemory {
            id,
            memory_manager: Rc::clone(&self.inner),
            cache: BucketCache::new(),
        }
    }
}
#[derive(Clone)]
pub struct VirtualMemory<M: Memory> {
    id: MemoryId,
    memory_manager: Rc<RefCell<MemoryManagerInner<M>>>,
    cache: BucketCache,
}
impl<M: Memory> VirtualMemory<M> {
    pub(crate) fn identity(&self) -> MemoryIdentity {
        let inner = self.memory_manager.borrow();
        MemoryIdentity {
            backend: inner.memory.identity(),
            id: self.id,
        }
    }
}

impl<M: Memory> Memory for VirtualMemory<M> {
    fn size(&self) -> u64 {
        self.memory_manager.borrow().memory_size(self.id)
    }

    fn grow(&self, pages: u64) -> i64 {
        self.memory_manager.borrow_mut().grow(self.id, pages)
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        self.memory_manager
            .borrow()
            .read(self.id, offset, dst, &self.cache);
    }

    unsafe fn read_unsafe(&self, offset: u64, dst: *mut u8, count: usize) {
        self.memory_manager
            .borrow()
            .read_unsafe(self.id, offset, dst, count, &self.cache);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        self.memory_manager
            .borrow()
            .write(self.id, offset, src, &self.cache);
    }
}

#[derive(Clone)]
struct MemoryManagerInner<M: Memory> {
    memory: M,
    allocated_buckets: u16,
    bucket_size_in_pages: u16,
    memory_sizes_in_pages: [u64; MAX_NUM_MEMORIES as usize],
    memory_buckets: Vec<Vec<BucketId>>,
}
impl<M: Memory> MemoryManagerInner<M> {
    fn init(memory: M, bucket_size_in_pages: u16) -> Self {
        if memory.size() == 0 {
            return Self::new(memory, bucket_size_in_pages);
        }

        let mut magic = [0_u8; 3];
        memory.read(0, &mut magic);
        if &magic == MAGIC {
            Self::load(memory)
        } else {
            Self::new(memory, bucket_size_in_pages)
        }
    }

    fn init_strict(memory: M, bucket_size_in_pages: u16) -> Result<Self, MemoryManagerInitError> {
        if memory.size() == 0 {
            return Ok(Self::new(memory, bucket_size_in_pages));
        }

        let mut magic = [0_u8; 3];
        memory.read(0, &mut magic);
        if &magic != MAGIC {
            return Err(MemoryManagerInitError::NonMemoryManagerLayout);
        }
        Self::try_load(memory)
    }

    fn new(memory: M, bucket_size_in_pages: u16) -> Self {
        let manager = Self {
            memory,
            allocated_buckets: 0,
            bucket_size_in_pages,
            memory_sizes_in_pages: [0; MAX_NUM_MEMORIES as usize],
            memory_buckets: vec![Vec::new(); MAX_NUM_MEMORIES as usize],
        };
        write_growing(
            &manager.memory,
            bucket_allocations_address(BucketId(0)),
            &[UNALLOCATED_BUCKET_MARKER; MAX_NUM_BUCKETS as usize],
        );
        manager.save_header();
        manager
    }
    fn load(memory: M) -> Self {
        let mut header = vec![0_u8; HEADER_SIZE as usize];
        memory.read(0, &mut header);
        assert_eq!(&header[0..3], MAGIC, "Bad magic.");
        assert_eq!(header[3], LAYOUT_VERSION, "Unsupported version.");
        let layout = load_validated_layout(&memory, &header);

        Self {
            memory,
            allocated_buckets: layout.allocated_buckets,
            bucket_size_in_pages: layout.bucket_size_in_pages,
            memory_sizes_in_pages: layout.memory_sizes_in_pages,
            memory_buckets: layout.memory_buckets,
        }
    }

    fn try_load(memory: M) -> Result<Self, MemoryManagerInitError> {
        let mut header = vec![0_u8; HEADER_SIZE as usize];
        memory.read(0, &mut header);
        if &header[0..3] != MAGIC {
            return Err(MemoryManagerInitError::NonMemoryManagerLayout);
        }
        if header[3] != LAYOUT_VERSION {
            return Err(MemoryManagerInitError::InvalidLayout(
                "Unsupported version.".to_string(),
            ));
        }
        let layout = try_load_validated_layout(&memory, &header)
            .map_err(|error| MemoryManagerInitError::InvalidLayout(error.to_string()))?;

        Ok(Self {
            memory,
            allocated_buckets: layout.allocated_buckets,
            bucket_size_in_pages: layout.bucket_size_in_pages,
            memory_sizes_in_pages: layout.memory_sizes_in_pages,
            memory_buckets: layout.memory_buckets,
        })
    }

    fn save_header(&self) {
        let mut header = [0_u8; HEADER_SIZE as usize];
        header[0..3].copy_from_slice(MAGIC);
        header[3] = LAYOUT_VERSION;
        header[4..6].copy_from_slice(&self.allocated_buckets.to_le_bytes());
        header[6..8].copy_from_slice(&self.bucket_size_in_pages.to_le_bytes());
        let mut offset = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES;
        for size in self.memory_sizes_in_pages {
            header[offset..offset + 8].copy_from_slice(&size.to_le_bytes());
            offset += 8;
        }
        write_growing(&self.memory, 0, &header);
    }

    fn memory_size(&self, id: MemoryId) -> u64 {
        self.memory_sizes_in_pages[id.0 as usize]
    }

    fn grow(&mut self, id: MemoryId, pages: u64) -> i64 {
        let old_size = self.memory_size(id);
        let Some(new_size) = old_size.checked_add(pages) else {
            return -1;
        };
        let current_buckets = self.num_buckets_needed(old_size);
        let required_buckets = self.num_buckets_needed(new_size);
        let new_buckets = required_buckets - current_buckets;
        let Some(target_allocated_buckets) =
            new_buckets.checked_add(u64::from(self.allocated_buckets))
        else {
            return -1;
        };
        if target_allocated_buckets > MAX_NUM_BUCKETS {
            return -1;
        }
        let Ok(new_buckets_len) = usize::try_from(new_buckets) else {
            return -1;
        };
        let memory_bucket = &mut self.memory_buckets[id.0 as usize];
        if memory_bucket.try_reserve(new_buckets_len).is_err() {
            return -1;
        }
        let mut rollback_buckets = Vec::new();
        if rollback_buckets.try_reserve(new_buckets_len).is_err() {
            return -1;
        }

        let Some(data_pages) =
            u64::from(self.bucket_size_in_pages).checked_mul(target_allocated_buckets)
        else {
            return -1;
        };
        let Some(pages_needed) = BUCKETS_OFFSET_IN_PAGES.checked_add(data_pages) else {
            return -1;
        };
        let current_pages = self.memory.size();
        if pages_needed > current_pages {
            let previous = self.memory.grow(pages_needed - current_pages);
            if previous < 0 {
                return -1;
            }
        }

        let mut rollback = AllocationRollback {
            memory: std::ptr::addr_of!(self.memory),
            buckets: rollback_buckets,
            committed: false,
            _memory: std::marker::PhantomData,
        };
        for _ in 0..new_buckets {
            let bucket = BucketId(self.allocated_buckets);
            memory_bucket.push(bucket);
            write_growing(&self.memory, bucket_allocations_address(bucket), &[id.0]);
            rollback.buckets.push(bucket);
            self.allocated_buckets = self
                .allocated_buckets
                .checked_add(1)
                .expect("allocated bucket count overflow");
        }

        self.memory_sizes_in_pages[id.0 as usize] = new_size;
        self.save_header();
        rollback.committed = true;
        old_size as i64
    }

    fn read(&self, id: MemoryId, offset: u64, dst: &mut [u8], cache: &BucketCache) {
        unsafe { self.read_unsafe(id, offset, dst.as_mut_ptr(), dst.len(), cache) }
    }

    unsafe fn read_unsafe(
        &self,
        id: MemoryId,
        offset: u64,
        dst: *mut u8,
        count: usize,
        cache: &BucketCache,
    ) {
        if count == 0 {
            return;
        }
        self.assert_bounds(id, offset, count as u64, "read");
        if let Some(real) = cache.get(VirtualSegment::new(offset, count as u64)) {
            self.memory.read_unsafe(real, dst, count);
            return;
        }
        let mut bytes_read = 0_u64;
        self.for_each_bucket(id, offset, count as u64, cache, |address, len| {
            self.memory
                .read_unsafe(address, dst.add(bytes_read as usize), len as usize);
            bytes_read += len;
        });
    }

    fn write(&self, id: MemoryId, offset: u64, src: &[u8], cache: &BucketCache) {
        if src.is_empty() {
            return;
        }
        self.assert_bounds(id, offset, src.len() as u64, "write");
        if let Some(real) = cache.get(VirtualSegment::new(offset, src.len() as u64)) {
            self.memory.write(real, src);
            return;
        }
        let mut written = 0_u64;
        self.for_each_bucket(id, offset, src.len() as u64, cache, |address, len| {
            self.memory
                .write(address, &src[written as usize..(written + len) as usize]);
            written += len;
        });
    }

    fn for_each_bucket(
        &self,
        MemoryId(id): MemoryId,
        offset: u64,
        mut len: u64,
        cache: &BucketCache,
        mut f: impl FnMut(u64, u64),
    ) {
        let bucket_size = self.bucket_size_in_bytes();
        let buckets = self.memory_buckets[id as usize].as_slice();
        let mut bucket_idx = (offset / bucket_size) as usize;
        let mut bucket_offset = offset % bucket_size;
        while len > 0 {
            let bucket = buckets.get(bucket_idx).expect("bucket idx out of bounds");
            let bucket_address = self.bucket_address(*bucket);
            let segment_len = (bucket_size - bucket_offset).min(len);
            cache.store(
                VirtualSegment::new(bucket_idx as u64 * bucket_size, bucket_size),
                bucket_address,
            );
            f(bucket_address + bucket_offset, segment_len);
            len -= segment_len;
            bucket_idx += 1;
            bucket_offset = 0;
        }
    }

    fn assert_bounds(&self, id: MemoryId, offset: u64, len: u64, operation: &str) {
        let end = offset
            .checked_add(len)
            .unwrap_or_else(|| panic!("{id:?}: {operation} out of bounds"));
        let capacity = self
            .memory_size(id)
            .checked_mul(STABLE_PAGE_SIZE)
            .unwrap_or_else(|| panic!("{id:?}: {operation} out of bounds"));
        assert!(end <= capacity, "{id:?}: {operation} out of bounds");
    }

    fn bucket_size_in_bytes(&self) -> u64 {
        u64::from(self.bucket_size_in_pages) * STABLE_PAGE_SIZE
    }

    fn num_buckets_needed(&self, pages: u64) -> u64 {
        pages.div_ceil(u64::from(self.bucket_size_in_pages))
    }

    fn bucket_address(&self, id: BucketId) -> u64 {
        BUCKETS_OFFSET_IN_BYTES + self.bucket_size_in_bytes() * u64::from(id.0)
    }
}

struct AllocationRollback<'memory, M: Memory> {
    memory: *const M,
    buckets: Vec<BucketId>,
    committed: bool,
    _memory: std::marker::PhantomData<&'memory M>,
}

impl<M: Memory> Drop for AllocationRollback<'_, M> {
    fn drop(&mut self) {
        if self.committed || !std::thread::panicking() {
            return;
        }
        for bucket in self.buckets.iter().copied() {
            let memory = unsafe { &*self.memory };
            write_growing(
                memory,
                bucket_allocations_address(bucket),
                &[UNALLOCATED_BUCKET_MARKER],
            );
        }
    }
}
