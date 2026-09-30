//! Minimal schema migration runner.
//!
//! Versions are stored both in SQLite and the stable superblock. The SQLite table
//! is the source of truth for applied SQL; the superblock is quick canister state.

use crate::db::connection::Connection;
use crate::db::DbError;
use crate::sqlite_vfs::ffi;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    pub version: u64,
    pub sql: &'static str,
}

/// Apply new migrations and verify known SHA-256 checksums before any SQL runs.
/// Version-only legacy entries remain unverified until explicitly adopted.
/// The caller must provide a transaction; prefer `Db`/`DbHandle::migrate`.
pub fn apply(connection: &Connection, migrations: &[Migration]) -> Result<(), DbError> {
    validate_versions(migrations)?;
    ensure_history(connection)?;
    // Check all applied entries first, including entries later in the list.
    // A changed later migration must not allow earlier pending SQL to run.
    for migration in migrations {
        verify_checksum(connection, migration)?;
    }
    for migration in migrations {
        let version = sqlite_version(migration.version)?;
        if is_applied(connection, version)? {
            continue;
        }
        let checksum = checksum(migration.sql);
        connection.execute_batch(migration.sql)?;
        connection.execute(
            "INSERT INTO __ic_sqlite_migrations(version) VALUES (?1)",
            crate::params![version],
        )?;
        record_checksum(connection, version, &checksum)?;
    }
    Ok(())
}

/// Explicitly trust the supplied SQL for already-applied legacy versions.
/// This never executes migration SQL or replaces a known checksum. It records
/// an operator assertion, not evidence of which SQL ran historically.
/// The caller must provide a transaction; prefer the facade's adoption API.
pub fn adopt_checksums(connection: &Connection, migrations: &[Migration]) -> Result<(), DbError> {
    validate_versions(migrations)?;
    ensure_history(connection)?;
    for migration in migrations {
        let version = sqlite_version(migration.version)?;
        if !is_applied(connection, version)? {
            return Err(history_error(format!(
                "cannot adopt checksum for unapplied migration version {}",
                migration.version
            )));
        }
        verify_checksum(connection, migration)?;
    }
    for migration in migrations {
        let version = sqlite_version(migration.version)?;
        if stored_checksum(connection, version)?.is_none() {
            record_checksum(connection, version, &checksum(migration.sql))?;
        }
    }
    Ok(())
}

fn ensure_history(connection: &Connection) -> Result<(), DbError> {
    // A side table preserves the published version-only table schema and lets
    // old runners keep inserting versions without inventing checksums.
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS __ic_sqlite_migrations (
            version INTEGER PRIMARY KEY NOT NULL
        );
        CREATE TABLE IF NOT EXISTS __ic_sqlite_migration_checksums (
            version INTEGER PRIMARY KEY NOT NULL,
            checksum BLOB NOT NULL CHECK(typeof(checksum) = 'blob' AND length(checksum) = 32)
        )",
    )
}

fn is_applied(connection: &Connection, version: i64) -> Result<bool, DbError> {
    connection
        .query_scalar::<i64>(
            "SELECT EXISTS(SELECT 1 FROM __ic_sqlite_migrations WHERE version = ?1)",
            crate::params![version],
        )
        .map(|value| value != 0)
}

fn stored_checksum(connection: &Connection, version: i64) -> Result<Option<Vec<u8>>, DbError> {
    connection.query_optional(
        "SELECT checksum FROM __ic_sqlite_migration_checksums WHERE version = ?1",
        crate::params![version],
        |row| row.get(0),
    )
}

fn verify_checksum(connection: &Connection, migration: &Migration) -> Result<(), DbError> {
    if let Some(stored) = stored_checksum(connection, sqlite_version(migration.version)?)? {
        if stored.as_slice() != checksum(migration.sql) {
            return Err(history_error(format!(
                "migration checksum mismatch for version {}",
                migration.version
            )));
        }
    }
    Ok(())
}

fn record_checksum(
    connection: &Connection,
    version: i64,
    checksum: &[u8; 32],
) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO __ic_sqlite_migration_checksums(version, checksum) VALUES (?1, ?2)",
        crate::params![version, checksum.as_slice()],
    )
}

fn checksum(sql: &str) -> [u8; 32] {
    Sha256::digest(sql.as_bytes()).into()
}

fn history_error(message: String) -> DbError {
    // Do not extend the published exhaustive DbError enum for this feature.
    DbError::Sqlite(ffi::SQLITE_CONSTRAINT, message)
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
        sqlite_version(migration.version)?;
        previous = Some(migration.version);
    }
    Ok(())
}

fn sqlite_version(version: u64) -> Result<i64, DbError> {
    i64::try_from(version).map_err(|_| DbError::MigrationVersionOutOfRange(version))
}

#[cfg(test)]
mod tests {
    #[test]
    fn checksum_is_sha256_of_exact_utf8_bytes() {
        assert_eq!(
            super::checksum("abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        assert_ne!(super::checksum("SELECT 1"), super::checksum("SELECT 1 "));
    }
}
