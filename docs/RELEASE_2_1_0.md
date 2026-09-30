# ic-sqlite-vfs 2.1.0

This release adds query cost reports, execution budgets and migration SQL
checksums to SQLite storage on the Internet Computer. The bundled SQLite is
updated to 3.53.4, and the locked anyhow dependency is updated to 1.0.103 with
upstream fixes.

## Added

- `query_profiled` exposes per-statement counters and instruction measurements.
  The optional `query-metrics` feature enables detailed VFS read counters.
- `query_with_budget` bounds rows, result payload bytes and SQL/value sizes,
  with an IC instruction soft limit enforced through SQLite progress callbacks.
- `update_with_budget` runs a dedicated transaction with instruction and dirty
  overlay limits, reserving instructions for commit. Budget errors roll back the
  transaction. Successful commits report when commit exceeds the soft limit.
- Newly applied migrations store SHA-256 of the exact UTF-8 SQL. All supplied
  known checksums are checked before pending migration SQL executes. A mismatch
  returns an error instead of rerunning or silently skipping changed SQL.
- `Db::adopt_migration_checksums` and the `DbHandle` equivalent explicitly record
  a reviewed baseline for already-applied legacy versions without running SQL.

## Compatibility and upgrade requirements

The 2.x stable layout is version 8. This release does **not** directly open 1.x
or v6 stable images. Do not upgrade such a deployment in place without a
separately validated migration/recovery plan. The checksum addition itself
changes neither the 2.x stable layout nor MemoryId ownership.

Version-only migration history remains unverified and is skipped for
compatibility. Checksums are not inferred automatically. Explicit adoption
asserts trust in the supplied SQL; it cannot prove which SQL ran historically.
Keep the complete migration list, including applied versions. Whitespace and
comment changes also change the digest. When a known digest differs, the
application must handle the migration error; trapping in `post_upgrade`
rejects the upgrade and preserves the installed image.

Instruction limits are soft per-operation limits, not hard message limits or
cycle budgets. Callers must leave room for cleanup and response encoding.
Authorization and tenant isolation remain application responsibilities.

Checksum metadata adds 32 digest bytes per version plus SQLite table/page
overhead. Repeated calls do not append duplicate records. Reported virtual
memory sizes exclude MemoryManager bucket slack; measure raw stable memory
separately when estimating storage costs.

Chunked import/export, backfill orchestration and alternate SQLite build
profiles are not included. See [API contract](API_STABILITY.md) and
[checksum validation](MIGRATION_CHECKSUM_VALIDATION.md) for details and measured
performance impact.
