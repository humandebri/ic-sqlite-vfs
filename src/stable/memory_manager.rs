//! Minimal fork of `ic-stable-structures` MemoryManager 0.7 layout.
//!
//! The fork keeps the existing on-stable-memory format, but removes unrelated
//! stable data structures from this crate's dependency graph.

use crate::config::STABLE_PAGE_SIZE;
pub use crate::stable::memory_layout::MemoryId;
use crate::stable::memory_layout::{
    bucket_allocations_address, read_u64, write_growing, BucketCache, BucketId, VirtualSegment,
    BUCKETS_OFFSET_IN_BYTES, BUCKETS_OFFSET_IN_PAGES, BUCKET_SIZE_IN_PAGES, HEADER_RESERVED_BYTES,
    HEADER_SIZE, LAYOUT_VERSION, MAGIC, MAX_NUM_BUCKETS, MAX_NUM_MEMORIES,
    UNALLOCATED_BUCKET_MARKER,
};
use crate::stable::raw_memory::Memory;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct MemoryManager<M: Memory> {
    inner: Rc<RefCell<MemoryManagerInner<M>>>,
}

impl<M: Memory> MemoryManager<M> {
    pub fn init(memory: M) -> Self {
        Self::init_with_bucket_size(memory, BUCKET_SIZE_IN_PAGES as u16)
    }

    pub fn init_with_bucket_size(memory: M, bucket_size_in_pages: u16) -> Self {
        Self {
            inner: Rc::new(RefCell::new(MemoryManagerInner::init(
                memory,
                bucket_size_in_pages,
            ))),
        }
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

    fn new(memory: M, bucket_size_in_pages: u16) -> Self {
        let manager = Self {
            memory,
            allocated_buckets: 0,
            bucket_size_in_pages,
            memory_sizes_in_pages: [0; MAX_NUM_MEMORIES as usize],
            memory_buckets: vec![Vec::new(); MAX_NUM_MEMORIES as usize],
        };
        manager.save_header();
        write_growing(
            &manager.memory,
            bucket_allocations_address(BucketId(0)),
            &[UNALLOCATED_BUCKET_MARKER; MAX_NUM_BUCKETS as usize],
        );
        manager
    }

    fn load(memory: M) -> Self {
        let mut header = vec![0_u8; HEADER_SIZE as usize];
        memory.read(0, &mut header);
        assert_eq!(&header[0..3], MAGIC, "Bad magic.");
        assert_eq!(header[3], LAYOUT_VERSION, "Unsupported version.");

        let allocated_buckets = u16::from_le_bytes([header[4], header[5]]);
        let bucket_size_in_pages = u16::from_le_bytes([header[6], header[7]]);
        let mut memory_sizes_in_pages = [0_u64; MAX_NUM_MEMORIES as usize];
        let mut offset = 3 + 1 + 2 + 2 + HEADER_RESERVED_BYTES;
        for size in &mut memory_sizes_in_pages {
            *size = read_u64(&header[offset..offset + 8]);
            offset += 8;
        }

        let mut buckets = vec![0_u8; MAX_NUM_BUCKETS as usize];
        memory.read(bucket_allocations_address(BucketId(0)), &mut buckets);
        let mut memory_buckets = vec![Vec::new(); MAX_NUM_MEMORIES as usize];
        for (bucket, owner) in buckets.into_iter().enumerate() {
            if owner != UNALLOCATED_BUCKET_MARKER {
                memory_buckets[owner as usize].push(BucketId(bucket as u16));
            }
        }

        Self {
            memory,
            allocated_buckets,
            bucket_size_in_pages,
            memory_sizes_in_pages,
            memory_buckets,
        }
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
        if new_buckets + u64::from(self.allocated_buckets) > MAX_NUM_BUCKETS {
            return -1;
        }

        let memory_bucket = &mut self.memory_buckets[id.0 as usize];
        memory_bucket.reserve(new_buckets as usize);
        for _ in 0..new_buckets {
            let bucket = BucketId(self.allocated_buckets);
            memory_bucket.push(bucket);
            write_growing(&self.memory, bucket_allocations_address(bucket), &[id.0]);
            self.allocated_buckets += 1;
        }

        let pages_needed = BUCKETS_OFFSET_IN_PAGES
            + u64::from(self.bucket_size_in_pages) * u64::from(self.allocated_buckets);
        if pages_needed > self.memory.size() {
            let previous = self.memory.grow(pages_needed - self.memory.size());
            assert!(previous >= 0, "{id:?}: grow failed");
        }

        self.memory_sizes_in_pages[id.0 as usize] = new_size;
        self.save_header();
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
        if let Some(real) = cache.get(VirtualSegment::new(offset, count as u64)) {
            self.memory.read_unsafe(real, dst, count);
            return;
        }
        self.assert_bounds(id, offset, count as u64, "read");
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
        if let Some(real) = cache.get(VirtualSegment::new(offset, src.len() as u64)) {
            self.memory.write(real, src);
            return;
        }
        self.assert_bounds(id, offset, src.len() as u64, "write");
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
        assert!(
            end <= self.memory_size(id) * STABLE_PAGE_SIZE,
            "{id:?}: {operation} out of bounds"
        );
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
