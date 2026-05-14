//! Canister-local lock state for SQLite.
//!
//! IC message execution is single-threaded. Locks still exist because SQLite's
//! pager expects state transitions while moving from shared to exclusive access.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_int;

thread_local! {
    static LOCK_LEVEL: RefCell<BTreeMap<crate::stable::memory::ContextId, c_int>> = const { RefCell::new(BTreeMap::new()) };
}

pub fn lock(level: c_int) {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return;
    };
    LOCK_LEVEL.with(|levels| {
        let mut levels = levels.borrow_mut();
        let current = levels.entry(context).or_insert(0);
        if level > *current {
            *current = level;
        }
    });
}

pub fn unlock(level: c_int) {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return;
    };
    LOCK_LEVEL.with(|levels| {
        levels.borrow_mut().insert(context, level);
    });
}

pub fn has_reserved() -> bool {
    level() >= crate::sqlite_vfs::ffi::SQLITE_LOCK_RESERVED
}

pub fn level() -> c_int {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return 0;
    };
    LOCK_LEVEL.with(|levels| levels.borrow().get(&context).copied().unwrap_or(0))
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
    use ic_stable_structures::{
        memory_manager::{MemoryId, MemoryManager},
        DefaultMemoryImpl,
    };

    #[test]
    fn lock_state_is_separated_by_context() {
        memory::reset_for_tests();
        reset_for_tests();
        let manager = MemoryManager::init(DefaultMemoryImpl::default());
        let first = memory::init_context(manager.get(MemoryId::new(32)));
        let second = memory::init_context(manager.get(MemoryId::new(33)));

        memory::with_context(first, || {
            lock(ffi::SQLITE_LOCK_RESERVED);
            assert!(has_reserved());
        });
        memory::with_context(second, || {
            assert!(!has_reserved());
        });
    }
}
