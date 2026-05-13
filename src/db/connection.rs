//! Thin SQLite C connection wrapper bound to the `icstable` VFS.
//!
//! `rusqlite` refuses `SQLITE_THREADSAFE=0`, so this crate keeps a small FFI
//! facade. Connections are per-message and never shared.

use crate::config::{SQLITE_URI, VFS_NAME};
use crate::db::row::Row;
use crate::db::statement::Statement;
use crate::db::value::{ToSql, Value};
use crate::db::{pragmas, DbError};
use crate::sqlite_vfs::ffi;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::{self, NonNull};

pub struct Connection {
    raw: NonNull<ffi::sqlite3>,
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
        Ok(Self { raw })
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

    pub fn execute_with_texts(&self, sql: &str, values: &[&str]) -> Result<(), DbError> {
        let values = values
            .iter()
            .map(|value| value as &dyn ToSql)
            .collect::<Vec<_>>();
        self.execute(sql, &values)
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, DbError> {
        let sql = CString::new(sql).map_err(|_| DbError::InteriorNul)?;
        let mut statement = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                self.raw.as_ptr(),
                sql.as_ptr(),
                -1,
                &mut statement,
                ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.raw.as_ptr(), rc));
        }
        let Some(raw) = NonNull::new(statement) else {
            return Err(DbError::Sqlite(
                rc,
                "sqlite3_prepare_v2 returned null".to_string(),
            ));
        };
        Ok(Statement::new(self.raw.as_ptr(), raw))
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

    pub fn query_i64(&self, sql: &str) -> Result<i64, DbError> {
        self.query_one(sql, &[], |row| row.get(0))
    }

    pub fn query_string(&self, sql: &str) -> Result<String, DbError> {
        self.query_one(sql, &[], |row| row.get(0))
    }

    pub fn query_optional_string_with_text(
        &self,
        sql: &str,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        let value = Value::Text(value);
        self.query_optional(sql, &[&value], |row| row.get(0))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_close(self.raw.as_ptr());
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
