//! Bounded execution of one SQL write in an isolated synchronous transaction.
//!
//! Execution is interruptible before stable publication. The final stable commit
//! cannot be interrupted safely: reserve instruction headroom and inspect the
//! reported total. A post-publication soft-limit overrun remains a success.
use super::query::{self, BudgetState, ObserverGuard};
use super::{Connection, DbError, DbHandle, QueryBudget, QueryError, StatementMetrics, ToSql};
use crate::sqlite_vfs::{ffi, overlay, stable_blob};
use crate::stable::memory::ContextId;
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateBudgetKind {
    Instructions,
    DirtyPages,
    ChangedBytes,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("update budget exceeded: {0:?}")]
    BudgetExceeded(UpdateBudgetKind),
    #[error("invalid update budget: {0}")]
    InvalidBudget(&'static str),
    #[error("IC instruction budgets require wasm32 execution")]
    InstructionCounterUnavailable,
    #[error("budgeted updates require a write statement without transaction, PRAGMA, ATTACH, DETACH or temporary-database operations")]
    UnsupportedStatement,
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// Dirty-page limits apply to the peak resident overlay, before allocating each
/// new page. Changed bytes are whole SQLite pages, not changed SQL value bytes.
/// SQLite's own page cache and other allocations are outside this memory limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpdateBudget {
    pub max_instructions: Option<u64>,
    /// Reserved within max_instructions for stable commit and cleanup. Must be
    /// positive and smaller than max_instructions when instruction limits apply.
    pub commit_reserve_instructions: u64,
    pub max_dirty_pages: u64,
    pub max_changed_bytes: u64,
    pub progress_interval: u32,
    pub max_sql_bytes: Option<u32>,
    pub max_value_bytes: Option<u32>,
}

impl Default for UpdateBudget {
    fn default() -> Self {
        Self {
            max_instructions: None,
            commit_reserve_instructions: 10_000_000,
            max_dirty_pages: 64,
            max_changed_bytes: 1_048_576,
            progress_interval: 1_000,
            max_sql_bytes: Some(1_048_576),
            max_value_bytes: Some(1_048_576),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UpdateMetrics {
    pub instructions: Option<u64>,
    pub prepare_instructions: Option<u64>,
    pub bind_instructions: Option<u64>,
    pub execute_instructions: Option<u64>,
    /// SQLite COMMIT plus final stable publication on success; no rollback time.
    pub commit_instructions: Option<u64>,
    pub statement: StatementMetrics,
    pub peak_dirty_pages: u64,
    pub overlay_peak_bytes: u64,
    pub committed_bytes: u64,
    /// True only for a successful commit whose measured total exceeds the soft
    /// limit. Never return a budget error after publishing a stable commit.
    pub committed_over_soft_limit: bool,
}

#[derive(Debug)]
pub struct UpdateReport {
    /// Direct SQLite changes(), excluding auxiliary trigger changes. Statements
    /// that change schema (including virtual table DDL) return zero.
    /// RETURNING rows are drained without allocating a result collection.
    pub result: Result<u64, UpdateError>,
    pub metrics: UpdateMetrics,
}

impl UpdateReport {
    pub(crate) fn failed(error: impl Into<UpdateError>) -> Self {
        Self {
            result: Err(error.into()),
            metrics: UpdateMetrics::default(),
        }
    }
}

fn classify(error: QueryError) -> UpdateError {
    match error {
        QueryError::BudgetExceeded(_) => {
            UpdateError::BudgetExceeded(UpdateBudgetKind::Instructions)
        }
        QueryError::InstructionCounterUnavailable => UpdateError::InstructionCounterUnavailable,
        QueryError::InvalidQueryBudget(reason) => UpdateError::InvalidBudget(reason),
        QueryError::Database(error) => error.into(),
        error => error.into(),
    }
}

struct Authorizer<'a> {
    connection: &'a super::connection::Connection,
    _schema_change: &'a Cell<bool>,
}
unsafe extern "C" fn authorize(
    data: *mut c_void,
    action: c_int,
    first: *const c_char,
    second: *const c_char,
    database: *const c_char,
    _: *const c_char,
) -> c_int {
    if matches!(
        action,
        ffi::SQLITE_CREATE_INDEX
            | ffi::SQLITE_CREATE_TABLE
            | ffi::SQLITE_CREATE_TRIGGER
            | ffi::SQLITE_CREATE_VIEW
            | ffi::SQLITE_CREATE_VTABLE
            | ffi::SQLITE_DROP_INDEX
            | ffi::SQLITE_DROP_TABLE
            | ffi::SQLITE_DROP_TRIGGER
            | ffi::SQLITE_DROP_VIEW
            | ffi::SQLITE_DROP_VTABLE
            | ffi::SQLITE_ALTER_TABLE
            | ffi::SQLITE_REINDEX
            | ffi::SQLITE_ANALYZE
    ) {
        (&*data.cast::<Cell<bool>>()).set(true);
    }
    let temporary = !database.is_null() && std::ffi::CStr::from_ptr(database).to_bytes() == b"temp";
    // FTS5 uses this read-only pragma internally when starting a write. Its
    // assignment form remains forbidden; top-level reads fail is_read_only().
    if action == ffi::SQLITE_PRAGMA
        && !temporary
        && second.is_null()
        && !first.is_null()
        && std::ffi::CStr::from_ptr(first).to_bytes() == b"data_version"
    {
        return ffi::SQLITE_OK;
    }
    if temporary
        || matches!(
            action,
            ffi::SQLITE_TRANSACTION
                | ffi::SQLITE_SAVEPOINT
                | ffi::SQLITE_PRAGMA
                | ffi::SQLITE_ATTACH
                | ffi::SQLITE_DETACH
                | ffi::SQLITE_CREATE_TEMP_TABLE
                | ffi::SQLITE_CREATE_TEMP_INDEX
                | ffi::SQLITE_CREATE_TEMP_TRIGGER
                | ffi::SQLITE_CREATE_TEMP_VIEW
                | ffi::SQLITE_DROP_TEMP_TABLE
                | ffi::SQLITE_DROP_TEMP_INDEX
                | ffi::SQLITE_DROP_TEMP_TRIGGER
                | ffi::SQLITE_DROP_TEMP_VIEW
        )
    {
        ffi::SQLITE_DENY
    } else {
        ffi::SQLITE_OK
    }
}
impl<'a> Authorizer<'a> {
    fn enter(
        connection: &'a super::connection::Connection,
        schema_change: &'a Cell<bool>,
    ) -> Result<Self, DbError> {
        let rc = unsafe {
            ffi::sqlite3_set_authorizer(
                connection.raw(),
                Some(authorize),
                (schema_change as *const Cell<bool>).cast_mut().cast(),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(super::connection::sqlite_error(connection.raw(), rc));
        }
        Ok(Self {
            connection,
            _schema_change: schema_change,
        })
    }
}
impl Drop for Authorizer<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_set_authorizer(self.connection.raw(), None, std::ptr::null_mut());
        }
    }
}

/// Outlives the statement, authorizer and progress handler, and drops before
/// the overlay. Also runs during host unwinding, while the DB context is active.
struct WriteConnectionGuard<'a> {
    connection: &'a Connection,
    context: ContextId,
    published: bool,
}

impl Drop for WriteConnectionGuard<'_> {
    fn drop(&mut self) {
        super::clear_read_connection(self.context);
        if !self.published {
            let _ = self.connection.execute_batch("ROLLBACK");
            super::clear_write_connection(self.context);
        }
    }
}

pub(crate) fn run(
    handle: DbHandle,
    sql: &str,
    values: &[&dyn ToSql],
    budget: UpdateBudget,
) -> UpdateReport {
    let execution_limit = match budget.max_instructions {
        Some(limit)
            if budget.commit_reserve_instructions == 0
                || limit <= budget.commit_reserve_instructions =>
        {
            return UpdateReport::failed(UpdateError::InvalidBudget(
                "commit reserve must be positive and smaller than max_instructions",
            ));
        }
        Some(limit) => Some(limit - budget.commit_reserve_instructions),
        None => None,
    };
    let query_budget = QueryBudget {
        max_instructions: execution_limit,
        max_sql_bytes: budget.max_sql_bytes,
        max_value_bytes: budget.max_value_bytes,
        progress_interval: budget.progress_interval,
        ..QueryBudget::default()
    };
    if let Err(error) = query_budget.validate() {
        return UpdateReport::failed(classify(error));
    }
    let start = query::instruction_counter();
    let state = BudgetState {
        start,
        limit: execution_limit,
        exceeded: Cell::new(false),
    };
    let mut metrics = UpdateMetrics::default();
    let result = handle
        .with_context(|| {
            super::reject_active_read_connection(handle.context)?;
            super::clear_read_connection(handle.context);
            let size = stable_blob::begin_update()?;
            let _overlay_guard = super::OverlayGuard;
            overlay::set_write_budget(budget.max_dirty_pages, budget.max_changed_bytes);
            let connection = super::write_connection(handle.context, size)?;
            let mut connection_guard = WriteConnectionGuard {
                connection: &connection,
                context: handle.context,
                published: false,
            };
            let result = (|| -> Result<u64, UpdateError> {
                state.check().map_err(classify)?;
                connection.execute_batch("BEGIN")?;
                (|| -> Result<u64, UpdateError> {
                    let guard = ObserverGuard::enter(&connection, query_budget, &state)
                        .map_err(classify)?;
                    let schema_change = Cell::new(false);
                    let authorizer = Authorizer::enter(&connection, &schema_change)?;
                    if budget
                        .max_sql_bytes
                        .is_some_and(|limit| sql.len() > limit as usize)
                    {
                        return Err(QueryError::SqlTooLong.into());
                    }
                    let before = query::instruction_counter();
                    let prepared = connection.prepare(sql);
                    metrics.prepare_instructions = query::delta(before);
                    state.check().map_err(classify)?;
                    let mut statement = prepared?;
                    if statement.is_read_only() {
                        return Err(UpdateError::UnsupportedStatement);
                    }
                    let execution = (|| -> Result<(), UpdateError> {
                        let before = query::instruction_counter();
                        let bound = if execution_limit.is_some() {
                            statement.query_with_bind_check(values, || state.check())
                        } else {
                            statement.query(values).map_err(QueryError::from)
                        };
                        metrics.bind_instructions = query::delta(before);
                        state.check().map_err(classify)?;
                        let mut rows = bound.map_err(classify)?;
                        let before = query::instruction_counter();
                        let stepped = (|| -> Result<(), UpdateError> {
                            loop {
                                state.check().map_err(classify)?;
                                let row = rows.next_row();
                                state.check().map_err(classify)?;
                                if row?.is_none() {
                                    break;
                                }
                            }
                            Ok(())
                        })();
                        metrics.execute_instructions = query::delta(before);
                        stepped
                    })();
                    metrics.statement = statement.metrics();
                    execution?;
                    drop(statement);
                    let changes = if schema_change.get() {
                        0
                    } else {
                        connection.changes()
                    };
                    drop(authorizer); // Allow only our own transaction completion.
                    state.check().map_err(classify)?;
                    let before = query::instruction_counter();
                    let committed = connection.execute_batch("COMMIT");
                    metrics.commit_instructions = query::delta(before);
                    state.check().map_err(classify)?;
                    committed?;
                    let stats = overlay::write_stats();
                    if let Some(bytes) = stats.exceeded {
                        return Err(UpdateError::BudgetExceeded(if bytes {
                            UpdateBudgetKind::ChangedBytes
                        } else {
                            UpdateBudgetKind::DirtyPages
                        }));
                    }
                    // Once stable writes start, return success even if the soft
                    // total is exceeded. Publication failures retain existing IC
                    // trap semantics; never interrupt halfway through publication.
                    drop(guard);
                    state.check().map_err(classify)?;
                    let committed_bytes = overlay_dirty_bytes();
                    metrics.peak_dirty_pages = stats.dirty_pages;
                    metrics.overlay_peak_bytes = stats.peak_bytes;
                    stable_blob::commit_update().map_err(DbError::from)?;
                    connection_guard.published = true;
                    metrics.committed_bytes = committed_bytes;
                    metrics.commit_instructions = query::delta(before);
                    Ok(changes)
                })()
            })();
            let stats = overlay::write_stats();
            metrics.peak_dirty_pages = metrics.peak_dirty_pages.max(stats.dirty_pages);
            metrics.overlay_peak_bytes = metrics.overlay_peak_bytes.max(stats.peak_bytes);
            let result = if let Some(bytes) = stats.exceeded {
                Err(UpdateError::BudgetExceeded(if bytes {
                    UpdateBudgetKind::ChangedBytes
                } else {
                    UpdateBudgetKind::DirtyPages
                }))
            } else {
                result
            };
            Ok(result)
        })
        .unwrap_or_else(|error| Err(error.into()));
    metrics.instructions = query::delta(start);
    metrics.committed_over_soft_limit = result.is_ok()
        && budget
            .max_instructions
            .zip(metrics.instructions)
            .is_some_and(|(limit, used)| used > limit);
    UpdateReport { result, metrics }
}

fn overlay_dirty_bytes() -> u64 {
    overlay::dirty_page_count().saturating_mul(u64::from(crate::config::SQLITE_PAGE_SIZE))
}
