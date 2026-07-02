//! Prepared statement lifecycle and row iteration.
//!
//! Each execution resets and rebinds the statement. `Drop` finalizes regular
//! statements; cached statements return to the connection cache.

use crate::db::connection::sqlite_error;
use crate::db::row::{FromColumn, Row};
use crate::db::value::{bind_all_with_count, bind_named_all, ToSql};
use crate::db::DbError;
use crate::sqlite_vfs::ffi;
use std::ffi::c_void;
use std::ptr::NonNull;

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
use std::cell::RefCell;
#[cfg(any(test, feature = "canister-api-test-failpoints"))]
use std::collections::BTreeMap;

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
thread_local! {
    static STEP_FAILPOINTS: RefCell<BTreeMap<crate::stable::memory::ContextId, StepFailpointState>> = const { RefCell::new(BTreeMap::new()) };
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepFailpoint {
    pub ordinal: u64,
    pub code: std::ffi::c_int,
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug)]
struct StepFailpointState {
    failpoint: StepFailpoint,
    count: u64,
}

pub struct Statement<'connection> {
    db: *mut ffi::sqlite3,
    raw: NonNull<ffi::sqlite3_stmt>,
    parameter_count: usize,
    _connection: std::marker::PhantomData<&'connection ()>,
}

pub struct Rows<'statement, 'connection> {
    statement: &'statement mut Statement<'connection>,
    done: bool,
    clear_bindings_on_drop: bool,
}

#[cfg(feature = "bench-profile")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryOptionalStringTextProfile {
    pub reset_bind: u64,
    pub step: u64,
    pub column_read: u64,
}

#[cfg(feature = "bench-profile")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExecuteTextTextProfile {
    pub reset_bind: u64,
    pub step: u64,
}

#[cfg(feature = "bench-profile")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryTextLenSumProfile {
    pub reset_bind: u64,
    pub row_scan: u64,
}

impl<'connection> Statement<'connection> {
    pub(crate) fn new(db: *mut ffi::sqlite3, raw: NonNull<ffi::sqlite3_stmt>) -> Self {
        let parameter_count =
            usize::try_from(unsafe { ffi::sqlite3_bind_parameter_count(raw.as_ptr()) })
                .unwrap_or(0);
        Self::from_cached_raw(db, raw, parameter_count)
    }

    pub(crate) fn from_cached_raw(
        db: *mut ffi::sqlite3,
        raw: NonNull<ffi::sqlite3_stmt>,
        parameter_count: usize,
    ) -> Self {
        Self {
            db,
            raw,
            parameter_count,
            _connection: std::marker::PhantomData,
        }
    }

    pub(crate) fn parameter_count(&self) -> usize {
        self.parameter_count
    }

    pub(crate) fn into_raw(self) -> NonNull<ffi::sqlite3_stmt> {
        let raw = self.raw;
        std::mem::forget(self);
        raw
    }

    pub fn execute(&mut self, values: &[&dyn ToSql]) -> Result<(), DbError> {
        self.reset_and_bind(values)?;
        let rc = step(self.raw.as_ptr())?;
        if rc == ffi::SQLITE_DONE {
            Ok(())
        } else {
            Err(sqlite_error(self.db, rc))
        }
    }

    pub fn execute_named(&mut self, values: &[(&str, &dyn ToSql)]) -> Result<(), DbError> {
        self.reset_and_bind_named(values)?;
        let rc = step(self.raw.as_ptr())?;
        if rc == ffi::SQLITE_DONE {
            Ok(())
        } else {
            Err(sqlite_error(self.db, rc))
        }
    }

    /// Executes a two-TEXT statement without copying bound text into SQLite.
    ///
    /// The text is bound with `SQLITE_STATIC` only for the duration of the
    /// SQLite step and cleared before this function returns.
    #[inline(always)]
    pub fn execute_text_text(&mut self, first: &str, second: &str) -> Result<(), DbError> {
        if let Err(error) = self.reset_and_bind_two_text_borrowed(first, second) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_DONE) => Ok(()),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    #[cfg(feature = "bench-profile")]
    #[doc(hidden)]
    pub fn execute_text_text_profiled(
        &mut self,
        first: &str,
        second: &str,
    ) -> Result<ExecuteTextTextProfile, DbError> {
        let mut profile = ExecuteTextTextProfile::default();

        let start = instruction_counter();
        if let Err(error) = self.reset_and_bind_two_text_borrowed(first, second) {
            self.clear_bindings();
            return Err(error);
        }
        profile.reset_bind = instruction_counter().saturating_sub(start);

        let start = instruction_counter();
        let rc = step(self.raw.as_ptr());
        profile.step = instruction_counter().saturating_sub(start);

        let result = match rc {
            Ok(ffi::SQLITE_DONE) => Ok(profile),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    /// Executes an INTEGER/TEXT statement without copying bound text into SQLite.
    ///
    /// The text is bound with `SQLITE_STATIC` only for the duration of the
    /// SQLite step and cleared before this function returns.
    #[inline(always)]
    pub fn execute_i64_text(&mut self, first: i64, second: &str) -> Result<(), DbError> {
        if let Err(error) = self.reset_and_bind_i64_text_borrowed(first, second) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_DONE) => Ok(()),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    /// Executes an INTEGER/BLOB statement without copying bound bytes into SQLite.
    ///
    /// The blob is bound with `SQLITE_STATIC` only for the duration of the
    /// SQLite step and cleared before this function returns.
    #[inline(always)]
    pub fn execute_i64_blob(&mut self, first: i64, second: &[u8]) -> Result<(), DbError> {
        if let Err(error) = self.reset_and_bind_i64_blob_borrowed(first, second) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_DONE) => Ok(()),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    /// Executes an INTEGER/INTEGER/TEXT statement without copying bound text into SQLite.
    ///
    /// The text is bound with `SQLITE_STATIC` only for the duration of the
    /// SQLite step and cleared before this function returns.
    #[inline(always)]
    pub fn execute_i64_i64_text(
        &mut self,
        first: i64,
        second: i64,
        third: &str,
    ) -> Result<(), DbError> {
        if let Err(error) = self.reset_and_bind_i64_i64_text_borrowed(first, second, third) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_DONE) => Ok(()),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    pub fn query<'statement>(
        &'statement mut self,
        values: &[&dyn ToSql],
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind(values)?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: false,
        })
    }

    #[doc(hidden)]
    /// Runs a query with one INTEGER parameter without dynamic parameter dispatch.
    #[inline(always)]
    pub fn query_i64<'statement>(
        &'statement mut self,
        value: i64,
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind_single_i64(value)?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: false,
        })
    }

    #[doc(hidden)]
    /// Runs a query with borrowed TEXT parameters without dynamic parameter dispatch.
    pub fn query_texts<'statement>(
        &'statement mut self,
        values: &'statement [&'statement str],
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind_text_iter(values.iter().copied())?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: true,
        })
    }

    #[doc(hidden)]
    /// Runs a query with borrowed TEXT parameters without materializing a slice.
    pub fn query_text_iter<'statement, I>(
        &'statement mut self,
        values: I,
    ) -> Result<Rows<'statement, 'connection>, DbError>
    where
        I: ExactSizeIterator<Item = &'statement str>,
    {
        self.reset_and_bind_text_iter(values)?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: true,
        })
    }

    #[doc(hidden)]
    /// Runs a one-shot query with borrowed TEXT parameters.
    ///
    /// Borrowed bindings are cleared when the returned rows are dropped.
    pub fn query_text_iter_ephemeral<'statement, I>(
        &'statement mut self,
        values: I,
    ) -> Result<Rows<'statement, 'connection>, DbError>
    where
        I: ExactSizeIterator<Item = &'statement str>,
    {
        self.reset_and_bind_text_iter(values)?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: true,
        })
    }

    #[doc(hidden)]
    /// Runs a borrowed-TEXT query and sums column 0 byte lengths.
    ///
    /// This keeps cached borrowed bindings safe by clearing them before return.
    pub fn query_text_iter_text_len_sum<'value, I>(&mut self, values: I) -> Result<u64, DbError>
    where
        I: ExactSizeIterator<Item = &'value str>,
    {
        if let Err(error) = self.reset_and_bind_text_iter(values) {
            self.clear_bindings();
            return Err(error);
        }

        let statement = self.raw.as_ptr();
        let mut total = 0_u64;
        loop {
            match step(statement) {
                Ok(ffi::SQLITE_ROW) => {
                    total = total.wrapping_add(read_string_column_zero_len(statement) as u64);
                }
                Ok(ffi::SQLITE_DONE) => {
                    self.clear_bindings();
                    return Ok(total);
                }
                Ok(rc) => {
                    self.clear_bindings();
                    return Err(sqlite_error(self.db, rc));
                }
                Err(error) => {
                    self.clear_bindings();
                    return Err(error);
                }
            }
        }
    }

    #[cfg(feature = "bench-profile")]
    #[doc(hidden)]
    pub fn query_text_iter_text_len_sum_profiled<'value, I>(
        &mut self,
        values: I,
    ) -> Result<(u64, QueryTextLenSumProfile), DbError>
    where
        I: ExactSizeIterator<Item = &'value str>,
    {
        let mut profile = QueryTextLenSumProfile::default();

        let start = instruction_counter();
        if let Err(error) = self.reset_and_bind_text_iter(values) {
            self.clear_bindings();
            return Err(error);
        }
        profile.reset_bind = instruction_counter().saturating_sub(start);

        let start = instruction_counter();
        let statement = self.raw.as_ptr();
        let mut total = 0_u64;
        loop {
            match step(statement) {
                Ok(ffi::SQLITE_ROW) => {
                    total = total.wrapping_add(read_string_column_zero_len(statement) as u64);
                }
                Ok(ffi::SQLITE_DONE) => {
                    profile.row_scan = instruction_counter().saturating_sub(start);
                    self.clear_bindings();
                    return Ok((total, profile));
                }
                Ok(rc) => {
                    self.clear_bindings();
                    return Err(sqlite_error(self.db, rc));
                }
                Err(error) => {
                    self.clear_bindings();
                    return Err(error);
                }
            }
        }
    }

    pub fn query_named<'statement>(
        &'statement mut self,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind_named(values)?;
        Ok(Rows {
            statement: self,
            done: false,
            clear_bindings_on_drop: false,
        })
    }

    pub fn query_one<T, F>(&mut self, values: &[&dyn ToSql], f: F) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query(values)?;
        match rows.next_row()? {
            Some(row) => f(&row),
            None => Err(DbError::NotFound),
        }
    }

    pub fn query_one_named<T, F>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<T, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query_named(values)?;
        match rows.next_row()? {
            Some(row) => f(&row),
            None => Err(DbError::NotFound),
        }
    }

    pub fn query_optional<T, F>(
        &mut self,
        values: &[&dyn ToSql],
        f: F,
    ) -> Result<Option<T>, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query(values)?;
        match rows.next_row()? {
            Some(row) => f(&row).map(Some),
            None => Ok(None),
        }
    }

    pub fn query_optional_named<T, F>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
        f: F,
    ) -> Result<Option<T>, DbError>
    where
        F: FnOnce(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query_named(values)?;
        match rows.next_row()? {
            Some(row) => f(&row).map(Some),
            None => Ok(None),
        }
    }

    pub fn query_all<T, F>(&mut self, values: &[&dyn ToSql], mut f: F) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query(values)?;
        let mut output = Vec::new();
        while let Some(row) = rows.next_row()? {
            output.push(f(&row)?);
        }
        Ok(output)
    }

    pub fn query_all_named<T, F>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
        mut f: F,
    ) -> Result<Vec<T>, DbError>
    where
        F: FnMut(&Row<'_>) -> Result<T, DbError>,
    {
        let mut rows = self.query_named(values)?;
        let mut output = Vec::new();
        while let Some(row) = rows.next_row()? {
            output.push(f(&row)?);
        }
        Ok(output)
    }

    pub fn query_scalar<T: FromColumn>(&mut self, values: &[&dyn ToSql]) -> Result<T, DbError> {
        self.query_one(values, |row| row.get(0))
    }
    pub fn query_scalar_named<T: FromColumn>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<T, DbError> {
        self.query_one_named(values, |row| row.get(0))
    }

    pub fn query_optional_scalar<T: FromColumn>(
        &mut self,
        values: &[&dyn ToSql],
    ) -> Result<Option<T>, DbError> {
        self.query_optional(values, |row| row.get(0))
    }

    #[inline(always)]
    pub fn query_optional_string_text(&mut self, value: &str) -> Result<Option<String>, DbError> {
        self.query_optional_string_text_borrowed(value)
    }

    #[inline(always)]
    pub(crate) fn query_optional_string_text_borrowed(
        &mut self,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        if let Err(error) = self.reset_and_bind_single_text_borrowed(value) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_ROW) => read_string_column_zero(self.raw.as_ptr()).map(Some),
            Ok(ffi::SQLITE_DONE) => Ok(None),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    #[inline(always)]
    pub fn query_optional_string_text_len(
        &mut self,
        value: &str,
    ) -> Result<Option<usize>, DbError> {
        self.query_optional_string_text_len_borrowed(value)
    }

    #[inline(always)]
    pub(crate) fn query_optional_string_text_len_borrowed(
        &mut self,
        value: &str,
    ) -> Result<Option<usize>, DbError> {
        if let Err(error) = self.reset_and_bind_single_text_borrowed(value) {
            self.clear_bindings();
            return Err(error);
        }
        let rc = step(self.raw.as_ptr());
        let result = match rc {
            Ok(ffi::SQLITE_ROW) => Ok(Some(read_string_column_zero_len(self.raw.as_ptr()))),
            Ok(ffi::SQLITE_DONE) => Ok(None),
            Ok(rc) => Err(sqlite_error(self.db, rc)),
            Err(error) => Err(error),
        };
        self.clear_bindings();
        result
    }

    #[cfg(feature = "bench-profile")]
    #[doc(hidden)]
    pub fn query_optional_string_text_profiled(
        &mut self,
        value: &str,
    ) -> Result<(Option<String>, QueryOptionalStringTextProfile), DbError> {
        let mut profile = QueryOptionalStringTextProfile::default();

        let start = instruction_counter();
        if let Err(error) = self.reset_and_bind_single_text_borrowed(value) {
            self.clear_bindings();
            return Err(error);
        }
        profile.reset_bind = instruction_counter().saturating_sub(start);

        let start = instruction_counter();
        let rc = step(self.raw.as_ptr());
        profile.step = instruction_counter().saturating_sub(start);

        match rc {
            Ok(ffi::SQLITE_ROW) => {
                let start = instruction_counter();
                let value = read_string_column_zero(self.raw.as_ptr()).map(Some);
                profile.column_read = instruction_counter().saturating_sub(start);
                self.clear_bindings();
                value.map(|value| (value, profile))
            }
            Ok(ffi::SQLITE_DONE) => {
                self.clear_bindings();
                Ok((None, profile))
            }
            Ok(rc) => {
                self.clear_bindings();
                Err(sqlite_error(self.db, rc))
            }
            Err(error) => {
                self.clear_bindings();
                Err(error)
            }
        }
    }

    #[cfg(feature = "bench-profile")]
    #[doc(hidden)]
    pub fn query_optional_string_text_len_profiled(
        &mut self,
        value: &str,
    ) -> Result<(Option<usize>, QueryOptionalStringTextProfile), DbError> {
        let mut profile = QueryOptionalStringTextProfile::default();

        let start = instruction_counter();
        if let Err(error) = self.reset_and_bind_single_text_borrowed(value) {
            self.clear_bindings();
            return Err(error);
        }
        profile.reset_bind = instruction_counter().saturating_sub(start);

        let start = instruction_counter();
        let rc = step(self.raw.as_ptr());
        profile.step = instruction_counter().saturating_sub(start);

        match rc {
            Ok(ffi::SQLITE_ROW) => {
                let start = instruction_counter();
                let value = Some(read_string_column_zero_len(self.raw.as_ptr()));
                profile.column_read = instruction_counter().saturating_sub(start);
                self.clear_bindings();
                Ok((value, profile))
            }
            Ok(ffi::SQLITE_DONE) => {
                self.clear_bindings();
                Ok((None, profile))
            }
            Ok(rc) => {
                self.clear_bindings();
                Err(sqlite_error(self.db, rc))
            }
            Err(error) => {
                self.clear_bindings();
                Err(error)
            }
        }
    }

    pub fn query_optional_scalar_named<T: FromColumn>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Option<T>, DbError> {
        self.query_optional_named(values, |row| row.get(0))
    }

    pub fn query_column<T: FromColumn>(
        &mut self,
        values: &[&dyn ToSql],
    ) -> Result<Vec<T>, DbError> {
        self.query_all(values, |row| row.get(0))
    }

    pub fn query_column_named<T: FromColumn>(
        &mut self,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Vec<T>, DbError> {
        self.query_all_named(values, |row| row.get(0))
    }

    fn reset_and_bind(&mut self, values: &[&dyn ToSql]) -> Result<(), DbError> {
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        if self.parameter_count == 0 && values.is_empty() {
            return Ok(());
        }
        bind_all_with_count(self.raw.as_ptr(), values, self.parameter_count)
    }

    #[inline(always)]
    fn reset_and_bind_single_text_borrowed(&mut self, value: &str) -> Result<(), DbError> {
        if self.parameter_count != 1 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 1,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_text_static(self.raw.as_ptr(), 1, value)
    }

    #[inline(always)]
    fn reset_and_bind_single_i64(&mut self, value: i64) -> Result<(), DbError> {
        if self.parameter_count != 1 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 1,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_i64(self.raw.as_ptr(), 1, value)
    }

    #[inline(always)]
    fn reset_and_bind_two_text_borrowed(
        &mut self,
        first: &str,
        second: &str,
    ) -> Result<(), DbError> {
        if self.parameter_count != 2 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 2,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        let first_len =
            std::ffi::c_int::try_from(first.len()).map_err(|_| DbError::TextTooLarge)?;
        let first_rc = unsafe {
            ffi::sqlite3_bind_text(
                self.raw.as_ptr(),
                1,
                first.as_ptr().cast(),
                first_len,
                ffi::SQLITE_STATIC(),
            )
        };
        if first_rc != ffi::SQLITE_OK {
            return Err(DbError::Sqlite(first_rc, "sqlite bind failed".to_string()));
        }
        let second_len =
            std::ffi::c_int::try_from(second.len()).map_err(|_| DbError::TextTooLarge)?;
        let second_rc = unsafe {
            ffi::sqlite3_bind_text(
                self.raw.as_ptr(),
                2,
                second.as_ptr().cast(),
                second_len,
                ffi::SQLITE_STATIC(),
            )
        };
        if second_rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(DbError::Sqlite(second_rc, "sqlite bind failed".to_string()))
        }
    }

    fn reset_and_bind_text_iter<'value, I>(&mut self, values: I) -> Result<(), DbError>
    where
        I: ExactSizeIterator<Item = &'value str>,
    {
        let actual = values.len();
        if actual != self.parameter_count {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        for (param, value) in (1..).zip(values) {
            let len = match std::ffi::c_int::try_from(value.len()) {
                Ok(len) => len,
                Err(_) => {
                    self.clear_bindings();
                    return Err(DbError::TextTooLarge);
                }
            };
            let rc = unsafe {
                ffi::sqlite3_bind_text(
                    self.raw.as_ptr(),
                    param,
                    value.as_ptr().cast(),
                    len,
                    ffi::SQLITE_STATIC(),
                )
            };
            if rc != ffi::SQLITE_OK {
                self.clear_bindings();
                return Err(DbError::Sqlite(rc, "sqlite bind failed".to_string()));
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn reset_and_bind_i64_text_borrowed(
        &mut self,
        first: i64,
        second: &str,
    ) -> Result<(), DbError> {
        if self.parameter_count != 2 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 2,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_i64(self.raw.as_ptr(), 1, first)?;
        let second_len =
            std::ffi::c_int::try_from(second.len()).map_err(|_| DbError::TextTooLarge)?;
        let second_rc = unsafe {
            ffi::sqlite3_bind_text(
                self.raw.as_ptr(),
                2,
                second.as_ptr().cast(),
                second_len,
                ffi::SQLITE_STATIC(),
            )
        };
        if second_rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(DbError::Sqlite(second_rc, "sqlite bind failed".to_string()))
        }
    }

    #[inline(always)]
    fn reset_and_bind_i64_blob_borrowed(
        &mut self,
        first: i64,
        second: &[u8],
    ) -> Result<(), DbError> {
        if self.parameter_count != 2 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 2,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_i64(self.raw.as_ptr(), 1, first)?;
        bind_blob_static(self.raw.as_ptr(), 2, second)
    }

    #[inline(always)]
    fn reset_and_bind_i64_i64_text_borrowed(
        &mut self,
        first: i64,
        second: i64,
        third: &str,
    ) -> Result<(), DbError> {
        if self.parameter_count != 3 {
            return Err(DbError::ParameterCountMismatch {
                expected: self.parameter_count,
                actual: 3,
            });
        }
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_i64(self.raw.as_ptr(), 1, first)?;
        bind_i64(self.raw.as_ptr(), 2, second)?;
        let third_len =
            std::ffi::c_int::try_from(third.len()).map_err(|_| DbError::TextTooLarge)?;
        let third_rc = unsafe {
            ffi::sqlite3_bind_text(
                self.raw.as_ptr(),
                3,
                third.as_ptr().cast(),
                third_len,
                ffi::SQLITE_STATIC(),
            )
        };
        if third_rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(DbError::Sqlite(third_rc, "sqlite bind failed".to_string()))
        }
    }

    fn clear_bindings(&mut self) {
        unsafe {
            ffi::sqlite3_clear_bindings(self.raw.as_ptr());
        }
    }

    fn reset_and_bind_named(&mut self, values: &[(&str, &dyn ToSql)]) -> Result<(), DbError> {
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        bind_named_all(self.raw.as_ptr(), values)
    }
}

static EMPTY_BLOB: u8 = 0;

#[inline(always)]
fn read_string_column_zero(statement: *mut ffi::sqlite3_stmt) -> Result<String, DbError> {
    let actual = unsafe { ffi::sqlite3_column_type(statement, 0) };
    if actual != ffi::SQLITE_TEXT {
        return Err(DbError::TypeMismatch {
            index: 0,
            expected: "TEXT",
            actual: sqlite_type_name(actual),
        });
    }
    let text = unsafe { ffi::sqlite3_column_text(statement, 0) };
    let len = unsafe { ffi::sqlite3_column_bytes(statement, 0) as usize };
    if len == 0 || text.is_null() {
        return Ok(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), len) };
    match std::str::from_utf8(bytes) {
        Ok(value) => Ok(value.to_owned()),
        Err(_) => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

#[inline(always)]
fn read_string_column_zero_len(statement: *mut ffi::sqlite3_stmt) -> usize {
    unsafe { ffi::sqlite3_column_bytes(statement, 0) as usize }
}

#[inline]
fn bind_text_static(
    statement: *mut ffi::sqlite3_stmt,
    index: std::ffi::c_int,
    value: &str,
) -> Result<(), DbError> {
    let len = std::ffi::c_int::try_from(value.len()).map_err(|_| DbError::TextTooLarge)?;
    let rc = unsafe {
        ffi::sqlite3_bind_text(
            statement,
            index,
            value.as_ptr().cast(),
            len,
            ffi::SQLITE_STATIC(),
        )
    };
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DbError::Sqlite(rc, "sqlite bind failed".to_string()))
    }
}

#[inline(always)]
fn bind_blob_static(
    statement: *mut ffi::sqlite3_stmt,
    index: std::ffi::c_int,
    value: &[u8],
) -> Result<(), DbError> {
    let len = std::ffi::c_int::try_from(value.len()).map_err(|_| DbError::BlobTooLarge)?;
    let ptr = if value.is_empty() {
        (&EMPTY_BLOB as *const u8).cast::<c_void>()
    } else {
        value.as_ptr().cast::<c_void>()
    };
    let rc = unsafe { ffi::sqlite3_bind_blob(statement, index, ptr, len, ffi::SQLITE_STATIC()) };
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DbError::Sqlite(rc, "sqlite bind failed".to_string()))
    }
}

#[inline(always)]
fn bind_i64(
    statement: *mut ffi::sqlite3_stmt,
    index: std::ffi::c_int,
    value: i64,
) -> Result<(), DbError> {
    let rc = unsafe { ffi::sqlite3_bind_int64(statement, index, value) };
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DbError::Sqlite(rc, "sqlite bind failed".to_string()))
    }
}

fn sqlite_type_name(code: std::ffi::c_int) -> &'static str {
    match code {
        ffi::SQLITE_INTEGER => "INTEGER",
        ffi::SQLITE_FLOAT => "REAL",
        ffi::SQLITE_TEXT => "TEXT",
        ffi::SQLITE_BLOB => "BLOB",
        ffi::SQLITE_NULL => "NULL",
        _ => "UNKNOWN",
    }
}

impl Rows<'_, '_> {
    pub fn next_row(&mut self) -> Result<Option<Row<'_>>, DbError> {
        if self.done {
            return Ok(None);
        }
        let rc = step(self.statement.raw.as_ptr())?;
        match rc {
            ffi::SQLITE_ROW => Ok(Some(Row::new(self.statement.raw.as_ptr()))),
            ffi::SQLITE_DONE => {
                self.done = true;
                self.clear_static_bindings();
                Ok(None)
            }
            _ => Err(sqlite_error(self.statement.db, rc)),
        }
    }

    #[doc(hidden)]
    /// Reads column 0 byte length for call sites that already select a known TEXT expression.
    ///
    /// General typed reads still use `Row::get`; this helper skips per-row type
    /// checks for hot benchmark paths that only need TEXT lengths.
    #[inline(always)]
    pub fn next_text_len_zero(&mut self) -> Result<Option<usize>, DbError> {
        let statement = self.statement.raw.as_ptr();
        let rc = step(statement)?;
        match rc {
            ffi::SQLITE_ROW => Ok(Some(read_string_column_zero_len(statement))),
            ffi::SQLITE_DONE => {
                self.done = true;
                self.clear_static_bindings();
                Ok(None)
            }
            _ => Err(sqlite_error(self.statement.db, rc)),
        }
    }

    fn clear_static_bindings(&mut self) {
        if self.clear_bindings_on_drop {
            self.statement.clear_bindings();
            self.clear_bindings_on_drop = false;
        }
    }
}

impl Drop for Rows<'_, '_> {
    fn drop(&mut self) {
        self.clear_static_bindings();
    }
}

#[inline(always)]
fn step(statement: *mut ffi::sqlite3_stmt) -> Result<std::ffi::c_int, DbError> {
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if let Some(code) = hit_step_failpoint() {
        return Err(DbError::Sqlite(code, "sqlite step failpoint".to_string()));
    }
    Ok(unsafe { ffi::sqlite3_step(statement) })
}

#[cfg(feature = "bench-profile")]
fn instruction_counter() -> u64 {
    crate::ic0_shim::performance_counter(0)
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn set_step_failpoint(failpoint: StepFailpoint) {
    if let Ok(context) = crate::stable::memory::active_context_id() {
        STEP_FAILPOINTS.with(|slot| {
            slot.borrow_mut().insert(
                context,
                StepFailpointState {
                    failpoint,
                    count: 0,
                },
            );
        });
    }
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn clear_step_failpoint() {
    STEP_FAILPOINTS.with(|slot| slot.borrow_mut().clear());
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_step_failpoint() -> Option<std::ffi::c_int> {
    let Ok(context) = crate::stable::memory::active_context_id() else {
        return None;
    };
    STEP_FAILPOINTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.get_mut(&context)?;
        state.count += 1;
        if state.failpoint.ordinal == state.count {
            let code = state.failpoint.code;
            slot.remove(&context);
            Some(code)
        } else {
            None
        }
    })
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_finalize(self.raw.as_ptr());
        }
    }
}
