//! Minimal schema migration runner.
//!
//! Versions are stored both in SQLite and the stable superblock. The SQLite table
//! is the source of truth for applied SQL; the superblock is quick canister state.

use crate::db::connection::Connection;
use crate::db::DbError;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    pub version: u64,
    pub sql: &'static str,
}

pub fn apply(connection: &Connection, migrations: &[Migration]) -> Result<(), DbError> {
    validate_versions(migrations)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS __ic_sqlite_migrations (
            version INTEGER PRIMARY KEY NOT NULL
        )",
    )?;

    for migration in migrations {
        let version = sqlite_version(migration.version)?;
        let exists = connection.query_scalar::<i64>(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM __ic_sqlite_migrations WHERE version = {version})"
            ),
            crate::params![],
        )?;
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

fn validate_versions(migrations: &[Migration]) -> Result<(), DbError> {
    let mut seen = BTreeSet::new();
    let mut previous = None;
    for migration in migrations {
        if !seen.insert(migration.version) {
            return Err(DbError::DuplicateMigrationVersion(migration.version));
        }
        if let Some(previous) = previous {
            if migration.version < previous {
                return Err(DbError::MigrationVersionOutOfOrder {
                    previous,
                    next: migration.version,
                });
            }
        }
        previous = Some(migration.version);
    }
    Ok(())
}

fn sqlite_version(version: u64) -> Result<i64, DbError> {
    i64::try_from(version).map_err(|_| DbError::MigrationVersionOutOfRange(version))
}
