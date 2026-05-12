//! Minimal schema migration runner.
//!
//! Versions are stored both in SQLite and the stable superblock. The SQLite table
//! is the source of truth for applied SQL; the superblock is quick canister state.

use crate::db::connection::Connection;
use crate::db::DbError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    pub version: u64,
    pub sql: &'static str,
}

pub fn apply(connection: &Connection, migrations: &[Migration]) -> Result<(), DbError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS __ic_sqlite_migrations (
            version INTEGER PRIMARY KEY NOT NULL
        )",
    )?;

    for migration in migrations {
        let version = sqlite_version(migration.version)?;
        let exists = connection.query_i64(&format!(
            "SELECT EXISTS(SELECT 1 FROM __ic_sqlite_migrations WHERE version = {version})"
        ))?;
        if exists != 0 {
            continue;
        }
        connection.execute_batch(migration.sql)?;
        connection.execute_batch(&format!(
            "INSERT INTO __ic_sqlite_migrations(version) VALUES ({version})"
        ))?;
    }

    Ok(())
}

fn sqlite_version(version: u64) -> Result<i64, DbError> {
    i64::try_from(version).map_err(|_| DbError::MigrationVersionOutOfRange(version))
}
