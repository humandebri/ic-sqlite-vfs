//! Heap-backed temporary file used when SQLite asks the VFS for temp storage.
//!
//! The configured PRAGMAs keep journals and temp tables in memory. This file
//! exists only to satisfy defensive SQLite paths without touching stable memory.

#[derive(Debug, Default)]
pub struct TempFile {
    bytes: Vec<u8>,
}

impl TempFile {
    pub fn read(&self, offset: u64, dst: &mut [u8]) -> bool {
        dst.fill(0);
        let start = match usize::try_from(offset) {
            Ok(value) => value,
            Err(_) => return false,
        };
        if start >= self.bytes.len() {
            return false;
        }
        let available = self.bytes.len() - start;
        let copied = available.min(dst.len());
        dst[..copied].copy_from_slice(&self.bytes[start..start + copied]);
        copied == dst.len()
    }

    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> bool {
        let start = match usize::try_from(offset) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let end = match start.checked_add(bytes.len()) {
            Some(value) => value,
            None => return false,
        };
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[start..end].copy_from_slice(bytes);
        true
    }

    pub fn truncate(&mut self, size: u64) -> bool {
        let size = match usize::try_from(size) {
            Ok(value) => value,
            Err(_) => return false,
        };
        self.bytes.resize(size, 0);
        true
    }

    pub fn len(&self) -> u64 {
        u64::try_from(self.bytes.len()).expect("usize fits in u64")
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}
