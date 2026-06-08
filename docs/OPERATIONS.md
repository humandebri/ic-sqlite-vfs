# Operations

## Transaction Rule

Canister `init` and `post_upgrade` must initialize one
`MemoryManager<DefaultMemoryImpl>`, choose the SQLite `MemoryId`, and call
`Db::init(memory)` before any DB operation. The same `MemoryId` must be used for
the lifetime of the deployed canister.

Public update APIs must be synchronous. `Db::update` accepts only
`FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>` and does not accept a
future. `await` inside a transaction is forbidden.

CI rejects `.await` and `async fn` under `src` through
`scripts/check-no-await.sh`.

SQLite `xWrite` and `xTruncate` calls inside a transaction do not write
directly to stable memory. They accumulate page-sized changes in a heap overlay.
After SQLite `COMMIT` succeeds, the crate appends dirty pages and a new page
table, then updates the superblock active page table offset. Normal `Err`
returns, SQL rollback, and panic do not change the active page table.

## Query Policy

This library does not analyze arbitrary SQL for index use or query complexity.
Its guarantee stops at synchronous transactions, `await` rejection, read-only
and query-only query connections, and admin checksum/import/export consistency.

The consuming canister must design `WHERE` clauses, indexes, `LIMIT`,
pagination, and input length caps for each application query. The reference
canister does not expose an arbitrary SQL endpoint.

Treat these SQL patterns as unsafe for public APIs unless tightly bounded:

- full scan
- huge result set
- `LIKE '%foo%'`
- join-heavy query
- unbounded `ORDER BY`
- huge `BLOB`
- filter without a primary key or index

Fetching many rows in one call can exceed the IC cycles or instruction limit
and trap. Public APIs should prefer point reads, indexed range reads, and
explicit page sizes.

## Storage Choice

Prefer `ic-stable-structures` when a key-value store, BTree, or append-only log
is enough. That shape is simpler and avoids SQL planner and VFS risk.

Choose SQLite only when schema migrations, compound indexes, relational
constraints, or ad-hoc queries are worth the extra storage complexity.

## Per-slot Databases

In a per-archive or per-slot layout, one slot maps to one dedicated `MemoryId`,
one `DbHandle`, and one SQLite image. The consuming canister reserves
`MemoryId` values in the `0..=254` range. `255` is the internal
unallocated-bucket marker and must not be used.

`MemoryId::new(120)` is the default slot anchor that matches the
`ic-rusqlite` default mounted DB. A single-DB canister can use `120` directly.
A per-slot archive can put the migrated/default slot at `120`, or reserve `120`
for the index/default DB and allocate adjacent application-owned IDs for
archive slots.

The slot catalog belongs to the consuming canister. Store
`archive_id -> slot_id -> MemoryId` in stable state, then rebuild the same
handle set from the catalog in `init` and `post_upgrade`. Never change the
`MemoryId` of an existing slot.

When slots are exhausted, reject new archive creation. Do not move an existing
DB to another `MemoryId` to free a slot. If deleted slots are reused, store a
generation or tombstone so stale archive references cannot open the new
occupant.

Run admin operations per slot. `integrity_check`, checksum refresh,
import/export, and compact must name the target `DbHandle`; backups should save
the catalog snapshot and the matching image for each slot.

## Migration Failure Recovery

Migrations run through `Db::migrate` in one transaction. On failure, SQL is
rolled back and `superblock.schema_version` is not advanced.

Recovery steps:

1. Check `schema_version`, `checksum`, and `checksum_stale` through `db_meta`.
2. Confirm that `db_integrity_check` returns `ok`.
3. Fix the failed migration SQL.
4. Upgrade the same canister, then rerun migration from an admin update after
   `Db::init(memory)` in `post_upgrade`.
5. Confirm that `db_meta.schema_version` advanced to the target version.

## Import

Import must run through controller-only APIs and must require checksum match.

1. On the source, run `db_refresh_checksum_chunk(max_bytes)` until
   `complete == true`.
2. Read `db_size`, `checksum`, and `last_tx_id` from `db_meta`.
3. Read every byte through `db_export_chunk(offset, len)`.
4. After export, confirm that `db_meta.last_tx_id` did not change.
5. On the destination, call `db_begin_import(total_size, expected_checksum)`.
6. Call `db_import_chunk` in order from offset 0.
7. `db_finish_import` verifies checksum and clears the import flag.

During import, the SQLite VFS rejects `/main.db` open, so normal DB APIs fail.
On checksum mismatch, the staging area is discarded, the existing DB is kept,
and the import flag is cleared. To abort an unfinished import, the controller
calls `db_cancel_import`.

Import restores the raw SQLite image, not application schema intent. If the
target canister expects newer migrations than the imported image has, run the
application migration path after import before exposing new-schema methods. The
reference compatibility test verifies this by importing `0.2.2` and `1.0.0`
images, then running the current `post_upgrade` migration path.

Reference controller guard:

```rust
fn require_controller() -> Result<(), String> {
    let caller = ic_cdk::api::msg_caller();
    if ic_cdk::api::is_controller(&caller) {
        Ok(())
    } else {
        Err("caller is not a controller".to_string())
    }
}

#[ic_cdk::query]
fn db_export_chunk(offset: u64, len: u64) -> Result<Vec<u8>, String> {
    require_controller()?;
    ic_sqlite_vfs::Db::export_chunk(offset, len).map_err(|error| error.to_string())
}

#[ic_cdk::update]
fn db_begin_import(total_size: u64, checksum: u64) -> Result<(), String> {
    require_controller()?;
    ic_sqlite_vfs::Db::begin_import(total_size, checksum)
        .map_err(|error| error.to_string())
}

#[ic_cdk::update]
fn db_compact() -> Result<(), String> {
    require_controller()?;
    ic_sqlite_vfs::Db::compact().map_err(|error| error.to_string())
}
```

## Capacity

If growing the selected `VirtualMemory` fails, the error includes
`current_pages` and `required_pages`. The caller should not retry blindly; check
capacity limits, remaining cycles, and chunk size.

Normal commits publish safely by appending dirty pages, dirty segment tables,
and a new root table before updating the superblock. Capacity growth for small
updates is roughly proportional to the number of dirty pages and dirty
segments. When `db_meta.compact_recommended == true`, a controller can run
`db_compact`.

## Integrity

Admin monitoring should check these values periodically:

- `db_integrity_check == "ok"`
- `db_meta.importing == false`
- `db_meta.checksum_stale == false` or a controller checksum refresh job is in
  progress
- `db_meta.checksum_refreshing == false`
- `db_meta.orphan_ratio_basis_points`
- `db_meta.compact_recommended`

`db_meta.checksum` is the last verified checksum. `checksum_stale == true` can
be normal after updates. When a fresh verified checksum is required, a
controller should run `db_refresh_checksum_chunk` to completion.
