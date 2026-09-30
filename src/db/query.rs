//! Bounded, measured execution of one read-only SQL statement.
//!
//! Instruction limits are soft limits: SQLite invokes the progress callback
//! intermittently. Opening a connection, a single VFS operation, and user row
//! mapping cannot be preempted; checks at Rust boundaries cover these phases.
//! Leave headroom for cleanup and for the canister's response encoding. Neither
//! Candid encoding nor code after this API returns is included in the report.

use super::{connection::Connection, DbError, DbHandle, Row, ToSql};
use crate::sqlite_vfs::ffi;
use std::cell::Cell;
use std::ffi::{c_int, c_void};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetKind {
    Instructions,
    Rows,
    ResultBytes,
}

/// Errors specific to measured queries. Existing `DbError` remains unchanged.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("query budget exceeded: {0:?}")]
    BudgetExceeded(BudgetKind),
    #[error("IC instruction budgets require wasm32 execution")]
    InstructionCounterUnavailable,
    #[error("invalid query budget: {0}")]
    InvalidQueryBudget(&'static str),
    #[error("SQL exceeds max_sql_bytes")]
    SqlTooLong,
    #[error("a query observer is already active on this connection")]
    QueryObserverActive,
    #[error("measured queries require a read-only statement")]
    QueryNotReadOnly,
}

/// Limits for one query, including prepare, bind, stepping and row mapping.
///
/// `max_result_bytes` counts SQLite value payloads before mapping: NULL is zero,
/// INTEGER/REAL are eight bytes, and TEXT/BLOB use SQLite's byte length. This
/// excludes Vec/row metadata, mapper allocations and wire encoding. Row/byte
/// overflow returns an error and discards the partial result, never a truncated
/// success. SQL/value limits additionally bound SQLite input and intermediate
/// values. Existing SQLite limits are never increased by this API.
///
/// Instruction budgets require wasm32/IC. Host execution explicitly rejects
/// them rather than substituting VM steps. The mapper must be synchronous and
/// should only read the row; its own work is checked after it returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryBudget {
    pub max_instructions: Option<u64>,
    pub max_rows: u64,
    pub max_result_bytes: u64,
    /// Approximate SQLite VM steps between IC instruction checks (not IC instructions).
    pub progress_interval: u32,
    pub max_sql_bytes: Option<u32>,
    pub max_value_bytes: Option<u32>,
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self {
            max_instructions: None,
            max_rows: 10_000,
            max_result_bytes: 1_048_576,
            progress_interval: 1_000,
            max_sql_bytes: Some(1_048_576),
            max_value_bytes: Some(1_048_576),
        }
    }
}

impl QueryBudget {
    pub(crate) fn unlimited() -> Self {
        Self {
            max_rows: u64::MAX,
            max_result_bytes: u64::MAX,
            max_sql_bytes: None,
            max_value_bytes: None,
            ..Self::default()
        }
    }

    pub(super) fn validate(self) -> Result<(), QueryError> {
        if self.progress_interval == 0 || self.progress_interval > i32::MAX as u32 {
            return Err(QueryError::InvalidQueryBudget(
                "progress_interval must be 1..=i32::MAX",
            ));
        }
        for limit in [self.max_sql_bytes, self.max_value_bytes]
            .into_iter()
            .flatten()
        {
            if limit == 0 || limit > i32::MAX as u32 {
                return Err(QueryError::InvalidQueryBudget(
                    "SQL/value limits must be 1..=i32::MAX",
                ));
            }
        }
        if self.max_instructions.is_some() && instruction_counter().is_none() {
            return Err(QueryError::InstructionCounterUnavailable);
        }
        Ok(())
    }
}

/// SQLite execution counters. These are SQLite VM counters, not IC instructions.
/// SQLite exposes signed 32-bit counters; very long executions may overflow.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatementMetrics {
    pub vm_steps: u32,
    pub full_scan_steps: u32,
    pub sorts: u32,
    pub autoindex_rows: u32,
    pub reprepares: u32,
    pub runs: u32,
    pub statement_memory_bytes: u32,
}

impl StatementMetrics {
    pub(crate) fn read(raw: *mut ffi::sqlite3_stmt, reset: bool) -> Self {
        let counter = |op| unsafe { ffi::sqlite3_stmt_status(raw, op, i32::from(reset)) as u32 };
        Self {
            vm_steps: counter(ffi::SQLITE_STMTSTATUS_VM_STEP),
            full_scan_steps: counter(ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP),
            sorts: counter(ffi::SQLITE_STMTSTATUS_SORT),
            autoindex_rows: counter(ffi::SQLITE_STMTSTATUS_AUTOINDEX),
            reprepares: counter(ffi::SQLITE_STMTSTATUS_REPREPARE),
            runs: counter(ffi::SQLITE_STMTSTATUS_RUN),
            statement_memory_bytes: counter(ffi::SQLITE_STMTSTATUS_MEMUSED),
        }
    }
}

/// Physical stable data I/O and logical VFS I/O during this call.
/// Stable metadata reads are excluded from stable_data_read_bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VfsMetrics {
    pub read_calls: u64,
    pub read_bytes: u64,
    pub stable_data_read_calls: u64,
    pub stable_data_read_bytes: u64,
}

/// IC instruction deltas are None on host builds. Total includes connection
/// acquisition and cleanup; phase deltas cover prepare, bind and row iteration.
/// `result_rows`/`result_bytes` count rows admitted to the mapper, even if it fails.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryMetrics {
    pub instructions: Option<u64>,
    pub prepare_instructions: Option<u64>,
    pub bind_instructions: Option<u64>,
    pub execute_instructions: Option<u64>,
    pub statement: StatementMetrics,
    pub result_rows: u64,
    pub result_bytes: u64,
    /// Available only with the `query-metrics` feature.
    pub vfs: Option<VfsMetrics>,
}

/// Metrics survive SQL, mapping and budget errors. No partial rows escape on error.
#[derive(Debug)]
pub struct QueryReport<T> {
    pub result: Result<Vec<T>, QueryError>,
    pub metrics: QueryMetrics,
}

impl<T> QueryReport<T> {
    pub(crate) fn failed(error: impl Into<QueryError>) -> Self {
        Self {
            result: Err(error.into()),
            metrics: QueryMetrics::default(),
        }
    }
}

pub(super) fn instruction_counter() -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    {
        Some(ic_cdk::api::performance_counter(0))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

pub(super) fn delta(start: Option<u64>) -> Option<u64> {
    start
        .zip(instruction_counter())
        .map(|(start, end)| end.saturating_sub(start))
}

pub(super) struct BudgetState {
    pub(super) start: Option<u64>,
    pub(super) limit: Option<u64>,
    pub(super) exceeded: Cell<bool>,
}

impl BudgetState {
    fn check_at(&self, current: Option<u64>) -> Result<(), QueryError> {
        if let (Some(start), Some(current), Some(limit)) = (self.start, current, self.limit) {
            if current.saturating_sub(start) > limit {
                self.exceeded.set(true);
            }
        }
        if self.exceeded.get() {
            Err(QueryError::BudgetExceeded(BudgetKind::Instructions))
        } else {
            Ok(())
        }
    }

    pub(super) fn check(&self) -> Result<(), QueryError> {
        if self.limit.is_none() {
            return Ok(());
        }
        self.check_at(instruction_counter())
    }
}

// SQLite receives a pointer to a stack state that outlives ObserverGuard. The
// callback never invokes SQLite, allocates, panics or borrows a RefCell.
unsafe extern "C" fn progress(data: *mut c_void) -> c_int {
    let state = &*data.cast::<BudgetState>();
    c_int::from(state.check().is_err())
}

pub(super) struct ObserverGuard<'a> {
    connection: &'a Connection,
    sql_limit: c_int,
    value_limit: c_int,
    progress_installed: bool,
    _state: &'a BudgetState,
}

impl<'a> ObserverGuard<'a> {
    pub(super) fn enter(
        connection: &'a Connection,
        budget: QueryBudget,
        state: &'a BudgetState,
    ) -> Result<Self, QueryError> {
        if connection.query_observer_active.replace(true) {
            return Err(QueryError::QueryObserverActive);
        }
        let raw = connection.raw();
        let sql_limit = unsafe { ffi::sqlite3_limit(raw, ffi::SQLITE_LIMIT_SQL_LENGTH, -1) };
        let value_limit = unsafe { ffi::sqlite3_limit(raw, ffi::SQLITE_LIMIT_LENGTH, -1) };
        unsafe {
            if let Some(limit) = budget.max_sql_bytes {
                ffi::sqlite3_limit(
                    raw,
                    ffi::SQLITE_LIMIT_SQL_LENGTH,
                    sql_limit.min(limit as c_int),
                );
            }
            if let Some(limit) = budget.max_value_bytes {
                ffi::sqlite3_limit(
                    raw,
                    ffi::SQLITE_LIMIT_LENGTH,
                    value_limit.min(limit as c_int),
                );
            }
            if budget.max_instructions.is_some() {
                ffi::sqlite3_progress_handler(
                    raw,
                    budget.progress_interval as c_int,
                    Some(progress),
                    (state as *const BudgetState).cast_mut().cast(),
                );
            }
        }
        Ok(Self {
            connection,
            sql_limit,
            value_limit,
            progress_installed: budget.max_instructions.is_some(),
            _state: state,
        })
    }
}

impl Drop for ObserverGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.progress_installed {
                ffi::sqlite3_progress_handler(self.connection.raw(), 0, None, std::ptr::null_mut());
            }
            ffi::sqlite3_limit(
                self.connection.raw(),
                ffi::SQLITE_LIMIT_SQL_LENGTH,
                self.sql_limit,
            );
            ffi::sqlite3_limit(
                self.connection.raw(),
                ffi::SQLITE_LIMIT_LENGTH,
                self.value_limit,
            );
        }
        self.connection.query_observer_active.set(false);
    }
}

pub(crate) fn run<T, F>(
    handle: DbHandle,
    sql: &str,
    values: &[&dyn ToSql],
    budget: QueryBudget,
    mut map: F,
) -> QueryReport<T>
where
    F: FnMut(&Row<'_>) -> Result<T, DbError>,
{
    if let Err(error) = budget.validate() {
        return QueryReport::failed(error);
    }
    let start = instruction_counter();
    let state = BudgetState {
        start,
        limit: budget.max_instructions,
        exceeded: Cell::new(false),
    };
    let vfs = VfsScope::enter();
    let mut metrics = QueryMetrics::default();
    let result = handle
        .query(|connection| {
            Ok((|| -> Result<Vec<T>, QueryError> {
                state.check()?;
                let _guard = ObserverGuard::enter(connection, budget, &state)?;
                if let Some(limit) = budget.max_sql_bytes {
                    if sql.len() > limit as usize {
                        return Err(QueryError::SqlTooLong);
                    }
                }
                let prepare_start = instruction_counter();
                let prepared = connection.prepare(sql);
                metrics.prepare_instructions = delta(prepare_start);
                state.check()?;
                let mut statement = prepared?;
                if !statement.is_read_only() {
                    return Err(QueryError::QueryNotReadOnly);
                }
                let result = (|| {
                    let bind_start = instruction_counter();
                    let bound = if budget.max_instructions.is_some() {
                        statement.query_with_bind_check(values, || state.check())
                    } else {
                        statement.query(values).map_err(QueryError::from)
                    };
                    metrics.bind_instructions = delta(bind_start);
                    state.check()?;
                    let mut rows = bound?;
                    let execute_start = instruction_counter();
                    let scanned = (|| {
                        let mut output = Vec::new();
                        loop {
                            state.check()?;
                            let stepped = rows.next_row();
                            state.check()?;
                            let Some(row) = stepped? else {
                                break;
                            };
                            if metrics.result_rows >= budget.max_rows {
                                return Err(QueryError::BudgetExceeded(BudgetKind::Rows));
                            }
                            let bytes = row.payload_bytes();
                            if bytes > budget.max_result_bytes.saturating_sub(metrics.result_bytes)
                            {
                                return Err(QueryError::BudgetExceeded(BudgetKind::ResultBytes));
                            }
                            metrics.result_rows += 1;
                            metrics.result_bytes += bytes;
                            let mapped = map(&row);
                            state.check()?;
                            output.push(mapped?);
                        }
                        Ok(output)
                    })();
                    metrics.execute_instructions = delta(execute_start);
                    scanned
                })();
                metrics.statement = statement.metrics();
                result
            })())
        })
        .unwrap_or_else(|error| Err(error.into()));
    metrics.instructions = delta(start);
    metrics.vfs = vfs.snapshot();
    // Includes finalization/guard cleanup in the soft budget as well.
    let result = state.check().and(result);
    QueryReport { result, metrics }
}

struct VfsScope {
    #[cfg(feature = "query-metrics")]
    previously_enabled: bool,
    #[cfg(feature = "query-metrics")]
    start: crate::read_metrics::ReadMetrics,
}

impl VfsScope {
    fn enter() -> Self {
        Self {
            #[cfg(feature = "query-metrics")]
            previously_enabled: crate::read_metrics::set_metrics_enabled(true),
            #[cfg(feature = "query-metrics")]
            start: crate::read_metrics::read_metrics_snapshot(),
        }
    }

    fn snapshot(&self) -> Option<VfsMetrics> {
        #[cfg(feature = "query-metrics")]
        {
            let end = crate::read_metrics::read_metrics_snapshot();
            Some(VfsMetrics {
                read_calls: end.x_read_calls.saturating_sub(self.start.x_read_calls),
                read_bytes: end.x_read_bytes.saturating_sub(self.start.x_read_bytes),
                stable_data_read_calls: end
                    .stable_data_read_calls
                    .saturating_sub(self.start.stable_data_read_calls),
                stable_data_read_bytes: end
                    .stable_data_read_bytes
                    .saturating_sub(self.start.stable_data_read_bytes),
            })
        }
        #[cfg(not(feature = "query-metrics"))]
        {
            None
        }
    }
}

impl Drop for VfsScope {
    fn drop(&mut self) {
        #[cfg(feature = "query-metrics")]
        crate::read_metrics::set_metrics_enabled(self.previously_enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_budget_is_a_delta_and_failure_is_sticky() {
        let state = BudgetState {
            start: Some(1_000),
            limit: Some(100),
            exceeded: Cell::new(false),
        };
        assert!(state.check_at(Some(1_100)).is_ok());
        assert!(matches!(
            state.check_at(Some(1_101)),
            Err(QueryError::BudgetExceeded(BudgetKind::Instructions))
        ));
        assert!(state.check_at(Some(1_000)).is_err());
    }
}
