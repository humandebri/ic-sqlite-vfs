//! Typed SQLite parameter binding.
//!
//! Values are copied into SQLite with `SQLITE_TRANSIENT`, so caller-owned text
//! and blob buffers only need to live until the bind call returns.

use crate::db::DbError;
use crate::sqlite_vfs::ffi;
use std::ffi::{c_int, c_void, CString};
use std::ptr;

pub struct Null;

pub const NULL: Null = Null;

pub enum Value<'value> {
    Text(&'value str),
    Integer(i64),
    Real(f64),
    Blob(&'value [u8]),
    Null,
}

pub trait ToSql {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError>;
}

impl ToSql for Value<'_> {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        match self {
            Value::Text(value) => bind_text(statement, index, value),
            Value::Integer(value) => bind_i64(statement, index, *value),
            Value::Real(value) => bind_f64(statement, index, *value),
            Value::Blob(value) => bind_blob(statement, index, value),
            Value::Null => bind_null(statement, index),
        }
    }
}

impl ToSql for &str {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_text(statement, index, self)
    }
}

impl ToSql for String {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_text(statement, index, self)
    }
}

impl ToSql for i64 {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_i64(statement, index, *self)
    }
}

impl ToSql for f64 {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_f64(statement, index, *self)
    }
}

impl ToSql for Vec<u8> {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_blob(statement, index, self)
    }
}

impl ToSql for &[u8] {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_blob(statement, index, self)
    }
}

impl ToSql for Null {
    fn bind_to(&self, statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
        bind_null(statement, index)
    }
}

pub(crate) fn bind_all(
    statement: *mut ffi::sqlite3_stmt,
    values: &[&dyn ToSql],
) -> Result<(), DbError> {
    for (index, value) in values.iter().enumerate() {
        let param = c_int::try_from(index + 1).map_err(|_| DbError::TooManyParameters)?;
        value.bind_to(statement, param)?;
    }
    Ok(())
}

pub(crate) fn bind_named_all(
    statement: *mut ffi::sqlite3_stmt,
    values: &[(&str, &dyn ToSql)],
) -> Result<(), DbError> {
    for (name, value) in values {
        let name_text = CString::new(*name).map_err(|_| DbError::InteriorNul)?;
        let index = unsafe { ffi::sqlite3_bind_parameter_index(statement, name_text.as_ptr()) };
        if index == 0 {
            return Err(DbError::ParameterNotFound((*name).to_string()));
        }
        value.bind_to(statement, index)?;
    }
    Ok(())
}

fn bind_text(statement: *mut ffi::sqlite3_stmt, index: c_int, value: &str) -> Result<(), DbError> {
    let value = CString::new(value).map_err(|_| DbError::InteriorNul)?;
    let len = c_int::try_from(value.as_bytes().len()).map_err(|_| DbError::TextTooLarge)?;
    let rc = unsafe {
        ffi::sqlite3_bind_text(
            statement,
            index,
            value.as_ptr(),
            len,
            ffi::SQLITE_TRANSIENT(),
        )
    };
    bind_result(rc)
}

fn bind_i64(statement: *mut ffi::sqlite3_stmt, index: c_int, value: i64) -> Result<(), DbError> {
    bind_result(unsafe { ffi::sqlite3_bind_int64(statement, index, value) })
}

fn bind_f64(statement: *mut ffi::sqlite3_stmt, index: c_int, value: f64) -> Result<(), DbError> {
    bind_result(unsafe { ffi::sqlite3_bind_double(statement, index, value) })
}

fn bind_blob(statement: *mut ffi::sqlite3_stmt, index: c_int, value: &[u8]) -> Result<(), DbError> {
    let len = c_int::try_from(value.len()).map_err(|_| DbError::BlobTooLarge)?;
    let ptr = if value.is_empty() {
        ptr::null()
    } else {
        value.as_ptr().cast::<c_void>()
    };
    let rc = unsafe { ffi::sqlite3_bind_blob(statement, index, ptr, len, ffi::SQLITE_TRANSIENT()) };
    bind_result(rc)
}

fn bind_null(statement: *mut ffi::sqlite3_stmt, index: c_int) -> Result<(), DbError> {
    bind_result(unsafe { ffi::sqlite3_bind_null(statement, index) })
}

fn bind_result(rc: c_int) -> Result<(), DbError> {
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DbError::Sqlite(rc, "sqlite bind failed".to_string()))
    }
}
