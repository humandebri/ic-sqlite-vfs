//! Canister-local lock state for SQLite.
//!
//! IC message execution is single-threaded. Locks still exist because SQLite's
//! pager expects state transitions while moving from shared to exclusive access.

use std::cell::Cell;
use std::ffi::c_int;

thread_local! {
    static LOCK_LEVEL: Cell<c_int> = const { Cell::new(0) };
}

pub fn lock(level: c_int) {
    LOCK_LEVEL.with(|state| {
        if level > state.get() {
            state.set(level);
        }
    });
}

pub fn unlock(level: c_int) {
    LOCK_LEVEL.with(|state| state.set(level));
}

pub fn has_reserved() -> bool {
    LOCK_LEVEL.with(|state| state.get() >= libsqlite3_sys::SQLITE_LOCK_RESERVED)
}

pub fn reset_for_tests() {
    LOCK_LEVEL.with(|state| state.set(0));
}
