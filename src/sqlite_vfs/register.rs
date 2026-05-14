//! SQLite OS initialization hook.
//!
//! `SQLITE_OS_OTHER=1` makes SQLite call these symbols instead of compiling a
//! Unix, Windows, or WASI backend. Registration is idempotent from SQLite's side.

use crate::sqlite_vfs::ffi;
use crate::sqlite_vfs::vfs;
use std::ffi::c_int;
#[cfg(not(target_arch = "wasm32"))]
use std::ptr::NonNull;

pub fn register() -> c_int {
    unsafe { sqlite3_os_init() }
}

#[no_mangle]
/// # Safety
///
/// SQLite calls this during OS initialization. It registers a process-global VFS
/// pointer whose fields are initialized once before registration.
pub unsafe extern "C" fn sqlite3_os_init() -> c_int {
    let vfs = vfs::prepare();
    ffi::sqlite3_vfs_register(vfs, 1)
}

#[no_mangle]
/// # Safety
///
/// SQLite calls this during OS shutdown. The VFS owns no shutdown-time resource.
pub unsafe extern "C" fn sqlite3_os_end() -> c_int {
    ffi::SQLITE_OK
}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_alloc(_kind: c_int) -> *mut ffi::sqlite3_mutex {
    NonNull::<u8>::dangling()
        .as_ptr()
        .cast::<ffi::sqlite3_mutex>()
}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_free(_mutex: *mut ffi::sqlite3_mutex) {}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_enter(_mutex: *mut ffi::sqlite3_mutex) {}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_try(_mutex: *mut ffi::sqlite3_mutex) -> c_int {
    ffi::SQLITE_OK
}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_leave(_mutex: *mut ffi::sqlite3_mutex) {}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_held(_mutex: *mut ffi::sqlite3_mutex) -> c_int {
    1
}

#[no_mangle]
#[cfg(not(target_arch = "wasm32"))]
pub extern "C" fn sqlite3_mutex_notheld(_mutex: *mut ffi::sqlite3_mutex) -> c_int {
    1
}
