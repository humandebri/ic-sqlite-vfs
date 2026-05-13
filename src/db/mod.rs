//! Public SQLite database facade for canister methods.
//!
//! Update paths accept only synchronous closures, which prevents holding a DB
//! transaction across `await`. Query paths open SQLite in read-only/query-only mode.

pub mod connection;
pub mod migrate;
pub mod pragmas;
pub mod row;
pub mod statement;
pub mod transaction;
pub mod value;

use crate::sqlite_vfs::stable_blob;
use crate::stable::meta::Superblock;
use connection::Connection;
pub use row::{FromColumn, Row};
pub use stable_blob::ChecksumRefresh;
use std::ffi::c_int;
pub use transaction::UpdateConnection;
pub use value::{Null, ToSql, Value, NULL};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite error {0}: {1}")]
    Sqlite(c_int, String),
    #[error("sqlite constraint failed: {0}")]
    Constraint(String),
    #[error("query returned no rows")]
    NotFound,
    #[error("column {index} has type {actual}, expected {expected}")]
    TypeMismatch {
        index: usize,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("column index {index} out of range for {count} columns")]
    ColumnOutOfRange { index: usize, count: usize },
    #[error("stable memory error: {0}")]
    Stable(#[from] crate::stable::memory::StableMemoryError),
    #[error("migration version exceeds SQLite INTEGER range: {0}")]
    MigrationVersionOutOfRange(u64),
    #[error("SQL contains an interior NUL byte")]
    InteriorNul,
    #[error("text value too large")]
    TextTooLarge,
    #[error("blob value too large")]
    BlobTooLarge,
    #[error("too many SQL parameters")]
    TooManyParameters,
    #[error("SQL parameter not found: {0}")]
    ParameterNotFound(String),
}

pub struct Db;

impl Db {
    pub fn init() -> Result<(), DbError> {
        crate::sqlite_vfs::register();
        Superblock::load()?;
        Ok(())
    }

    pub fn update<T, F>(f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>,
    {
        Self::init()?;
        stable_blob::begin_update()?;
        let _overlay_guard = OverlayGuard;
        let connection = connection::open_read_write()?;
        transaction::run_immediate(&connection, f)
    }

    pub fn query<T, F>(f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Connection) -> Result<T, DbError>,
    {
        Self::init()?;
        let connection = connection::open_read_only()?;
        f(&connection)
    }

    pub fn migrate(migrations: &[migrate::Migration]) -> Result<(), DbError> {
        Self::update(|connection| migrate::apply(connection, migrations))?;
        let target_version = migrations
            .iter()
            .map(|migration| migration.version)
            .max()
            .unwrap_or(0);
        let mut block = Superblock::load()?;
        if block.schema_version < target_version {
            block.schema_version = target_version;
            block.store()?;
        }
        Ok(())
    }

    pub fn integrity_check() -> Result<String, DbError> {
        Self::query(|connection| connection.query_string("PRAGMA integrity_check"))
    }

    pub fn export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, DbError> {
        Self::init()?;
        stable_blob::export_chunk(offset, len).map_err(DbError::from)
    }

    pub fn db_checksum() -> Result<u64, DbError> {
        Self::init()?;
        stable_blob::checksum().map_err(DbError::from)
    }

    pub fn refresh_checksum() -> Result<u64, DbError> {
        Self::init()?;
        stable_blob::refresh_checksum().map_err(DbError::from)
    }

    pub fn refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, DbError> {
        Self::init()?;
        stable_blob::refresh_checksum_chunk(max_bytes).map_err(DbError::from)
    }

    pub fn begin_import(total_size: u64, expected_checksum: u64) -> Result<(), DbError> {
        Self::init()?;
        stable_blob::begin_import(total_size, expected_checksum).map_err(DbError::from)
    }

    pub fn import_chunk(offset: u64, bytes: &[u8]) -> Result<(), DbError> {
        Self::init()?;
        stable_blob::import_chunk(offset, bytes).map_err(DbError::from)
    }

    pub fn finish_import() -> Result<(), DbError> {
        Self::init()?;
        stable_blob::finish_import().map_err(DbError::from)
    }
}

struct OverlayGuard;

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        stable_blob::rollback_update();
    }
}
