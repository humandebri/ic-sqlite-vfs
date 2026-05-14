//! Typed reads from the current SQLite result row.
//!
//! `Row` is only valid until the owning statement is stepped, reset, or
//! finalized. Getters copy text and blob data before returning.

use crate::db::DbError;
use crate::sqlite_vfs::ffi;
use std::ffi::c_int;
use std::marker::PhantomData;
use std::slice;

pub struct Row<'row> {
    raw: *mut ffi::sqlite3_stmt,
    _row: PhantomData<&'row ()>,
}

pub trait FromColumn: Sized {
    const EXPECTED: &'static str;

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextLen(pub usize);

impl Row<'_> {
    pub(crate) fn new(raw: *mut ffi::sqlite3_stmt) -> Self {
        Self {
            raw,
            _row: PhantomData,
        }
    }

    pub fn get<T: FromColumn>(&self, index: usize) -> Result<T, DbError> {
        self.check_index(index)?;
        T::read(self, index)
    }

    fn check_index(&self, index: usize) -> Result<(), DbError> {
        let count = unsafe { ffi::sqlite3_column_count(self.raw) };
        let count = usize::try_from(count).unwrap_or(0);
        if index < count {
            Ok(())
        } else {
            Err(DbError::ColumnOutOfRange { index, count })
        }
    }

    fn column_type(&self, index: usize) -> Result<c_int, DbError> {
        let index = c_int::try_from(index).map_err(|_| DbError::ColumnOutOfRange {
            index,
            count: usize::try_from(unsafe { ffi::sqlite3_column_count(self.raw) }).unwrap_or(0),
        })?;
        Ok(unsafe { ffi::sqlite3_column_type(self.raw, index) })
    }

    fn require_type(
        &self,
        index: usize,
        expected: &'static str,
        actual: c_int,
    ) -> Result<(), DbError> {
        let expected_code = match expected {
            "TEXT" => ffi::SQLITE_TEXT,
            "INTEGER" => ffi::SQLITE_INTEGER,
            "REAL" => ffi::SQLITE_FLOAT,
            "BLOB" => ffi::SQLITE_BLOB,
            "NULL" => ffi::SQLITE_NULL,
            _ => actual,
        };
        if actual == expected_code {
            Ok(())
        } else {
            Err(DbError::TypeMismatch {
                index,
                expected,
                actual: type_name(actual),
            })
        }
    }
}

impl FromColumn for String {
    const EXPECTED: &'static str = "TEXT";

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        let actual = row.column_type(index)?;
        row.require_type(index, Self::EXPECTED, actual)?;
        let index = c_int::try_from(index).map_err(|_| DbError::TooManyParameters)?;
        let text = unsafe { ffi::sqlite3_column_text(row.raw, index) };
        let len = unsafe { ffi::sqlite3_column_bytes(row.raw, index) };
        let len = usize::try_from(len).map_err(|_| DbError::TextTooLarge)?;
        if len == 0 {
            return Ok(String::new());
        }
        if text.is_null() {
            return Ok(String::new());
        }
        let bytes = unsafe { slice::from_raw_parts(text.cast::<u8>(), len) };
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

impl FromColumn for TextLen {
    const EXPECTED: &'static str = "TEXT";

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        let actual = row.column_type(index)?;
        row.require_type(index, Self::EXPECTED, actual)?;
        let index = c_int::try_from(index).map_err(|_| DbError::TooManyParameters)?;
        let len = unsafe { ffi::sqlite3_column_bytes(row.raw, index) };
        usize::try_from(len)
            .map(TextLen)
            .map_err(|_| DbError::TextTooLarge)
    }
}

impl FromColumn for i64 {
    const EXPECTED: &'static str = "INTEGER";

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        let actual = row.column_type(index)?;
        row.require_type(index, Self::EXPECTED, actual)?;
        let index = c_int::try_from(index).map_err(|_| DbError::TooManyParameters)?;
        Ok(unsafe { ffi::sqlite3_column_int64(row.raw, index) })
    }
}

impl FromColumn for f64 {
    const EXPECTED: &'static str = "REAL";

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        let actual = row.column_type(index)?;
        row.require_type(index, Self::EXPECTED, actual)?;
        let index = c_int::try_from(index).map_err(|_| DbError::TooManyParameters)?;
        Ok(unsafe { ffi::sqlite3_column_double(row.raw, index) })
    }
}

impl FromColumn for Vec<u8> {
    const EXPECTED: &'static str = "BLOB";

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        let actual = row.column_type(index)?;
        row.require_type(index, Self::EXPECTED, actual)?;
        let index = c_int::try_from(index).map_err(|_| DbError::TooManyParameters)?;
        let ptr = unsafe { ffi::sqlite3_column_blob(row.raw, index) };
        let len = unsafe { ffi::sqlite3_column_bytes(row.raw, index) };
        let len = usize::try_from(len).map_err(|_| DbError::BlobTooLarge)?;
        if len == 0 {
            return Ok(Vec::new());
        }
        if ptr.is_null() {
            return Ok(Vec::new());
        }
        let bytes = unsafe { slice::from_raw_parts(ptr.cast::<u8>(), len) };
        Ok(bytes.to_vec())
    }
}

impl<T: FromColumn> FromColumn for Option<T> {
    const EXPECTED: &'static str = T::EXPECTED;

    fn read(row: &Row<'_>, index: usize) -> Result<Self, DbError> {
        if row.column_type(index)? == ffi::SQLITE_NULL {
            Ok(None)
        } else {
            T::read(row, index).map(Some)
        }
    }
}

fn type_name(code: c_int) -> &'static str {
    match code {
        ffi::SQLITE_INTEGER => "INTEGER",
        ffi::SQLITE_FLOAT => "REAL",
        ffi::SQLITE_TEXT => "TEXT",
        ffi::SQLITE_BLOB => "BLOB",
        ffi::SQLITE_NULL => "NULL",
        _ => "UNKNOWN",
    }
}
