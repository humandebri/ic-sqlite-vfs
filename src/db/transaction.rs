//! Synchronous transaction wrapper for update canister methods.
//!
//! The closure cannot be async, so SQLite state cannot be held across an
//! inter-canister call or any other `await` point.

use crate::db::connection::Connection;
use crate::db::DbError;
use crate::sqlite_vfs::stable_blob;
use std::ops::Deref;

pub struct UpdateConnection<'connection> {
    connection: &'connection Connection,
    savepoint_id: u64,
}

impl<'connection> UpdateConnection<'connection> {
    fn new(connection: &'connection Connection) -> Self {
        Self {
            connection,
            savepoint_id: 0,
        }
    }

    pub fn savepoint<T, F>(&mut self, f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut UpdateConnection<'connection>) -> Result<T, DbError>,
    {
        let name = self.next_savepoint_name();
        self.connection
            .execute_batch(&format!("SAVEPOINT {name}"))?;
        match f(self) {
            Ok(value) => {
                self.connection
                    .execute_batch(&format!("RELEASE SAVEPOINT {name}"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self
                    .connection
                    .execute_batch(&format!("ROLLBACK TRANSACTION TO SAVEPOINT {name}"));
                let _ = self
                    .connection
                    .execute_batch(&format!("RELEASE SAVEPOINT {name}"));
                Err(error)
            }
        }
    }

    fn next_savepoint_name(&mut self) -> String {
        let id = self.savepoint_id;
        self.savepoint_id += 1;
        format!("__ic_sqlite_sp_{id}")
    }
}

impl Deref for UpdateConnection<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
    }
}

pub fn run_immediate<T, F>(connection: &Connection, f: F) -> Result<T, DbError>
where
    F: FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>,
{
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let mut update_connection = UpdateConnection::new(connection);
    match f(&mut update_connection) {
        Ok(value) => {
            if let Err(error) = connection.execute_batch("COMMIT") {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error);
            }
            stable_blob::commit_update()?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            stable_blob::rollback_update();
            Err(error)
        }
    }
}
