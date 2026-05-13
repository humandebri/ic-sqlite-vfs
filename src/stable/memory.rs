//! Byte-addressed stable memory wrapper.
//!
//! On wasm32 this calls the IC stable memory API. Native tests use a heap Vec so
//! VFS behavior can be verified without a replica.

use crate::config::STABLE_PAGE_SIZE;
#[cfg(any(
    not(target_arch = "wasm32"),
    test,
    feature = "canister-api-test-failpoints"
))]
use std::cell::RefCell;

#[derive(Debug, thiserror::Error)]
pub enum StableMemoryError {
    #[error(
        "stable memory grow failed: current_pages={current_pages}, required_pages={required_pages}"
    )]
    GrowFailed {
        current_pages: u64,
        required_pages: u64,
    },
    #[error("offset overflow")]
    OffsetOverflow,
    #[error("import session already started")]
    ImportAlreadyStarted,
    #[error("import session not started")]
    ImportNotStarted,
    #[error("import chunk out of order: offset={offset}, expected={expected}")]
    ImportOutOfOrder { offset: u64, expected: u64 },
    #[error("import chunk out of bounds: offset={offset}, len={len}, db_size={db_size}")]
    ImportOutOfBounds { offset: u64, len: u64, db_size: u64 },
    #[error("import incomplete: written_until={written_until}, db_size={db_size}")]
    ImportIncomplete { written_until: u64, db_size: u64 },
    #[error("checksum mismatch: expected={expected}, actual={actual}")]
    ChecksumMismatch { expected: u64, actual: u64 },
    #[error("checksum refresh chunk size must be greater than zero")]
    ChecksumRefreshChunkEmpty,
    #[error("stable blob failpoint: {0}")]
    Failpoint(&'static str),
    #[error("superblock metadata checksum mismatch")]
    MetaChecksumMismatch,
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryFailpoint {
    GrowFailed { ordinal: u64 },
    TrapAfterWrite { ordinal: u64 },
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
thread_local! {
    static FAILPOINT: RefCell<Option<MemoryFailpoint>> = const { RefCell::new(None) };
    static GROW_COUNT: RefCell<u64> = const { RefCell::new(0) };
    static WRITE_COUNT: RefCell<u64> = const { RefCell::new(0) };
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static TEST_MEMORY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn set_failpoint(failpoint: MemoryFailpoint) {
    FAILPOINT.with(|slot| *slot.borrow_mut() = Some(failpoint));
    GROW_COUNT.with(|count| *count.borrow_mut() = 0);
    WRITE_COUNT.with(|count| *count.borrow_mut() = 0);
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn clear_failpoint() {
    FAILPOINT.with(|slot| *slot.borrow_mut() = None);
    GROW_COUNT.with(|count| *count.borrow_mut() = 0);
    WRITE_COUNT.with(|count| *count.borrow_mut() = 0);
}

pub fn size_pages() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        ic_cdk::api::stable_size()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        TEST_MEMORY.with(|memory| {
            let len = u64::try_from(memory.borrow().len()).expect("usize fits in u64");
            len / STABLE_PAGE_SIZE
        })
    }
}

pub fn ensure_capacity(end_offset: u64) -> Result<(), StableMemoryError> {
    let current_bytes = size_pages()
        .checked_mul(STABLE_PAGE_SIZE)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    if end_offset <= current_bytes {
        return Ok(());
    }

    let missing = end_offset
        .checked_sub(current_bytes)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    let pages = missing.div_ceil(STABLE_PAGE_SIZE);

    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if hit_grow_failpoint() {
        return Err(StableMemoryError::GrowFailed {
            current_pages: current_bytes / STABLE_PAGE_SIZE,
            required_pages: current_bytes / STABLE_PAGE_SIZE + pages,
        });
    }

    #[cfg(target_arch = "wasm32")]
    {
        let previous = ic_cdk::api::stable_grow(pages);
        if previous == u64::MAX {
            let required_pages = size_pages()
                .checked_add(pages)
                .ok_or(StableMemoryError::OffsetOverflow)?;
            return Err(StableMemoryError::GrowFailed {
                current_pages: size_pages(),
                required_pages,
            });
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        TEST_MEMORY.with(|memory| {
            let new_len_u64 = current_bytes
                .checked_add(pages * STABLE_PAGE_SIZE)
                .ok_or(StableMemoryError::OffsetOverflow)?;
            let new_len =
                usize::try_from(new_len_u64).map_err(|_| StableMemoryError::GrowFailed {
                    current_pages: current_bytes / STABLE_PAGE_SIZE,
                    required_pages: new_len_u64 / STABLE_PAGE_SIZE,
                })?;
            memory.borrow_mut().resize(new_len, 0);
            Ok::<(), StableMemoryError>(())
        })?;
    }

    Ok(())
}

pub fn read(offset: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
    if dst.is_empty() {
        return Ok(());
    }
    let end = checked_end(offset, dst.len())?;
    ensure_capacity(end)?;

    #[cfg(target_arch = "wasm32")]
    {
        ic_cdk::api::stable_read(offset, dst);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        TEST_MEMORY.with(|memory| {
            let start = usize::try_from(offset).map_err(|_| StableMemoryError::OffsetOverflow)?;
            let end = start
                .checked_add(dst.len())
                .ok_or(StableMemoryError::OffsetOverflow)?;
            dst.copy_from_slice(&memory.borrow()[start..end]);
            Ok::<(), StableMemoryError>(())
        })?;
    }

    Ok(())
}

pub fn write(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let end = checked_end(offset, bytes.len())?;
    ensure_capacity(end)?;

    #[cfg(target_arch = "wasm32")]
    {
        ic_cdk::api::stable_write(offset, bytes);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        TEST_MEMORY.with(|memory| {
            let start = usize::try_from(offset).map_err(|_| StableMemoryError::OffsetOverflow)?;
            let end = start
                .checked_add(bytes.len())
                .ok_or(StableMemoryError::OffsetOverflow)?;
            memory.borrow_mut()[start..end].copy_from_slice(bytes);
            Ok::<(), StableMemoryError>(())
        })?;
    }

    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if hit_write_trap_failpoint() {
        fail_after_stable_write();
    }

    Ok(())
}

#[cfg(any(test, debug_assertions))]
pub fn reset_for_tests() {
    #[cfg(not(target_arch = "wasm32"))]
    TEST_MEMORY.with(|memory| memory.borrow_mut().clear());
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    clear_failpoint();
}

#[cfg(test)]
pub fn snapshot_for_tests() -> Vec<u8> {
    TEST_MEMORY.with(|memory| memory.borrow().clone())
}

#[cfg(test)]
pub fn restore_for_tests(snapshot: Vec<u8>) {
    TEST_MEMORY.with(|memory| *memory.borrow_mut() = snapshot);
}

fn checked_end(offset: u64, len: usize) -> Result<u64, StableMemoryError> {
    let len = u64::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
    offset
        .checked_add(len)
        .ok_or(StableMemoryError::OffsetOverflow)
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_grow_failpoint() -> bool {
    GROW_COUNT.with(|count| {
        let mut count = count.borrow_mut();
        *count += 1;
        let current = *count;
        FAILPOINT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if *slot == Some(MemoryFailpoint::GrowFailed { ordinal: current }) {
                *slot = None;
                true
            } else {
                false
            }
        })
    })
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_write_trap_failpoint() -> bool {
    WRITE_COUNT.with(|count| {
        let mut count = count.borrow_mut();
        *count += 1;
        let current = *count;
        FAILPOINT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if *slot == Some(MemoryFailpoint::TrapAfterWrite { ordinal: current }) {
                *slot = None;
                true
            } else {
                false
            }
        })
    })
}

#[cfg(all(target_arch = "wasm32", feature = "canister-api-test-failpoints"))]
fn fail_after_stable_write() -> ! {
    ic_cdk::trap("stable write failpoint");
}

#[cfg(all(
    any(test, feature = "canister-api-test-failpoints"),
    not(all(target_arch = "wasm32", feature = "canister-api-test-failpoints"))
))]
fn fail_after_stable_write() -> ! {
    panic!("stable write failpoint");
}
