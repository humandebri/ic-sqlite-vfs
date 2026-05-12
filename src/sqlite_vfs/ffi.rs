//! Raw SQLite FFI re-exports used by the VFS layer.
//!
//! Keeping the alias in one place makes callback signatures easier to audit
//! against SQLite's `sqlite3_vfs` and `sqlite3_io_methods` documentation.

pub use libsqlite3_sys::*;
