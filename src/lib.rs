//! IC canister crate root for the `icstable` SQLite VFS.
//!
//! The database image is stored inside a MemoryManager-compatible virtual
//! memory. SQLite reaches it only through `sqlite3_vfs` callbacks, so no POSIX
//! or WASI filesystem is used.

#[cfg(all(feature = "sqlite-bundled", feature = "sqlite-precompiled"))]
compile_error!("features `sqlite-bundled` and `sqlite-precompiled` cannot be enabled together");

#[cfg(feature = "canister-api")]
pub mod api;
#[cfg(feature = "bench-profile")]
#[doc(hidden)]
pub mod bench_support;
pub mod config;
pub mod db;
#[cfg(any(test, debug_assertions, feature = "bench-profile"))]
#[doc(hidden)]
mod read_metrics;
#[doc(hidden)]
mod sqlite_vfs;
#[doc(hidden)]
mod stable;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod test_support;

pub use db::{Db, DbError, DbHandle};
pub use stable::memory::{DbMemory, StableMemoryError};
pub use stable::memory_manager::{MemoryId, MemoryManager, MemoryManagerInitError};
pub use stable::raw_memory::{DefaultMemoryImpl, Memory, MemoryIdentity};

#[macro_export]
macro_rules! params {
    () => {
        &[]
    };
    ($($value:expr),+ $(,)?) => {
        &[$($crate::db::value::to_sql_ref(&$value)),+]
    };
}

#[macro_export]
macro_rules! named_params {
    () => {
        &[]
    };
    ($($name:expr => $value:expr),+ $(,)?) => {
        &[$(($name, $crate::db::value::to_sql_ref(&$value))),+]
    };
}

#[cfg(feature = "canister-api")]
use api::{ChecksumRefresh, DbMeta};

#[cfg(feature = "canister-api")]
ic_cdk::export_candid!();
