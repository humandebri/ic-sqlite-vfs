//! Byte-addressed SQLite memory wrapper.
//!
//! The crate stores SQLite inside a user-provided `VirtualMemory`, so it can
//! coexist with other stable structures managed by the same MemoryManager.

use crate::config::STABLE_PAGE_SIZE;
use crate::stable::memory_manager::{MemoryIdentity, VirtualMemory};
use crate::stable::raw_memory::{DefaultMemoryImpl, Memory};
use std::cell::{Cell, RefCell};
#[cfg(any(test, feature = "canister-api-test-failpoints"))]
use std::collections::BTreeMap;

pub type DbMemory = VirtualMemory<DefaultMemoryImpl>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ContextId(u64);

#[derive(Debug, thiserror::Error)]
pub enum StableMemoryError {
    #[error("stable memory backend is not initialized")]
    NotInitialized,
    #[error("stable memory backend is already initialized")]
    AlreadyInitialized,
    #[error("stable memory is already registered with a database handle")]
    MemoryAlreadyRegistered,
    #[error(
        "stable memory grow failed: current_pages={current_pages}, required_pages={required_pages}"
    )]
    GrowFailed {
        current_pages: u64,
        required_pages: u64,
    },
    #[error(
        "stable memory read out of bounds: offset={offset}, len={len}, size_bytes={size_bytes}"
    )]
    ReadOutOfBounds {
        offset: u64,
        len: u64,
        size_bytes: u64,
    },
    #[error("offset overflow")]
    OffsetOverflow,
    #[error("import session already started")]
    ImportAlreadyStarted,
    #[error("import session not started")]
    ImportNotStarted,
    #[error("database update already in progress")]
    UpdateInProgress,
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
    #[error("unsupported stable layout version: {0}")]
    UnsupportedLayoutVersion(u64),
    #[error("non-empty stable memory does not contain an ic-sqlite-vfs image")]
    ForeignStableMemoryImage,
    #[error("zero extent limit exceeded: limit={limit}")]
    ZeroExtentLimitExceeded { limit: usize },
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryFailpoint {
    GrowFailed { ordinal: u64 },
    TrapAfterWrite { ordinal: u64 },
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
thread_local! {
    static FAILPOINTS: RefCell<BTreeMap<ContextId, MemoryFailpointState>> = const { RefCell::new(BTreeMap::new()) };
}

thread_local! {
    static NEXT_CONTEXT_ID: Cell<u64> = const { Cell::new(1) };
    static DEFAULT_CONTEXT: Cell<Option<ContextId>> = const { Cell::new(None) };
    static CURRENT_CONTEXT: Cell<Option<ContextId>> = const { Cell::new(None) };
    static CACHE_GENERATION: Cell<u64> = const { Cell::new(0) };
    static DB_MEMORY: RefCell<Vec<ContextState>> = const { RefCell::new(Vec::new()) };
    static REGISTERED_MEMORY: RefCell<Vec<(ContextId, MemoryIdentity)>> = const { RefCell::new(Vec::new()) };
}

struct ContextState {
    context: ContextId,
    memory: DbMemory,
    last_error: Option<ContextLastError>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextLastError {
    pub(crate) errno: i32,
    pub(crate) message: String,
}

pub fn init(memory: DbMemory) -> Result<ContextId, StableMemoryError> {
    DEFAULT_CONTEXT.with(|default| {
        if default.get().is_some() {
            return Err(StableMemoryError::AlreadyInitialized);
        }
        let context = init_context(memory)?;
        default.set(Some(context));
        Ok(context)
    })
}

pub fn init_context(memory: DbMemory) -> Result<ContextId, StableMemoryError> {
    let identity = memory.identity();
    reject_registered_memory(identity)?;
    let context = NEXT_CONTEXT_ID.with(|next| {
        let current = next.get();
        let context = ContextId(current);
        next.set(current.checked_add(1).expect("context id overflow"));
        context
    });
    DB_MEMORY.with(|slot| {
        slot.borrow_mut().push(ContextState {
            context,
            memory,
            last_error: None,
        });
    });
    REGISTERED_MEMORY.with(|slot| {
        slot.borrow_mut().push((context, identity));
    });
    Ok(context)
}

fn reject_registered_memory(identity: MemoryIdentity) -> Result<(), StableMemoryError> {
    REGISTERED_MEMORY.with(|slot| {
        if slot
            .borrow()
            .iter()
            .any(|(_, registered)| *registered == identity)
        {
            Err(StableMemoryError::MemoryAlreadyRegistered)
        } else {
            Ok(())
        }
    })
}

pub fn default_context() -> Option<ContextId> {
    DEFAULT_CONTEXT.with(Cell::get)
}

pub(crate) fn cache_generation() -> u64 {
    CACHE_GENERATION.with(Cell::get)
}

#[inline(always)]
pub fn active_context_id() -> Result<ContextId, StableMemoryError> {
    if let Some(context) = CURRENT_CONTEXT.with(Cell::get) {
        return Ok(context);
    }
    default_context().ok_or(StableMemoryError::NotInitialized)
}

#[inline(always)]
pub fn with_context<T>(context: ContextId, f: impl FnOnce() -> T) -> T {
    let previous = CURRENT_CONTEXT.with(|current| {
        let previous = current.get();
        current.set(Some(context));
        previous
    });
    let _guard = ContextGuard { previous };
    f()
}

pub(crate) fn record_last_error(context: ContextId, errno: i32, message: String) {
    DB_MEMORY.with(|slot| {
        if let Some(state) = slot
            .borrow_mut()
            .iter_mut()
            .find(|state| state.context == context)
        {
            state.last_error = Some(ContextLastError { errno, message });
        }
    });
}

pub(crate) fn last_error(context: ContextId) -> Option<ContextLastError> {
    DB_MEMORY.with(|slot| {
        slot.borrow()
            .iter()
            .find(|state| state.context == context)
            .and_then(|state| state.last_error.clone())
    })
}

pub(crate) fn last_errno(context: ContextId) -> i32 {
    last_error(context).map_or(0, |error| error.errno)
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn set_failpoint(failpoint: MemoryFailpoint) {
    if let Ok(context) = active_context_id() {
        FAILPOINTS.with(|slot| {
            slot.borrow_mut().insert(
                context,
                MemoryFailpointState {
                    failpoint: Some(failpoint),
                    grow_count: 0,
                    write_count: 0,
                },
            );
        });
    }
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn clear_failpoint() {
    FAILPOINTS.with(|slot| slot.borrow_mut().clear());
}

pub fn size_pages() -> u64 {
    with_memory(|memory| memory.size()).unwrap_or(0)
}

pub fn ensure_capacity(end_offset: u64) -> Result<(), StableMemoryError> {
    with_memory(|memory| ensure_memory_capacity(memory, end_offset))?
}

#[cfg(any(test, debug_assertions))]
pub fn read(offset: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
    if dst.is_empty() {
        return Ok(());
    }
    let len = u64::try_from(dst.len()).map_err(|_| StableMemoryError::OffsetOverflow)?;
    let end = offset
        .checked_add(len)
        .ok_or(StableMemoryError::OffsetOverflow)?;
    with_memory(|memory| {
        let size_bytes = memory
            .size()
            .checked_mul(STABLE_PAGE_SIZE)
            .ok_or(StableMemoryError::OffsetOverflow)?;
        if end > size_bytes {
            return Err(StableMemoryError::ReadOutOfBounds {
                offset,
                len,
                size_bytes,
            });
        }
        memory.read(offset, dst);
        Ok(())
    })?
}

#[inline(always)]
pub(crate) fn read_preallocated(offset: u64, dst: &mut [u8]) -> Result<(), StableMemoryError> {
    checked_end(offset, dst.len())?;
    with_memory(|memory| {
        debug_assert_capacity(memory, offset, dst.len(), "read_preallocated");
        memory.read(offset, dst);
    })?;
    Ok(())
}

pub fn write(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let end = checked_end(offset, bytes.len())?;
    with_memory(|memory| {
        ensure_memory_capacity(memory, end)?;
        memory.write(offset, bytes);
        Ok(())
    })??;

    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    crate::read_metrics::record_stable_data_write(bytes.len());

    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if hit_write_trap_failpoint() {
        fail_after_stable_write();
    }

    Ok(())
}

#[inline(always)]
pub(crate) fn write_prechecked(offset: u64, bytes: &[u8]) -> Result<(), StableMemoryError> {
    debug_assert!(checked_end(offset, bytes.len()).is_ok());
    with_memory(|memory| {
        debug_assert_capacity(memory, offset, bytes.len(), "write_prechecked");
        memory.write(offset, bytes);
    })?;

    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    crate::read_metrics::record_stable_data_write(bytes.len());

    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if hit_write_trap_failpoint() {
        fail_after_stable_write();
    }

    Ok(())
}

#[inline(always)]
pub(crate) fn write_prechecked_unmetered(
    offset: u64,
    bytes: &[u8],
) -> Result<(), StableMemoryError> {
    debug_assert!(checked_end(offset, bytes.len()).is_ok());
    with_memory(|memory| {
        debug_assert_capacity(memory, offset, bytes.len(), "write_prechecked_unmetered");
        memory.write(offset, bytes);
    })?;

    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if hit_write_trap_failpoint() {
        fail_after_stable_write();
    }

    Ok(())
}

fn ensure_memory_capacity(memory: &DbMemory, end_offset: u64) -> Result<(), StableMemoryError> {
    let current_pages = memory.size();
    let current_bytes = current_pages
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
        let required_pages = current_pages
            .checked_add(pages)
            .ok_or(StableMemoryError::OffsetOverflow)?;
        return Err(StableMemoryError::GrowFailed {
            current_pages,
            required_pages,
        });
    }

    let previous = memory.grow(pages);
    if previous < 0 {
        let required_pages = current_pages
            .checked_add(pages)
            .ok_or(StableMemoryError::OffsetOverflow)?;
        return Err(StableMemoryError::GrowFailed {
            current_pages,
            required_pages,
        });
    }

    #[cfg(any(test, debug_assertions, feature = "bench-profile"))]
    crate::read_metrics::record_stable_grow(pages);
    Ok(())
}

#[inline(always)]
fn debug_assert_capacity(memory: &DbMemory, offset: u64, len: usize, operation: &str) {
    #[cfg(debug_assertions)]
    {
        let Ok(end) = checked_end(offset, len) else {
            debug_assert!(false, "{operation} offset overflow");
            return;
        };
        let Some(capacity) = memory.size().checked_mul(STABLE_PAGE_SIZE) else {
            debug_assert!(false, "{operation} capacity overflow");
            return;
        };
        debug_assert!(
            end <= capacity,
            "{operation} requires preallocated capacity: offset={offset}, len={len}, capacity={capacity}"
        );
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (memory, offset, len, operation);
    }
}

#[cfg(any(test, debug_assertions))]
pub fn reset_for_tests() {
    clear_initialization();
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    clear_failpoint();
}

#[cfg(any(test, debug_assertions))]
pub fn set_next_context_id_for_tests(value: u64) {
    NEXT_CONTEXT_ID.with(|next| next.set(value));
}

#[cfg(any(test, debug_assertions))]
pub(crate) fn clear_initialization() {
    DB_MEMORY.with(|memory| memory.borrow_mut().clear());
    REGISTERED_MEMORY.with(|memory| memory.borrow_mut().clear());
    DEFAULT_CONTEXT.with(|context| context.set(None));
    CURRENT_CONTEXT.with(|context| context.set(None));
    NEXT_CONTEXT_ID.with(|next| next.set(1));
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    clear_failpoint();
    advance_cache_generation();
}

pub(crate) fn clear_failed_initialization(context: ContextId) {
    DB_MEMORY.with(|memory| {
        memory.borrow_mut().retain(|state| state.context != context);
    });
    REGISTERED_MEMORY.with(|memory| {
        memory
            .borrow_mut()
            .retain(|(stored_context, _)| *stored_context != context);
    });
    DEFAULT_CONTEXT.with(|default| {
        if default.get() == Some(context) {
            default.set(None);
        }
    });
    CURRENT_CONTEXT.with(|current| {
        if current.get() == Some(context) {
            current.set(None);
        }
    });
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    FAILPOINTS.with(|slot| {
        slot.borrow_mut().remove(&context);
    });
    advance_cache_generation();
}

#[cfg(any(test, debug_assertions))]
pub fn snapshot_for_tests() -> Vec<u8> {
    let len = usize::try_from(size_pages().saturating_mul(STABLE_PAGE_SIZE))
        .expect("test memory size fits usize");
    let mut out = vec![0_u8; len];
    read(0, &mut out).expect("test memory snapshot succeeds");
    out
}

#[cfg(any(test, debug_assertions))]
pub fn restore_for_tests(snapshot: Vec<u8>) -> DbMemory {
    reset_for_tests();
    let memory = memory_for_tests();
    let pages = u64::try_from(snapshot.len())
        .expect("snapshot len fits u64")
        .div_ceil(STABLE_PAGE_SIZE);
    if pages > 0 {
        assert!(memory.grow(pages) >= 0, "snapshot memory grows");
        memory.write(0, &snapshot);
    }
    memory
}

#[cfg(any(test, debug_assertions))]
pub fn memory_for_tests() -> DbMemory {
    use crate::stable::memory_manager::{MemoryId, MemoryManager};

    MemoryManager::init(DefaultMemoryImpl::default()).get(MemoryId::new(42))
}

#[inline(always)]
fn with_memory<T>(f: impl FnOnce(&DbMemory) -> T) -> Result<T, StableMemoryError> {
    let context = active_context_id()?;
    DB_MEMORY.with(|slot| {
        let slot = slot.borrow();
        if let Some(state) = slot.first() {
            if state.context == context {
                return Ok(f(&state.memory));
            }
        }
        for state in slot.iter().skip(1) {
            if state.context == context {
                return Ok(f(&state.memory));
            }
        }
        Err(StableMemoryError::NotInitialized)
    })
}

fn advance_cache_generation() {
    CACHE_GENERATION.with(|generation| {
        generation.set(generation.get().wrapping_add(1));
    });
}

struct ContextGuard {
    previous: Option<ContextId>,
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT_CONTEXT.with(|current| current.set(self.previous));
    }
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug)]
struct MemoryFailpointState {
    failpoint: Option<MemoryFailpoint>,
    grow_count: u64,
    write_count: u64,
}

fn checked_end(offset: u64, len: usize) -> Result<u64, StableMemoryError> {
    let len = u64::try_from(len).map_err(|_| StableMemoryError::OffsetOverflow)?;
    offset
        .checked_add(len)
        .ok_or(StableMemoryError::OffsetOverflow)
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_grow_failpoint() -> bool {
    let Ok(context) = active_context_id() else {
        return false;
    };
    FAILPOINTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.get_mut(&context) else {
            return false;
        };
        state.grow_count += 1;
        if state.failpoint
            == Some(MemoryFailpoint::GrowFailed {
                ordinal: state.grow_count,
            })
        {
            state.failpoint = None;
            true
        } else {
            false
        }
    })
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_write_trap_failpoint() -> bool {
    let Ok(context) = active_context_id() else {
        return false;
    };
    FAILPOINTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.get_mut(&context) else {
            return false;
        };
        state.write_count += 1;
        if state.failpoint
            == Some(MemoryFailpoint::TrapAfterWrite {
                ordinal: state.write_count,
            })
        {
            state.failpoint = None;
            true
        } else {
            false
        }
    })
}

#[cfg(all(target_arch = "wasm32", feature = "canister-api-test-failpoints"))]
fn fail_after_stable_write() -> ! {
    crate::ic0_shim::trap("stable write failpoint");
}

#[cfg(all(
    any(test, feature = "canister-api-test-failpoints"),
    not(all(target_arch = "wasm32", feature = "canister-api-test-failpoints"))
))]
fn fail_after_stable_write() -> ! {
    panic!("stable write failpoint");
}
