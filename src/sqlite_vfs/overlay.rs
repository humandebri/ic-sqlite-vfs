//! Per-message write overlay for the main SQLite database image.
//!
//! SQLite may write dirty pages before it knows whether a transaction will
//! commit. The overlay keeps those writes in heap memory so a normal `Result`
//! error can discard them without changing the active stable-memory image.

use crate::sqlite_vfs::stable_blob;
use crate::stable::memory::StableMemoryError;
use std::cell::RefCell;

#[derive(Debug)]
pub struct Overlay {
    base_size: u64,
    size: u64,
    writes: Vec<OverlayWrite>,
}

#[derive(Debug)]
struct OverlayWrite {
    offset: u64,
    bytes: Vec<u8>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

impl Overlay {
    pub fn new(base_size: u64) -> Self {
        Self {
            base_size,
            size: base_size,
            writes: Vec::new(),
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == self.base_size && self.writes.is_empty()
    }

    pub fn max_end(&self) -> Result<u64, StableMemoryError> {
        let mut end = self.size;
        for write in &self.writes {
            let len =
                u64::try_from(write.bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
            end = end.max(checked_add(write.offset, len)?);
        }
        Ok(end)
    }

    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> Result<bool, StableMemoryError> {
        dst.fill(0);
        if dst.is_empty() {
            return Ok(true);
        }
        if offset >= self.size {
            return Ok(false);
        }

        let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let copied = requested.min(self.size - offset);
        let copied_len = usize::try_from(copied).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let _ = stable_blob::read_base_at(offset, &mut dst[..copied_len])?;
        self.overlay_writes(offset, &mut dst[..copied_len])?;
        Ok(copied == requested)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let len = u64::try_from(bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let end = checked_add(offset, len)?;
        self.size = self.size.max(end);
        self.writes.push(OverlayWrite {
            offset,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    pub fn truncate(&mut self, size: u64) -> Result<(), StableMemoryError> {
        self.size = size;

        let mut trimmed = Vec::new();
        for mut write in std::mem::take(&mut self.writes) {
            if write.offset >= size {
                continue;
            }
            let len =
                u64::try_from(write.bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
            let end = checked_add(write.offset, len)?;
            if end <= size {
                trimmed.push(write);
                continue;
            }
            let keep = usize::try_from(size - write.offset)
                .map_err(|_| StableMemoryError::OffsetOverflow)?;
            write.bytes.truncate(keep);
            trimmed.push(write);
        }
        self.writes = trimmed;
        Ok(())
    }

    pub fn read_merged_chunk(&self, offset: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
        dst.fill(0);
        if dst.is_empty() {
            return Ok(());
        }
        let requested = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let end = checked_add(offset, requested)?;
        if end > self.size {
            return Err(StableMemoryError::OffsetOverflow);
        }
        let _ = stable_blob::read_base_at(offset, dst)?;
        self.overlay_writes(offset, dst)
    }

    fn overlay_writes(&self, base: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
        let dst_len = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
        let dst_end = checked_add(base, dst_len)?;
        for write in &self.writes {
            let write_len =
                u64::try_from(write.bytes.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
            let write_end = checked_add(write.offset, write_len)?;
            let start = base.max(write.offset);
            let end = dst_end.min(write_end);
            if start >= end {
                continue;
            }
            let dst_start =
                usize::try_from(start - base).map_err(|_| StableMemoryError::OffsetOverflow)?;
            let src_start = usize::try_from(start - write.offset)
                .map_err(|_| StableMemoryError::OffsetOverflow)?;
            let len =
                usize::try_from(end - start).map_err(|_| StableMemoryError::OffsetOverflow)?;
            dst[dst_start..dst_start + len]
                .copy_from_slice(&write.bytes[src_start..src_start + len]);
        }
        Ok(())
    }
}

pub fn begin(base_size: u64) -> Result<(), StableMemoryError> {
    OVERLAY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(StableMemoryError::Failpoint("overlay already active"));
        }
        *slot = Some(Overlay::new(base_size));
        Ok(())
    })
}

pub fn rollback() {
    OVERLAY.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub fn take() -> Option<Overlay> {
    OVERLAY.with(|slot| slot.borrow_mut().take())
}

pub fn read_at(offset: u64, dst: &mut [u8]) -> Option<Result<bool, StableMemoryError>> {
    OVERLAY.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|overlay| overlay.read_at(offset, dst))
    })
}

pub fn write_at(offset: u64, bytes: &[u8]) -> Option<Result<(), StableMemoryError>> {
    OVERLAY.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(|overlay| overlay.write_at(offset, bytes))
    })
}

pub fn truncate(size: u64) -> Option<Result<(), StableMemoryError>> {
    OVERLAY.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(|overlay| overlay.truncate(size))
    })
}

pub fn file_size() -> Option<u64> {
    OVERLAY.with(|slot| slot.borrow().as_ref().map(Overlay::size))
}

fn checked_add(left: u64, right: u64) -> Result<u64, StableMemoryError> {
    left.checked_add(right)
        .ok_or(StableMemoryError::OffsetOverflow)
}

#[cfg(test)]
mod tests {
    use super::Overlay;
    use crate::stable::memory;

    #[test]
    fn later_overlapping_write_wins_regardless_of_offset_order() {
        memory::reset_for_tests();
        let mut overlay = Overlay::new(0);

        overlay.write_at(4, b"AAAA").unwrap();
        overlay.write_at(2, b"bbbbbb").unwrap();

        let mut out = vec![0; 8];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out.as_slice(), b"\0\0bbbbbb");
    }

    #[test]
    fn truncate_then_sparse_extend_zero_fills_gap() {
        memory::reset_for_tests();
        let mut overlay = Overlay::new(0);

        overlay.write_at(0, b"abcd").unwrap();
        overlay.truncate(1).unwrap();
        overlay.write_at(3, b"z").unwrap();

        let mut out = vec![0; 4];
        assert!(overlay.read_at(0, &mut out).unwrap());
        assert_eq!(out.as_slice(), b"a\0\0z");
    }
}
