//! Thin SQLite C connection wrapper bound to the `icstable` VFS.
//!
//! `rusqlite` refuses `SQLITE_THREADSAFE=0`, so this crate keeps a small FFI
//! facade. Connections are per-message and never shared.

use crate::config::{SQLITE_URI, STATEMENT_CACHE_CAPACITY, VFS_NAME};
use crate::db::row::{FromColumn, Row};
use crate::db::statement::Statement;
use crate::db::value::ToSql;
use crate::db::{pragmas, DbError};
use crate::sqlite_vfs::ffi;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};

pub struct Connection {
    raw: NonNull<ffi::sqlite3>,
    cached: RefCell<StatementCache>,
}

pub struct CachedStatement<'connection> {
    statement: Option<Statement<'connection>>,
    sql: String,
    cache: &'connection RefCell<StatementCache>,
}

struct StatementCache {
    statements: BTreeMap<String, NonNull<ffi::sqlite3_stmt>>,
    returned_lru: VecDeque<String>,
}

impl StatementCache {
    fn new() -> Self {
        Self {
            statements: BTreeMap::new(),
            returned_lru: VecDeque::new(),
        }
    }

    fn take(&mut self, sql: &str) -> Option<NonNull<ffi::sqlite3_stmt>> {
        let raw = self.statements.remove(sql)?;
        self.returned_lru.retain(|cached_sql| cached_sql != sql);
        Some(raw)
    }

    unsafe fn insert(&mut self, sql: String, raw: NonNull<ffi::sqlite3_stmt>) {
        if let Some(previous) = self.statements.insert(sql.clone(), raw) {
            ffi::sqlite3_finalize(previous.as_ptr());
        }
        self.returned_lru.retain(|cached_sql| cached_sql != &sql);
        self.returned_lru.push_back(sql);
        self.evict_over_capacity();
    }

    unsafe fn evict_over_capacity(&mut self) {
        while self.statements.len() > STATEMENT_CACHE_CAPACITY {
            let Some(sql) = self.returned_lru.pop_front() else {
                return;
            };
            if let Some(statement) = self.statements.remove(&sql) {
                ffi::sqlite3_finalize(statement.as_ptr());
            }
        }
    }

    unsafe fn finalize_all(&mut self) {
        for (_, statement) in std::mem::take(&mut self.statements) {
            ffi::sqlite3_finalize(statement.as_ptr());
        }
        self.returned_lru.clear();
    }
}

pub fn open_read_write() -> Result<Connection, DbError> {
    let flags = ffi::SQLITE_OPEN_READWRITE
        | ffi::SQLITE_OPEN_CREATE
        | ffi::SQLITE_OPEN_URI
        | ffi::SQLITE_OPEN_NOMUTEX;
    let connection = Connection::open(flags)?;
    pragmas::apply_read_write(&connection)?;
    Ok(connection)
}

pub fn open_read_only() -> Result<Connection, DbError> {
    let flags = ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_URI | ffi::SQLITE_OPEN_NOMUTEX;
    let connection = Connection::open(flags)?;
    pragmas::apply_read_only(&connection)?;
    Ok(connection)
}

impl Connection {
    fn open(flags: c_int) -> Result<Self, DbError> {
        let filename = CString::new(SQLITE_URI).map_err(|_| DbError::InteriorNul)?;
        let vfs = CString::new(VFS_NAME).map_err(|_| DbError::InteriorNul)?;
        let mut db = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_open_v2(filename.as_ptr(), &mut db, flags, vfs.as_ptr()) };
        let Some(raw) = NonNull::new(db) else {
            return Err(DbError::Sqlite(
                rc,
                "sqlite3_open_v2 returned null".to_string(),
            ));
        };
        if rc != ffi::SQLITE_OK {
            let error = sqlite_error(raw.as_ptr(), rc);
            unsafe {
                ffi::sqlite3_close(raw.as_ptr());
            }
            return Err(error);
        }
        Ok(Self {
            raw,
            cached: RefCell::new(StatementCache::new()),
        })
    }

    pub fn raw(&self) -> *mut ffi::sqlite3 {
        self.raw.as_ptr()
    }

    pub fn execute_batch(&self, sql: &str) -> Result<(), DbError> {
        let sql = CString::new(sql).map_err(|_| DbError::InteriorNul)?;
        let mut error = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.raw.as_ptr(),
                sql.as_ptr(),
                None,
                ptr::null_mut(),
                &mut error,
            )
        };
        if rc == ffi::SQLITE_OK {
            return Ok(());
        }
        Err(classify_sqlite_error(rc, take_error_message(error)))
    }

    pub fn execute(&self, sql: &str, values: &[&dyn ToSql]) -> Result<(), DbError> {
        let mut statement = self.prepare(sql)?;
        statement.execute(values)
    }

    pub fn execute_named(&self, sql: &str, values: &[(&str, &dyn ToSql)]) -> Result<(), DbError> {
        let mut statement = self.prepare(sql)?;
        statement.execute_named(values)
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, DbError> {
        let sql = CString::new(sql).map_err(|_| DbError::InteriorNul)?;
        let mut statement = ptr::null_mut();
        let mut tail = ptr::null();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                self.raw.as_ptr(),
                sql.as_ptr(),
                -1,
                &mut statement,
                &mut tail,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.raw.as_ptr(), rc));
        }
        let Some(raw) = NonNull::new(statement) else {
            return Err(DbError::EmptySql);
        };
        if !tail_is_empty(tail) {
            unsafe {
                ffi::sqlite3_finalize(raw.as_ptr());
            }
            return Err(DbError::TrailingSql);
        }
        Ok(Statement::new(self.raw.as_ptr(), raw))
    }

    pub fn prepare_cached(&self, sql: &str) -> Result<CachedStatement<'_>, DbError> {
        if let Some(raw) = self.cached.borrow_mut().take(sql) {
            return Ok(CachedStatement::new(
                Statement::new(self.raw.as_ptr(), raw),
                sql.to_string(),
                &self.cached,
            ));
        }
        let statement = self.prepare(sql)?;
        Ok(CachedStatement::new(
            statement,
            sql.to_string(),
            &self.cached,
        ))
    }

    pub fn query_one<T, F>(&self, sql: &str, values: &[&dyn ToSql], f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_one(values, f)
    }

    pub fn query_one_named<T, F>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_one_named(values, f)
    }

    pub fn query_optional<T, F>(
        &self,
        sql: &str,
        values: &[&dyn ToSql],
        f: F,
    ) -> Result<Option<T>, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_optional(values, f)
    }

    pub fn query_optional_named<T, F>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<Option<T>, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_optional_named(values, f)
    }

    pub fn query_all<T, F>(&self, sql: &str, values: &[&dyn ToSql], f: F) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_all(values, f)
    }

    pub fn query_all_named<T, F>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        let mut statement = self.prepare(sql)?;
        statement.query_all_named(values, f)
    }

    pub fn exists(&self, sql: &str, values: &[&dyn ToSql]) -> Result<bool, DbError> {
        self.query_optional(sql, values, |row| row.get::<i64>(0))
            .map(|value| value.unwrap_or(0) != 0)
    }

    pub fn query_scalar<T: FromColumn>(
        &self,
        sql: &str,
        values: &[&dyn ToSql],
    ) -> Result<T, DbError> {
        self.query_one(sql, values, |row| row.get(0))
    }

    pub fn query_scalar_named<T: FromColumn>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<T, DbError> {
        self.query_one_named(sql, values, |row| row.get(0))
    }

    pub fn query_optional_scalar<T: FromColumn>(
        &self,
        sql: &str,
        values: &[&dyn ToSql],
    ) -> Result<Option<T>, DbError> {
        self.query_optional(sql, values, |row| row.get(0))
    }

    pub fn query_optional_string_text(
        &self,
        sql: &str,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        let mut statement = self.prepare(sql)?;
        statement.query_optional_string_text(value)
    }

    pub fn query_optional_scalar_named<T: FromColumn>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Option<T>, DbError> {
        self.query_optional_named(sql, values, |row| row.get(0))
    }

    pub fn query_column<T: FromColumn>(
        &self,
        sql: &str,
        values: &[&dyn ToSql],
    ) -> Result<Vec<T>, DbError> {
        self.query_all(sql, values, |row| row.get(0))
    }

    pub fn query_column_named<T: FromColumn>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Vec<T>, DbError> {
        self.query_all_named(sql, values, |row| row.get(0))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            self.cached.get_mut().finalize_all();
            ffi::sqlite3_close(self.raw.as_ptr());
        }
    }
}

impl<'connection> CachedStatement<'connection> {
    fn new(
        statement: Statement<'connection>,
        sql: String,
        cache: &'connection RefCell<StatementCache>,
    ) -> Self {
        Self {
            statement: Some(statement),
            sql,
            cache,
        }
    }

    pub fn discard(mut self) {
        if let Some(statement) = self.statement.take() {
            unsafe {
                ffi::sqlite3_finalize(statement.into_raw().as_ptr());
            }
        }
    }
}

impl<'connection> Deref for CachedStatement<'connection> {
    type Target = Statement<'connection>;

    fn deref(&self) -> &Self::Target {
        self.statement
            .as_ref()
            .expect("cached statement is present")
    }
}

impl DerefMut for CachedStatement<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.statement
            .as_mut()
            .expect("cached statement is present")
    }
}

impl Drop for CachedStatement<'_> {
    fn drop(&mut self) {
        let Some(statement) = self.statement.take() else {
            return;
        };
        let raw = statement.into_raw();
        unsafe {
            ffi::sqlite3_reset(raw.as_ptr());
            ffi::sqlite3_clear_bindings(raw.as_ptr());
            self.cache.borrow_mut().insert(self.sql.clone(), raw);
        }
    }
}

pub(crate) fn sqlite_error(db: *mut ffi::sqlite3, code: c_int) -> DbError {
    let message = unsafe {
        let ptr = ffi::sqlite3_errmsg(db);
        if ptr.is_null() {
            "unknown sqlite error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    classify_sqlite_error(code, message)
}

fn classify_sqlite_error(code: c_int, message: String) -> DbError {
    if code == ffi::SQLITE_CONSTRAINT {
        DbError::Constraint(message)
    } else {
        DbError::Sqlite(code, message)
    }
}

fn take_error_message(error: *mut c_char) -> String {
    if error.is_null() {
        return "unknown sqlite error".to_string();
    }
    let message = unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() };
    unsafe {
        ffi::sqlite3_free(error.cast::<c_void>());
    }
    message
}

fn tail_is_empty(tail: *const c_char) -> bool {
    if tail.is_null() {
        return true;
    }
    let bytes = unsafe { CStr::from_ptr(tail).to_bytes() };
    bytes.iter().all(u8::is_ascii_whitespace)
}

#[cfg(test)]
mod tests {
    use super::open_read_write;
    use crate::config::STATEMENT_CACHE_CAPACITY;
    use crate::sqlite_vfs::{lock, stable_blob};
    use crate::stable::memory;
    use crate::Db;
    use serial_test::serial;

    fn reset() {
        stable_blob::rollback_update();
        stable_blob::invalidate_read_cache();
        memory::reset_for_tests();
        lock::reset_for_tests();
        Db::init(memory::memory_for_tests()).unwrap();
    }

    #[test]
    #[serial]
    fn cached_statements_are_lru_bounded() {
        reset();
        let connection = open_read_write().unwrap();

        for index in 0..(STATEMENT_CACHE_CAPACITY + 8) {
            let sql = format!("SELECT {index}");
            let mut statement = connection.prepare_cached(&sql).unwrap();
            let value = statement.query_scalar::<i64>(crate::params![]).unwrap();
            assert_eq!(value, i64::try_from(index).unwrap());
        }

        let cache = connection.cached.borrow();
        assert_eq!(cache.statements.len(), STATEMENT_CACHE_CAPACITY);
        assert!(!cache.statements.contains_key("SELECT 0"));
        assert!(cache
            .statements
            .contains_key(&format!("SELECT {}", STATEMENT_CACHE_CAPACITY + 7)));
    }

    #[test]
    #[serial]
    fn discarded_cached_statement_is_finalized_not_cached() {
        reset();
        let connection = open_read_write().unwrap();

        let statement = connection.prepare_cached("SELECT 1").unwrap();
        statement.discard();

        assert_eq!(connection.cached.borrow().statements.len(), 0);
    }
}
