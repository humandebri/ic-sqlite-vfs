//! Stable-memory backend used by the local MemoryManager fork.
//!
//! This module keeps only the byte-addressed memory trait and the native/IC
//! implementations required by SQLite's virtual-memory adapter.

use crate::config::STABLE_PAGE_SIZE;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryBackendIdentity {
    Ic0StableMemory,
    Address(*const ()),
}

pub trait Memory {
    fn identity(&self) -> MemoryBackendIdentity {
        MemoryBackendIdentity::Address(std::ptr::from_ref(self).cast::<()>())
    }

    fn size(&self) -> u64;
    fn grow(&self, pages: u64) -> i64;
    fn read(&self, offset: u64, dst: &mut [u8]);
    fn write(&self, offset: u64, src: &[u8]);

    unsafe fn read_unsafe(&self, offset: u64, dst: *mut u8, count: usize) {
        std::ptr::write_bytes(dst, 0, count);
        let slice = std::slice::from_raw_parts_mut(dst, count);
        self.read(offset, slice);
    }
}

#[cfg(target_arch = "wasm32")]
pub type DefaultMemoryImpl = Ic0StableMemory;

#[cfg(not(target_arch = "wasm32"))]
pub type DefaultMemoryImpl = VectorMemory;

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Default)]
pub struct Ic0StableMemory;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "ic0")]
extern "C" {
    fn stable64_size() -> u64;
    fn stable64_grow(additional_pages: u64) -> i64;
    fn stable64_read(dst: u64, offset: u64, size: u64);
    fn stable64_write(offset: u64, src: u64, size: u64);
}

#[cfg(target_arch = "wasm32")]
impl Memory for Ic0StableMemory {
    fn identity(&self) -> MemoryBackendIdentity {
        MemoryBackendIdentity::Ic0StableMemory
    }

    fn size(&self) -> u64 {
        unsafe { stable64_size() }
    }

    fn grow(&self, pages: u64) -> i64 {
        unsafe { stable64_grow(pages) }
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        unsafe { stable64_read(dst.as_mut_ptr() as u64, offset, dst.len() as u64) }
    }

    unsafe fn read_unsafe(&self, offset: u64, dst: *mut u8, count: usize) {
        stable64_read(dst as u64, offset, count as u64);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        unsafe { stable64_write(offset, src.as_ptr() as u64, src.len() as u64) }
    }
}

pub type VectorMemory = Rc<RefCell<Vec<u8>>>;

impl Memory for RefCell<Vec<u8>> {
    fn identity(&self) -> MemoryBackendIdentity {
        MemoryBackendIdentity::Address(std::ptr::from_ref(self).cast::<()>())
    }

    fn size(&self) -> u64 {
        self.borrow().len() as u64 / STABLE_PAGE_SIZE
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
        self.borrow_mut().resize(next_bytes as usize, 0);
        size as i64
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        let end = checked_end(offset, dst.len(), "read");
        dst.copy_from_slice(&self.borrow()[offset as usize..end as usize]);
    }

    unsafe fn read_unsafe(&self, offset: u64, dst: *mut u8, count: usize) {
        let end = checked_end(offset, count, "read");
        assert!(end as usize <= self.borrow().len(), "read: out of bounds");
        std::ptr::copy(self.borrow().as_ptr().add(offset as usize), dst, count);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        let end = checked_end(offset, src.len(), "write");
        self.borrow_mut()[offset as usize..end as usize].copy_from_slice(src);
    }
}

impl<M: Memory> Memory for Rc<M> {
    fn identity(&self) -> MemoryBackendIdentity {
        self.deref().identity()
    }

    fn size(&self) -> u64 {
        self.deref().size()
    }

    fn grow(&self, pages: u64) -> i64 {
        self.deref().grow(pages)
    }

    fn read(&self, offset: u64, dst: &mut [u8]) {
        self.deref().read(offset, dst);
    }

    unsafe fn read_unsafe(&self, offset: u64, dst: *mut u8, count: usize) {
        self.deref().read_unsafe(offset, dst, count);
    }

    fn write(&self, offset: u64, src: &[u8]) {
        self.deref().write(offset, src);
    }
}

fn checked_end(offset: u64, len: usize, operation: &str) -> u64 {
    let end = offset
        .checked_add(len as u64)
        .unwrap_or_else(|| panic!("{operation}: out of bounds"));
    assert!(end <= usize::MAX as u64, "{operation}: out of bounds");
    end
}
