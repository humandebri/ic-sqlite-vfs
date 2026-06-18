//! Canister-local lock state for SQLite.
//!
//! IC message execution is single-threaded. Locks still exist because SQLite's
//! pager expects state transitions while moving from shared to exclusive access.

use std::cell::RefCell;
use std::ffi::c_int;

use crate::stable::memory::ContextId;

thread_local! {
    static LOCK_LEVEL: RefCell<Vec<(ContextId, c_int)>> = const { RefCell::new(Vec::new()) };
}

pub fn lock(level: c_int) {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return;
    };
    lock_for(context, level);
}

pub(crate) fn lock_for(context: ContextId, level: c_int) {
    LOCK_LEVEL.with(|levels| {
        let mut levels = levels.borrow_mut();
        for (stored_context, current) in levels.iter_mut() {
            if *stored_context == context {
                if level > *current {
                    *current = level;
                }
                return;
            }
        }
        levels.push((context, level));
    });
}

pub fn unlock(level: c_int) {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return;
    };
    unlock_for(context, level);
}

pub(crate) fn unlock_for(context: ContextId, level: c_int) {
    LOCK_LEVEL.with(|levels| {
        let mut levels = levels.borrow_mut();
        for (stored_context, current) in levels.iter_mut() {
            if *stored_context == context {
                *current = level;
                return;
            }
        }
        levels.push((context, level));
    });
}

pub fn has_reserved() -> bool {
    level() >= crate::sqlite_vfs::ffi::SQLITE_LOCK_RESERVED
}

pub(crate) fn has_reserved_for(context: ContextId) -> bool {
    level_for(context) >= crate::sqlite_vfs::ffi::SQLITE_LOCK_RESERVED
}

pub fn level() -> c_int {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return 0;
    };
    level_for(context)
}

pub(crate) fn level_for(context: ContextId) -> c_int {
    LOCK_LEVEL.with(|levels| {
        levels
            .borrow()
            .iter()
            .find_map(|(stored_context, level)| {
                if *stored_context == context {
                    Some(*level)
                } else {
                    None
                }
            })
            .unwrap_or(0)
    })
}

#[cfg(any(test, debug_assertions))]
pub fn reset_for_tests() {
    LOCK_LEVEL.with(|levels| levels.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::{has_reserved, lock, reset_for_tests};
    use crate::sqlite_vfs::ffi;
    use crate::stable::memory;
    use crate::stable::memory_manager::{MemoryId, MemoryManager};
    use crate::stable::raw_memory::DefaultMemoryImpl;

    #[test]
    fn lock_state_is_separated_by_context() {
        memory::reset_for_tests();
        reset_for_tests();
        let manager = MemoryManager::init(DefaultMemoryImpl::default());
        let first = memory::init_context(manager.get(MemoryId::new(32))).unwrap();
        let second = memory::init_context(manager.get(MemoryId::new(33))).unwrap();

        memory::with_context(first, || {
            lock(ffi::SQLITE_LOCK_RESERVED);
            assert!(has_reserved());
        });
        memory::with_context(second, || {
            assert!(!has_reserved());
        });
    }
}
