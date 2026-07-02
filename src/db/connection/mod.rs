//! Thin SQLite C connection wrapper bound to the `icstable` VFS.
//!
//! `rusqlite` refuses `SQLITE_THREADSAFE=0`, so this crate keeps a small FFI
//! facade. Write connections are per-message; read-only connections may be
//! reused inside one context cache.

use crate::config::{SQLITE_URI_NUL, STATEMENT_CACHE_CAPACITY, VFS_NAME_NUL};
use crate::db::row::{FromColumn, Row};
use crate::db::statement::Statement;
use crate::db::value::ToSql;
use crate::db::{pragmas, DbError};
use crate::sqlite_vfs::ffi;
use std::cell::RefCell;
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
    statements: Vec<CachedEntry>,
}

struct CachedEntry {
    sql: String,
    statement: NonNull<ffi::sqlite3_stmt>,
    parameter_count: usize,
}

impl StatementCache {
    fn new() -> Self {
        Self {
            statements: Vec::new(),
        }
    }

    fn take(&mut self, sql: &str) -> Option<(String, NonNull<ffi::sqlite3_stmt>, usize)> {
        if let Some(entry) = self.statements.last() {
            if entry.sql == sql {
                // Invariant: `last()` just returned `Some`, and no code can
                // mutate `statements` between the check and this `pop()`.
                let entry = self.statements.pop().expect("last cached statement exists");
                return Some((entry.sql, entry.statement, entry.parameter_count));
            }
        }
        let index = self.statements.iter().position(|entry| entry.sql == sql)?;
        let entry = self.statements.remove(index);
        Some((entry.sql, entry.statement, entry.parameter_count))
    }

    unsafe fn insert(
        &mut self,
        sql: String,
        raw: NonNull<ffi::sqlite3_stmt>,
        parameter_count: usize,
    ) {
        if let Some(index) = self.statements.iter().position(|entry| entry.sql == sql) {
            let previous = self.statements.remove(index);
            ffi::sqlite3_finalize(previous.statement.as_ptr());
        }
        self.statements.push(CachedEntry {
            sql,
            statement: raw,
            parameter_count,
        });
        self.evict_over_capacity();
    }

    unsafe fn evict_over_capacity(&mut self) {
        while self.statements.len() > STATEMENT_CACHE_CAPACITY {
            let entry = self.statements.remove(0);
            ffi::sqlite3_finalize(entry.statement.as_ptr());
        }
    }

    unsafe fn finalize_all(&mut self) {
        for entry in std::mem::take(&mut self.statements) {
            ffi::sqlite3_finalize(entry.statement.as_ptr());
        }
    }
}

pub(crate) fn open_read_write() -> Result<Connection, DbError> {
    open_read_write_with_page_size(true)
}

pub(crate) fn open_read_write_existing() -> Result<Connection, DbError> {
    open_read_write_with_page_size(false)
}

fn open_read_write_with_page_size(apply_page_size: bool) -> Result<Connection, DbError> {
    let flags = ffi::SQLITE_OPEN_READWRITE
        | ffi::SQLITE_OPEN_CREATE
        | ffi::SQLITE_OPEN_URI
        | ffi::SQLITE_OPEN_NOMUTEX;
    let connection = Connection::open(flags)?;
    pragmas::apply_read_write(&connection, apply_page_size)?;
    Ok(connection)
}

pub(crate) fn open_read_only() -> Result<Connection, DbError> {
    let flags = ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_URI | ffi::SQLITE_OPEN_NOMUTEX;
    let connection = Connection::open(flags)?;
    pragmas::apply_read_only(&connection)?;
    Ok(connection)
}

impl Connection {
    fn open(flags: c_int) -> Result<Self, DbError> {
        debug_assert!(CStr::from_bytes_with_nul(SQLITE_URI_NUL).is_ok());
        debug_assert!(CStr::from_bytes_with_nul(VFS_NAME_NUL).is_ok());
        let filename = unsafe { CStr::from_bytes_with_nul_unchecked(SQLITE_URI_NUL) };
        let vfs = unsafe { CStr::from_bytes_with_nul_unchecked(VFS_NAME_NUL) };
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
        self.execute_batch_cstr(&sql)
    }

    pub(crate) fn execute_batch_nul_terminated(&self, sql: &'static [u8]) -> Result<(), DbError> {
        debug_assert!(CStr::from_bytes_with_nul(sql).is_ok());
        let sql = unsafe { CStr::from_bytes_with_nul_unchecked(sql) };
        self.execute_batch_cstr(sql)
    }

    fn execute_batch_cstr(&self, sql: &CStr) -> Result<(), DbError> {
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

    pub fn execute_text_text(&self, sql: &str, first: &str, second: &str) -> Result<(), DbError> {
        let mut statement = self.prepare(sql)?;
        statement.execute_text_text(first, second)
    }

    #[inline(always)]
    pub fn changes(&self) -> u64 {
        unsafe { ffi::sqlite3_changes64(self.raw.as_ptr()) as u64 }
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
        if let Some((cached_sql, raw, parameter_count)) = self.cached.borrow_mut().take(sql) {
            return Ok(CachedStatement::new(
                Statement::from_cached_raw(self.raw.as_ptr(), raw, parameter_count),
                cached_sql,
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

    /// Runs a single-row query.
    ///
    /// This is a `rusqlite`-style alias for [`Connection::query_one`].
    pub fn query_row<T, F>(&self, sql: &str, values: &[&dyn ToSql], f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        self.query_one(sql, values, f)
    }

    /// Runs a single-row query with named parameters.
    ///
    /// This is a `rusqlite`-style alias for [`Connection::query_one_named`].
    pub fn query_row_named<T, F>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        self.query_one_named(sql, values, f)
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

    /// Maps all rows into a `Vec<T>`.
    ///
    /// Unlike `rusqlite::Statement::query_map`, this returns a collected
    /// `Vec<T>`, not an iterator. That keeps the prepared statement lifetime
    /// inside one synchronous canister message.
    pub fn query_map<T, F>(&self, sql: &str, values: &[&dyn ToSql], f: F) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        self.query_all(sql, values, f)
    }

    /// Maps all rows into a `Vec<T>` using named parameters.
    ///
    /// Unlike `rusqlite::Statement::query_map`, this returns a collected
    /// `Vec<T>`, not an iterator. That keeps the prepared statement lifetime
    /// inside one synchronous canister message.
    pub fn query_map_named<T, F>(
        &self,
        sql: &str,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        self.query_all_named(sql, values, f)
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
        let mut statement = self.prepare_cached(sql)?;
        statement.query_optional_string_text_borrowed(value)
    }

    #[doc(hidden)]
    /// Runs a cached borrowed-TEXT query and sums column 0 byte lengths.
    ///
    /// The statement helper clears borrowed bindings before this returns.
    pub fn query_text_iter_text_len_sum<'value, I>(
        &self,
        sql: &str,
        values: I,
    ) -> Result<u64, DbError>
    where
        I: ExactSizeIterator<Item = &'value str>,
    {
        let mut statement = self.prepare_cached(sql)?;
        statement.query_text_iter_text_len_sum(values)
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
            let rc = ffi::sqlite3_close(self.raw.as_ptr());
            debug_assert_eq!(rc, ffi::SQLITE_OK, "sqlite3_close left resources open");
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
        // Invariant: `statement` is `Some` for every live `CachedStatement`;
        // `discard` consumes `self`, and `drop` is the only path that takes it.
        self.statement
            .as_ref()
            .expect("cached statement is present")
    }
}

impl DerefMut for CachedStatement<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Invariant: mutable access requires a live value. The only method that
        // clears `statement` consumes `self`, so callers cannot observe `None`.
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
        let parameter_count = statement.parameter_count();
        let raw = statement.into_raw();
        unsafe {
            ffi::sqlite3_reset(raw.as_ptr());
            ffi::sqlite3_clear_bindings(raw.as_ptr());
            self.cache
                .borrow_mut()
                .insert(std::mem::take(&mut self.sql), raw, parameter_count);
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
    if unsafe { *tail } == 0 {
        return true;
    }
    let bytes = unsafe { CStr::from_ptr(tail).to_bytes() };
    bytes.iter().all(u8::is_ascii_whitespace)
}

#[cfg(test)]
mod tests;
