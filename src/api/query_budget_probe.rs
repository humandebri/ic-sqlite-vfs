//! Fixed SQL workload for PocketIC tests; never built into the production facade.
use crate::db::{QueryBudget, QueryReport};
use crate::{params, Db};
use candid::CandidType;
use serde::Deserialize;

#[derive(CandidType, Deserialize)]
pub(crate) struct BudgetProbeReport {
    error: Option<String>,
    instructions: Option<u64>,
    prepare_instructions: Option<u64>,
    bind_instructions: Option<u64>,
    execute_instructions: Option<u64>,
    vm_steps: u32,
    result_rows: u64,
    result_bytes: u64,
    recovery: i64,
    stable_read_bytes: Option<u64>,
}

#[ic_cdk::update]
fn db_test_query_budget(
    iterations: u32,
    instruction_budget: Option<u64>,
    max_rows: u64,
    max_bytes: u64,
    aggregate: bool,
) -> Result<BudgetProbeReport, String> {
    super::require_controller()?;
    if iterations == 0 || iterations > 1_000_000 {
        return Err("iterations must be 1..=1000000".into());
    }
    let sql = if aggregate {
        "WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x < ?1) SELECT sum(x) FROM seq"
    } else {
        "WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x < ?1) SELECT x FROM seq"
    };
    let report: QueryReport<i64> = Db::query_with_budget(
        sql,
        params![i64::from(iterations)],
        QueryBudget {
            max_instructions: instruction_budget,
            max_rows,
            max_result_bytes: max_bytes,
            progress_interval: 100,
            ..QueryBudget::default()
        },
        |row| row.get(0),
    );
    finish_report(report)
}

#[ic_cdk::update]
fn db_test_query_bind_budget() -> Result<BudgetProbeReport, String> {
    super::require_controller()?;
    Db::query(|_| Ok(())).map_err(super::error_text)?;
    let blob = vec![0u8; 64 * 1024];
    let values: Vec<&dyn crate::db::ToSql> = vec![&blob; 1000];
    let report = Db::query_with_budget(
        "SELECT ?1000",
        &values,
        QueryBudget {
            max_instructions: Some(10_000_000),
            ..QueryBudget::default()
        },
        |row| row.get::<Vec<u8>>(0),
    );
    finish_report(report)
}

fn finish_report<T>(report: QueryReport<T>) -> Result<BudgetProbeReport, String> {
    // Same cached connection, same message: verifies that the interrupt handler
    // and connection limits do not leak into the subsequent ordinary query.
    let recovery = Db::query(|c| c.query_scalar::<i64>("SELECT COUNT(*) FROM kv", params![]))
        .map_err(super::error_text)?;
    Ok(BudgetProbeReport {
        error: report.result.err().map(|error| error.to_string()),
        instructions: report.metrics.instructions,
        prepare_instructions: report.metrics.prepare_instructions,
        bind_instructions: report.metrics.bind_instructions,
        execute_instructions: report.metrics.execute_instructions,
        vm_steps: report.metrics.statement.vm_steps,
        result_rows: report.metrics.result_rows,
        result_bytes: report.metrics.result_bytes,
        recovery,
        stable_read_bytes: report.metrics.vfs.map(|v| v.stable_data_read_bytes),
    })
}

#[derive(CandidType, Deserialize)]
pub(crate) struct QueryOverheadReport {
    plain_instructions: u64,
    profiled_instructions: u64,
    budgeted_instructions: u64,
}

#[ic_cdk::update]
fn db_test_query_overhead(iterations: u32) -> Result<QueryOverheadReport, String> {
    super::require_controller()?;
    if iterations == 0 || iterations > 100_000 {
        return Err("iterations must be 1..=100000".into());
    }
    let sql = "WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x < ?1) SELECT x FROM seq";
    // Warm connection acquisition; every path still prepares a fresh statement.
    Db::query(|_| Ok(())).map_err(super::error_text)?;
    let start = ic_cdk::api::performance_counter(0);
    let plain =
        Db::query(|c| c.query_all(sql, params![i64::from(iterations)], |row| row.get::<i64>(0)))
            .map_err(super::error_text)?;
    let plain_instructions = ic_cdk::api::performance_counter(0) - start;
    let start = ic_cdk::api::performance_counter(0);
    let profiled = Db::query_profiled(sql, params![i64::from(iterations)], |row| row.get::<i64>(0));
    let profiled_instructions = ic_cdk::api::performance_counter(0) - start;
    let start = ic_cdk::api::performance_counter(0);
    let budgeted = Db::query_with_budget(
        sql,
        params![i64::from(iterations)],
        QueryBudget {
            max_instructions: Some(1_000_000_000),
            max_rows: u64::from(iterations),
            max_result_bytes: u64::from(iterations) * 8,
            ..QueryBudget::default()
        },
        |row| row.get::<i64>(0),
    );
    let budgeted_instructions = ic_cdk::api::performance_counter(0) - start;
    if plain != profiled.result.map_err(|e| e.to_string())?
        || plain != budgeted.result.map_err(|e| e.to_string())?
    {
        return Err("query paths produced different rows".into());
    }
    Ok(QueryOverheadReport {
        plain_instructions,
        profiled_instructions,
        budgeted_instructions,
    })
}

#[derive(CandidType, Deserialize)]
pub(crate) struct UpdateProbeReport {
    error: Option<String>,
    changed_rows: Option<u64>,
    instructions: Option<u64>,
    commit_instructions: Option<u64>,
    peak_dirty_pages: u64,
    overlay_peak_bytes: u64,
    committed_bytes: u64,
    committed_over_soft_limit: bool,
    stored_rows: i64,
}

#[ic_cdk::update]
fn db_test_update_budget(
    iterations: u32,
    blob_bytes: u32,
    instructions: Option<u64>,
    reserve: u64,
    max_pages: u64,
    max_bytes: u64,
) -> Result<UpdateProbeReport, String> {
    super::require_controller()?;
    if iterations == 0 || iterations > 100_000 || blob_bytes > 65536 {
        return Err("invalid fixed-workload size".into());
    }
    Db::update(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS budget_items(id INTEGER PRIMARY KEY, body BLOB); DELETE FROM budget_items"))
        .map_err(super::error_text)?;
    let report = Db::update_with_budget(
        "WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x < ?1) INSERT INTO budget_items SELECT x, zeroblob(?2) FROM seq",
        params![i64::from(iterations), i64::from(blob_bytes)],
        crate::db::UpdateBudget {
            max_instructions: instructions, commit_reserve_instructions: reserve,
            max_dirty_pages: max_pages, max_changed_bytes: max_bytes,
            progress_interval: 100, ..crate::db::UpdateBudget::default()
        },
    );
    let stored_rows = Db::query(|c| c.query_scalar("SELECT count(*) FROM budget_items", params![]))
        .map_err(super::error_text)?;
    let (changed_rows, error) = match report.result {
        Ok(rows) => (Some(rows), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(UpdateProbeReport {
        error,
        changed_rows,
        instructions: report.metrics.instructions,
        commit_instructions: report.metrics.commit_instructions,
        peak_dirty_pages: report.metrics.peak_dirty_pages,
        overlay_peak_bytes: report.metrics.overlay_peak_bytes,
        committed_bytes: report.metrics.committed_bytes,
        committed_over_soft_limit: report.metrics.committed_over_soft_limit,
        stored_rows,
    })
}
