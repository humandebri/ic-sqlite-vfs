//! Thin SQLite C connection wrapper bound to the `icstable` VFS.
//!
//! `rusqlite` refuses `SQLITE_THREADSAFE=0`, so the facade uses SQLite C FFI
//! directly. Connections are per-message and never shared.

use crate::config::{SQLITE_URI, VFS_NAME};
use crate::db::{pragmas, DbError};
use crate::sqlite_vfs::ffi;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::{self, NonNull};

pub struct Connection {
    raw: NonNull<ffi::sqlite3>,
}

pub struct Statement<'connection> {
    connection: &'connection Connection,
    raw: NonNull<ffi::sqlite3_stmt>,
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
        let message = take_error_message(error);
        Err(DbError::Sqlite(rc, message))
    }

    pub fn query_i64(&self, sql: &str) -> Result<i64, DbError> {
        self.with_statement(sql, |statement| {
            let rc = unsafe { ffi::sqlite3_step(statement) };
            if rc != ffi::SQLITE_ROW {
                return Err(sqlite_error(self.raw.as_ptr(), rc));
            }
            Ok(unsafe { ffi::sqlite3_column_int64(statement, 0) })
        })
    }

    pub fn query_string(&self, sql: &str) -> Result<String, DbError> {
        self.with_statement(sql, |statement| {
            let rc = unsafe { ffi::sqlite3_step(statement) };
            if rc != ffi::SQLITE_ROW {
                return Err(sqlite_error(self.raw.as_ptr(), rc));
            }
            let text = unsafe { ffi::sqlite3_column_text(statement, 0) };
            if text.is_null() {
                return Ok(String::new());
            }
            let text = unsafe { CStr::from_ptr(text.cast::<c_char>()) };
            Ok(text.to_string_lossy().into_owned())
        })
    }

    pub fn raw(&self) -> *mut ffi::sqlite3 {
        self.raw.as_ptr()
    }

    pub fn execute_with_texts(&self, sql: &str, values: &[&str]) -> Result<(), DbError> {
        self.with_statement(sql, |statement| {
            bind_texts(statement, values)?;
            let rc = unsafe { ffi::sqlite3_step(statement) };
            if rc == ffi::SQLITE_DONE {
                Ok(())
            } else {
                Err(sqlite_error(self.raw.as_ptr(), rc))
            }
        })
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
        Ok(Statement {
            connection: self,
            raw,
        })
    }

    pub fn query_optional_string_with_text(
        &self,
        sql: &str,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        self.with_statement(sql, |statement| {
            bind_texts(statement, &[value])?;
            let rc = unsafe { ffi::sqlite3_step(statement) };
            if rc == ffi::SQLITE_DONE {
                return Ok(None);
            }
            if rc != ffi::SQLITE_ROW {
                return Err(sqlite_error(self.raw.as_ptr(), rc));
            }
            let text = unsafe { ffi::sqlite3_column_text(statement, 0) };
            if text.is_null() {
                return Ok(None);
            }
            let text = unsafe { CStr::from_ptr(text.cast::<c_char>()) };
            Ok(Some(text.to_string_lossy().into_owned()))
        })
    }

    fn with_statement<T, F>(&self, sql: &str, f: F) -> Result<T, DbError>
    where
        F: FnOnce(*mut ffi::sqlite3_stmt) -> Result<T, DbError>,
    {
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
        let result = f(statement);
        let finalize_rc = unsafe { ffi::sqlite3_finalize(statement) };
        if finalize_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.raw.as_ptr(), finalize_rc));
        }
        result
    }
}

fn bind_texts(statement: *mut ffi::sqlite3_stmt, values: &[&str]) -> Result<(), DbError> {
    for (index, value) in values.iter().enumerate() {
        let value = CString::new(*value).map_err(|_| DbError::InteriorNul)?;
        let len = c_int::try_from(value.as_bytes().len()).map_err(|_| DbError::TextTooLarge)?;
        let param = c_int::try_from(index + 1).map_err(|_| DbError::TooManyParameters)?;
        let rc = unsafe {
            ffi::sqlite3_bind_text(
                statement,
                param,
                value.as_ptr(),
                len,
                ffi::SQLITE_TRANSIENT(),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(DbError::Sqlite(rc, "sqlite3_bind_text failed".to_string()));
        }
    }
    Ok(())
}

impl Statement<'_> {
    pub fn execute_with_texts(&mut self, values: &[&str]) -> Result<(), DbError> {
        self.reset_and_clear()?;
        bind_texts(self.raw.as_ptr(), values)?;
        let rc = unsafe { ffi::sqlite3_step(self.raw.as_ptr()) };
        if rc == ffi::SQLITE_DONE {
            Ok(())
        } else {
            Err(sqlite_error(self.connection.raw.as_ptr(), rc))
        }
    }

    pub fn query_optional_string_with_text(
        &mut self,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        self.reset_and_clear()?;
        bind_texts(self.raw.as_ptr(), &[value])?;
        let rc = unsafe { ffi::sqlite3_step(self.raw.as_ptr()) };
        if rc == ffi::SQLITE_DONE {
            return Ok(None);
        }
        if rc != ffi::SQLITE_ROW {
            return Err(sqlite_error(self.connection.raw.as_ptr(), rc));
        }
        let text = unsafe { ffi::sqlite3_column_text(self.raw.as_ptr(), 0) };
        if text.is_null() {
            return Ok(None);
        }
        let text = unsafe { CStr::from_ptr(text.cast::<c_char>()) };
        Ok(Some(text.to_string_lossy().into_owned()))
    }

    fn reset_and_clear(&mut self) -> Result<(), DbError> {
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.connection.raw.as_ptr(), reset_rc));
        }
        let clear_rc = unsafe { ffi::sqlite3_clear_bindings(self.raw.as_ptr()) };
        if clear_rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(sqlite_error(self.connection.raw.as_ptr(), clear_rc))
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_close(self.raw.as_ptr());
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_finalize(self.raw.as_ptr());
        }
    }
}

fn sqlite_error(db: *mut ffi::sqlite3, code: c_int) -> DbError {
    let message = unsafe {
        let ptr = ffi::sqlite3_errmsg(db);
        if ptr.is_null() {
            "unknown sqlite error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    DbError::Sqlite(code, message)
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
