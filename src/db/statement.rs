//! Prepared statement lifecycle and row iteration.
//!
//! Each execution resets and clears bindings first. `Drop` finalizes the SQLite
//! statement, so prepared statements can be reused without leaking C resources.

use crate::db::connection::sqlite_error;
use crate::db::row::Row;
use crate::db::value::{bind_all, bind_named_all, ToSql};
use crate::db::DbError;
use crate::sqlite_vfs::ffi;
use std::ptr::NonNull;

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
use std::cell::RefCell;

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
thread_local! {
    static STEP_FAILPOINT: RefCell<Option<StepFailpoint>> = const { RefCell::new(None) };
    static STEP_COUNT: RefCell<u64> = const { RefCell::new(0) };
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepFailpoint {
    pub ordinal: u64,
    pub code: std::ffi::c_int,
}

pub struct Statement<'connection> {
    db: *mut ffi::sqlite3,
    raw: NonNull<ffi::sqlite3_stmt>,
    _connection: std::marker::PhantomData<&'connection ()>,
}

pub struct Rows<'statement, 'connection> {
    statement: &'statement mut Statement<'connection>,
    done: bool,
}

impl<'connection> Statement<'connection> {
    pub(crate) fn new(db: *mut ffi::sqlite3, raw: NonNull<ffi::sqlite3_stmt>) -> Self {
        Self {
            db,
            raw,
            _connection: std::marker::PhantomData,
        }
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

    pub fn execute_with_texts(&mut self, values: &[&str]) -> Result<(), DbError> {
        let values = values
            .iter()
            .map(|value| value as &dyn ToSql)
            .collect::<Vec<_>>();
        self.execute(&values)
    }

    pub fn query<'statement>(
        &'statement mut self,
        values: &[&dyn ToSql],
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind(values)?;
        Ok(Rows {
            statement: self,
            done: false,
        })
    }

    pub fn query_named<'statement>(
        &'statement mut self,
        values: &[(&str, &dyn ToSql)],
    ) -> Result<Rows<'statement, 'connection>, DbError> {
        self.reset_and_bind_named(values)?;
        Ok(Rows {
            statement: self,
            done: false,
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

    pub fn query_optional_string_with_text(
        &mut self,
        value: &str,
    ) -> Result<Option<String>, DbError> {
        self.query_optional(&[&value], |row| row.get(0))
    }

    fn reset_and_bind(&mut self, values: &[&dyn ToSql]) -> Result<(), DbError> {
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        let clear_rc = unsafe { ffi::sqlite3_clear_bindings(self.raw.as_ptr()) };
        if clear_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, clear_rc));
        }
        bind_all(self.raw.as_ptr(), values)
    }

    fn reset_and_bind_named(&mut self, values: &[(&str, &dyn ToSql)]) -> Result<(), DbError> {
        let reset_rc = unsafe { ffi::sqlite3_reset(self.raw.as_ptr()) };
        if reset_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, reset_rc));
        }
        let clear_rc = unsafe { ffi::sqlite3_clear_bindings(self.raw.as_ptr()) };
        if clear_rc != ffi::SQLITE_OK {
            return Err(sqlite_error(self.db, clear_rc));
        }
        bind_named_all(self.raw.as_ptr(), values)
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
                Ok(None)
            }
            _ => Err(sqlite_error(self.statement.db, rc)),
        }
    }
}

fn step(statement: *mut ffi::sqlite3_stmt) -> Result<std::ffi::c_int, DbError> {
    #[cfg(any(test, feature = "canister-api-test-failpoints"))]
    if let Some(code) = hit_step_failpoint() {
        return Err(DbError::Sqlite(code, "sqlite step failpoint".to_string()));
    }
    Ok(unsafe { ffi::sqlite3_step(statement) })
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn set_step_failpoint(failpoint: StepFailpoint) {
    STEP_FAILPOINT.with(|slot| *slot.borrow_mut() = Some(failpoint));
    STEP_COUNT.with(|count| *count.borrow_mut() = 0);
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
pub fn clear_step_failpoint() {
    STEP_FAILPOINT.with(|slot| *slot.borrow_mut() = None);
    STEP_COUNT.with(|count| *count.borrow_mut() = 0);
}

#[cfg(any(test, feature = "canister-api-test-failpoints"))]
fn hit_step_failpoint() -> Option<std::ffi::c_int> {
    STEP_COUNT.with(|count| {
        let mut count = count.borrow_mut();
        *count += 1;
        let current = *count;
        STEP_FAILPOINT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let failpoint = *slot;
            if failpoint.is_some_and(|value| value.ordinal == current) {
                *slot = None;
                failpoint.map(|value| value.code)
            } else {
                None
            }
        })
    })
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_finalize(self.raw.as_ptr());
        }
    }
}
