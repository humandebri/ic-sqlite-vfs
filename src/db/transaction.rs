//! Synchronous transaction wrapper for update canister methods.
//!
//! The closure cannot be async, so SQLite state cannot be held across an
//! inter-canister call or any other `await` point.

use crate::db::connection::Connection;
use crate::db::DbError;
use crate::sqlite_vfs::stable_blob;
use crate::stable::meta::Superblock;

pub fn run_immediate<T, F>(connection: &Connection, f: F) -> Result<T, DbError>
where
    F: FnOnce(&Connection) -> Result<T, DbError>,
{
    connection.execute_batch("BEGIN IMMEDIATE")?;
    match f(connection) {
        Ok(value) => {
            connection.execute_batch("COMMIT")?;
            stable_blob::refresh_checksum()?;
            Superblock::bump_tx()?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}
