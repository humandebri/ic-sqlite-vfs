//! SQLite VFS implementation for IC stable memory.
//!
//! `register` exposes `sqlite3_os_init`, while `vfs` and `file` hold callback
//! bodies for `sqlite3_vfs` and `sqlite3_io_methods`.

#[cfg(test)]
mod failpoint_tests;
pub mod ffi;
pub mod file;
#[cfg(target_arch = "wasm32")]
pub mod libc;
pub mod lock;
mod overlay;
pub mod register;
pub mod stable_blob;
pub mod temp;
pub mod vfs;

pub use register::register;
