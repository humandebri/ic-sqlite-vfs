//! IC canister crate root for the `icstable` SQLite VFS.
//!
//! The database image is stored directly in IC stable memory. SQLite reaches it
//! only through `sqlite3_vfs` callbacks, so no POSIX or WASI filesystem is used.

#[cfg(feature = "canister-api")]
pub mod api;
pub mod config;
pub mod db;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod read_metrics;
#[doc(hidden)]
pub mod sqlite_vfs;
#[doc(hidden)]
pub mod stable;

pub use db::{Db, DbError};

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
