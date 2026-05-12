//! Required SQLite PRAGMA setup.
//!
//! These settings keep rollback journal and temp storage in heap memory while the
//! database image itself remains the only stable-memory payload.

use crate::config::{BUSY_TIMEOUT_MS, SQLITE_CACHE_SIZE_KIB, SQLITE_PAGE_SIZE};
use crate::db::connection::Connection;
use crate::db::DbError;

pub fn apply_read_write(connection: &Connection) -> Result<(), DbError> {
    connection.execute_batch(&format!(
        "PRAGMA page_size = {SQLITE_PAGE_SIZE};
         PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;
         PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA foreign_keys = ON;
         PRAGMA cache_size = -{SQLITE_CACHE_SIZE_KIB};
         PRAGMA busy_timeout = {BUSY_TIMEOUT_MS};"
    ))?;
    Ok(())
}

pub fn apply_read_only(connection: &Connection) -> Result<(), DbError> {
    connection.execute_batch(&format!(
        "PRAGMA query_only = ON;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA busy_timeout = {BUSY_TIMEOUT_MS};"
    ))?;
    Ok(())
}
