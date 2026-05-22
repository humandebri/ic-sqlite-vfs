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
use crate::stable::memory::{self, ContextId, DbMemory};
use crate::stable::meta::Superblock;
use connection::Connection;
pub use row::{FromColumn, Row, TextLen};
pub use stable_blob::ChecksumRefresh;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_int;
use std::rc::Rc;
pub use transaction::UpdateConnection;
pub use value::{Null, ToSql, Value, NULL};

thread_local! {
    static READ_CONNECTIONS: RefCell<BTreeMap<ContextId, Rc<Connection>>> = const { RefCell::new(BTreeMap::new()) };
    static WRITE_CONNECTIONS: RefCell<BTreeMap<ContextId, Rc<Connection>>> = const { RefCell::new(BTreeMap::new()) };
    static ACTIVE_READ_CONNECTIONS: RefCell<Vec<(ContextId, usize)>> = const { RefCell::new(Vec::new()) };
}

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
    #[error("stable memory backend is not initialized; call Db::init(memory) first")]
    StableMemoryNotInitialized,
    #[error("stable memory backend is already initialized")]
    StableMemoryAlreadyInitialized,
    #[error("cannot mutate database while a query connection is active")]
    ReadConnectionInUse,
    #[error("migration version exceeds SQLite INTEGER range: {0}")]
    MigrationVersionOutOfRange(u64),
    #[error("duplicate migration version: {0}")]
    DuplicateMigrationVersion(u64),
    #[error("SQL contains an interior NUL byte")]
    InteriorNul,
    #[error("SQL contains no statement")]
    EmptySql,
    #[error("SQL contains trailing text after the first statement")]
    TrailingSql,
    #[error("text value too large")]
    TextTooLarge,
    #[error("blob value too large")]
    BlobTooLarge,
    #[error("too many SQL parameters")]
    TooManyParameters,
    #[error("SQL parameter count mismatch: expected {expected}, actual {actual}")]
    ParameterCountMismatch { expected: usize, actual: usize },
    #[error("named bind cannot be used with anonymous SQL parameter at index {index}")]
    AnonymousParameterInNamedBind { index: usize },
    #[error("SQL parameter not found: {0}")]
    ParameterNotFound(String),
}

pub struct Db;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DbHandle {
    context: ContextId,
}

impl Db {
    pub fn init(memory: DbMemory) -> Result<(), DbError> {
        let context = memory::init(memory).map_err(|error| match error {
            crate::stable::memory::StableMemoryError::AlreadyInitialized => {
                DbError::StableMemoryAlreadyInitialized
            }
            crate::stable::memory::StableMemoryError::NotInitialized => {
                DbError::StableMemoryNotInitialized
            }
            error => DbError::Stable(error),
        })?;
        clear_read_connection(context);
        clear_write_connection(context);
        let handle = DbHandle::from_context(context);
        let result = handle.initialize();
        if result.is_err() {
            clear_read_connection(context);
            clear_write_connection(context);
            memory::clear_failed_initialization(context);
        }
        result
    }

    fn default_handle() -> Result<DbHandle, DbError> {
        memory::default_context()
            .map(DbHandle::from_context)
            .ok_or(DbError::StableMemoryNotInitialized)
    }

    pub fn update<T, F>(f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>,
    {
        Self::default_handle()?.update(f)
    }

    pub fn query<T, F>(f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Connection) -> Result<T, DbError>,
    {
        Self::default_handle()?.query(f)
    }

    pub fn migrate(migrations: &[migrate::Migration]) -> Result<(), DbError> {
        Self::default_handle()?.migrate(migrations)
    }

    pub fn integrity_check() -> Result<String, DbError> {
        Self::default_handle()?.integrity_check()
    }

    pub fn export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, DbError> {
        Self::default_handle()?.export_chunk(offset, len)
    }

    pub fn db_checksum() -> Result<u64, DbError> {
        Self::default_handle()?.db_checksum()
    }

    pub fn refresh_checksum() -> Result<u64, DbError> {
        Self::default_handle()?.refresh_checksum()
    }

    pub fn refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, DbError> {
        Self::default_handle()?.refresh_checksum_chunk(max_bytes)
    }

    pub fn begin_import(total_size: u64, expected_checksum: u64) -> Result<(), DbError> {
        Self::default_handle()?.begin_import(total_size, expected_checksum)
    }

    pub fn import_chunk(offset: u64, bytes: &[u8]) -> Result<(), DbError> {
        Self::default_handle()?.import_chunk(offset, bytes)
    }

    pub fn finish_import() -> Result<(), DbError> {
        Self::default_handle()?.finish_import()
    }

    pub fn cancel_import() -> Result<(), DbError> {
        Self::default_handle()?.cancel_import()
    }

    pub fn compact() -> Result<(), DbError> {
        Self::default_handle()?.compact()
    }
}

impl DbHandle {
    pub fn init(memory: DbMemory) -> Result<Self, DbError> {
        let handle = Self::from_context(memory::init_context(memory));
        clear_read_connection(handle.context);
        clear_write_connection(handle.context);
        if let Err(error) = handle.initialize() {
            clear_read_connection(handle.context);
            clear_write_connection(handle.context);
            memory::clear_failed_initialization(handle.context);
            return Err(error);
        }
        Ok(handle)
    }

    fn from_context(context: ContextId) -> Self {
        Self { context }
    }

    fn initialize(self) -> Result<(), DbError> {
        self.with_context(|| {
            crate::sqlite_vfs::register();
            Superblock::load()?;
            stable_blob::ensure_page_map_layout()?;
            Ok(())
        })
    }

    fn with_context<T>(self, f: impl FnOnce() -> Result<T, DbError>) -> Result<T, DbError> {
        memory::with_context(self.context, f)
    }

    pub fn update<T, F>(self, f: F) -> Result<T, DbError>
    where
        F: FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>,
    {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            let db_size = stable_blob::begin_update()?;
            let _overlay_guard = OverlayGuard;
            let connection = write_connection(self.context, db_size)?;
            let result = transaction::run_immediate(&connection, f);
            clear_read_connection(self.context);
            if result.is_err() {
                clear_write_connection(self.context);
            }
            result
        })
    }

    pub fn query<T, F>(self, f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Connection) -> Result<T, DbError>,
    {
        self.with_context(|| with_read_connection(self.context, f))
    }

    pub fn migrate(self, migrations: &[migrate::Migration]) -> Result<(), DbError> {
        self.update(|connection| migrate::apply(connection, migrations))?;
        self.with_context(|| {
            let target_version = migrations
                .iter()
                .map(|migration| migration.version)
                .max()
                .unwrap_or(0);
            let mut block = Superblock::load()?;
            if block.schema_version < target_version {
                clear_read_connection(self.context);
                block.schema_version = target_version;
                block.store()?;
            }
            Ok(())
        })
    }

    pub fn integrity_check(self) -> Result<String, DbError> {
        self.query(|connection| {
            connection.query_scalar::<String>("PRAGMA integrity_check", crate::params![])
        })
    }

    pub fn export_chunk(self, offset: u64, len: u64) -> Result<Vec<u8>, DbError> {
        self.with_context(|| stable_blob::export_chunk(offset, len).map_err(DbError::from))
    }

    pub fn db_checksum(self) -> Result<u64, DbError> {
        self.with_context(|| stable_blob::checksum().map_err(DbError::from))
    }

    pub fn refresh_checksum(self) -> Result<u64, DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            stable_blob::refresh_checksum().map_err(DbError::from)
        })
    }

    pub fn refresh_checksum_chunk(self, max_bytes: u64) -> Result<ChecksumRefresh, DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            stable_blob::refresh_checksum_chunk(max_bytes).map_err(DbError::from)
        })
    }

    pub fn begin_import(self, total_size: u64, expected_checksum: u64) -> Result<(), DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            clear_write_connection(self.context);
            stable_blob::begin_import(total_size, expected_checksum).map_err(DbError::from)
        })
    }

    pub fn import_chunk(self, offset: u64, bytes: &[u8]) -> Result<(), DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            stable_blob::import_chunk(offset, bytes).map_err(DbError::from)
        })
    }

    pub fn finish_import(self) -> Result<(), DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            clear_write_connection(self.context);
            stable_blob::finish_import().map_err(DbError::from)
        })
    }

    pub fn cancel_import(self) -> Result<(), DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            clear_write_connection(self.context);
            stable_blob::cancel_import().map_err(DbError::from)
        })
    }

    pub fn compact(self) -> Result<(), DbError> {
        self.with_context(|| {
            reject_active_read_connection(self.context)?;
            clear_read_connection(self.context);
            clear_write_connection(self.context);
            stable_blob::compact().map_err(DbError::from)
        })
    }
}

fn write_connection(context: ContextId, db_size: u64) -> Result<Rc<Connection>, DbError> {
    WRITE_CONNECTIONS.with(|slot| {
        let cached = { slot.borrow().get(&context).cloned() };
        if let Some(connection) = cached {
            return Ok(connection);
        }
        let connection = if db_size == 0 {
            connection::open_read_write()?
        } else {
            connection::open_read_write_existing()?
        };
        let connection = Rc::new(connection);
        slot.borrow_mut().insert(context, Rc::clone(&connection));
        Ok(connection)
    })
}

fn with_read_connection<T>(
    context: ContextId,
    f: impl FnOnce(&Connection) -> Result<T, DbError>,
) -> Result<T, DbError> {
    READ_CONNECTIONS.with(|slot| {
        let cached = { slot.borrow().get(&context).cloned() };
        let connection = if let Some(connection) = cached {
            connection
        } else {
            let connection = Rc::new(connection::open_read_only()?);
            slot.borrow_mut().insert(context, Rc::clone(&connection));
            connection
        };
        let _guard = ReadGuard::enter(context);
        f(&connection)
    })
}

fn reject_active_read_connection(context: ContextId) -> Result<(), DbError> {
    ACTIVE_READ_CONNECTIONS.with(|slot| {
        let slot = slot.borrow();
        let active = active_read_index(&slot, context)
            .map(|index| slot[index].1)
            .unwrap_or(0);
        if active == 0 {
            Ok(())
        } else {
            Err(DbError::ReadConnectionInUse)
        }
    })
}

fn clear_read_connection(context: ContextId) {
    READ_CONNECTIONS.with(|slot| {
        slot.borrow_mut().remove(&context);
    });
}

fn clear_write_connection(context: ContextId) {
    WRITE_CONNECTIONS.with(|slot| {
        slot.borrow_mut().remove(&context);
    });
}

struct ReadGuard {
    context: ContextId,
}

impl ReadGuard {
    fn enter(context: ContextId) -> Self {
        ACTIVE_READ_CONNECTIONS.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_empty() {
                slot.push((context, 1));
                return;
            }
            if let Some(index) = active_read_index(&slot, context) {
                slot[index].1 += 1;
            } else {
                slot.push((context, 1));
            }
        });
        Self { context }
    }
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        ACTIVE_READ_CONNECTIONS.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.len() == 1 && slot[0].0 == self.context {
                let depth = &mut slot[0].1;
                *depth = depth.saturating_sub(1);
                if *depth == 0 {
                    slot.clear();
                }
                return;
            }
            let Some(index) = active_read_index(&slot, self.context) else {
                return;
            };
            let depth = &mut slot[index].1;
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                slot.swap_remove(index);
            }
        });
    }
}

fn active_read_index(entries: &[(ContextId, usize)], context: ContextId) -> Option<usize> {
    entries
        .iter()
        .position(|(stored_context, _)| *stored_context == context)
}

struct OverlayGuard;

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        stable_blob::rollback_update();
    }
}
