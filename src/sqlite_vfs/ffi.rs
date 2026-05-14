//! Raw SQLite FFI used by the VFS layer.
//!
//! Bindings are vendored so `sqlite-precompiled` can link without compiling C.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(improper_ctypes)]
#![allow(clippy::all)]

use std::ffi::c_void;

include!(concat!(env!("OUT_DIR"), "/sqlite_bindings.rs"));

pub fn SQLITE_STATIC() -> sqlite3_destructor_type {
    None
}

pub fn SQLITE_TRANSIENT() -> sqlite3_destructor_type {
    Some(unsafe { std::mem::transmute::<isize, unsafe extern "C" fn(*mut c_void)>(-1) })
}
