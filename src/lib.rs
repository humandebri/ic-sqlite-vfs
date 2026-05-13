//! IC canister crate root for the `icstable` SQLite VFS.
//!
//! The database image is stored directly in IC stable memory. SQLite reaches it
//! only through `sqlite3_vfs` callbacks, so no POSIX or WASI filesystem is used.

#[cfg(feature = "canister-api")]
pub mod api;
pub mod config;
pub mod db;
pub mod sqlite_vfs;
pub mod stable;

pub use db::{Db, DbError};

#[cfg(feature = "canister-api")]
use api::{ChecksumRefresh, DbMeta};

#[cfg(feature = "canister-api")]
ic_cdk::export_candid!();
