# Operations

## Transaction Rule

Canister `init` and `post_upgrade` must initialize a stable `Memory` backend and
call `Db::init(memory)` before any DB operation. The default backend initializes
one `MemoryManager<DefaultMemoryImpl>`, chooses the SQLite `MemoryId`, and uses
the same `MemoryId` for the lifetime of the deployed canister.
For upgrade-sensitive deployments, initialize preexisting raw stable memory
through the strict MemoryManager path before selecting the SQLite virtual
memory; `Db::init` only protects the selected virtual memory from foreign
SQLite images.

Public update APIs must be synchronous. `Db::update` accepts only
`FnOnce(&mut UpdateConnection<'_>) -> Result<T, DbError>` and does not accept a
future. `await`, inter-canister calls, and `ic0.call_perform` inside a
transaction are forbidden.

CI rejects `.await`, `async fn`, `call_perform`, `ic_cdk::call`, and `call_raw`
under `src` through `scripts/check-no-await.sh`. This is a guardrail, not a
complete mechanical proof that no future code path can split commit across
message executions.

SQLite `xWrite` and `xTruncate` calls inside a transaction do not write
directly to stable memory. They accumulate page-sized changes in a heap overlay.
After SQLite `COMMIT` succeeds, the crate writes dirty logical pages to their
fixed stable-memory offsets, then updates the superblock. This ordering relies
on IC message execution atomicity and trap rollback: panic/trap must discard all
stable-memory writes from the current message. Normal `Err` returns, SQL
rollback, and panic do not change the active image under that runtime contract.

## Query Policy

This library does not analyze arbitrary SQL for index use or query complexity.
Its guarantee stops at synchronous transactions, `await` rejection, read-only
and query-only query connections, and admin checksum consistency.

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

SQLite `random()` and `randomblob()` are deterministic in this VFS so replicas
can agree on state. Do not use them for secrets, tokens, nonces, password reset
values, or cryptographic IDs. Fetch secure randomness outside SQLite and pass it
into SQL as a bound value.

Fetching many rows in one call can exceed the IC cycles or instruction limit
and trap. Public APIs should prefer point reads, indexed range reads, and
explicit page sizes.

## Storage Choice

Prefer `ic-stable-structures` when a key-value store, BTree, or append-only log
is enough. That shape is simpler and avoids SQL planner and VFS risk.

Choose SQLite only when schema migrations, compound indexes, relational
constraints, or ad-hoc queries are worth the extra storage complexity.

## Per-slot Databases

In a default-backend per-archive or per-slot layout, one slot maps to one
dedicated `MemoryId`, one `DbHandle`, and one SQLite image. The consuming
canister reserves `MemoryId` values in the `0..=254` range. `255` is the
internal unallocated-bucket marker and must not be used.

`MemoryId::new(120)` is the default fresh destination slot anchor matching the
`ic-rusqlite` default mounted DB. Do not call `Db::init` on an existing
`ic-rusqlite` `120` image. The current release has no direct migration path
from `ic-rusqlite`; import can return only after a bounded staging design. A
new single-DB canister can use `120` directly. A per-slot archive can reserve
`120` for the index/default DB and allocate adjacent application-owned IDs for
archive slots.

The slot catalog belongs to the consuming canister. Store
`archive_id -> slot_id -> MemoryId` in stable state, then rebuild the same
handle set from the catalog in `init` and `post_upgrade`. Never change the
`MemoryId` of an existing slot.

When slots are exhausted, reject new archive creation. Do not move an existing
DB to another `MemoryId` to free a slot. If deleted slots are reused, store a
generation or tombstone so stale archive references cannot open the new
occupant.

Pool allocators, extent maps, free lists, tombstones, trap injection,
fragmentation benchmarks, and integrity checkers are outside this crate's VFS
responsibility. Put that storage policy in a separate crate that implements
`Memory`, then pass per-DB handles to `DbHandle::init`.

Run admin operations per slot. `integrity_check` and checksum refresh must name
the target `DbHandle`.

## Migration Failure Recovery

Migrations run through `Db::migrate` in one transaction. Versions must be
strictly increasing, and SQL must be static trusted SQL rather than user-built
text. On failure, SQL is rolled back and `superblock.schema_version` is not
advanced.

Recovery steps:

1. Check `schema_version`, `checksum`, and `checksum_stale` through `db_meta`.
2. Confirm that `db_integrity_check` returns `ok`.
3. Fix the failed migration SQL.
4. Upgrade the same canister, then rerun migration from an admin update after
   `Db::init(memory)` in `post_upgrade`.
5. Confirm that `db_meta.schema_version` advanced to the target version.

## Import

Import/export/compact are not exposed by the Rust facade or reference canister
in the current release. There is no supported direct migration from
`ic-rusqlite` or older `ic-sqlite-vfs` layouts. Reintroduction requires a
bounded staging design with explicit capacity behavior.

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

#[ic_cdk::update]
fn db_refresh_checksum_chunk(max_bytes: u64) -> Result<ChecksumRefresh, String> {
    require_controller()?;
    ic_sqlite_vfs::Db::refresh_checksum_chunk(max_bytes)
        .map_err(|error| error.to_string())
}
```

## Capacity

If growing the selected `Memory` backend fails, the error includes
`current_pages` and `required_pages`. The caller should not retry blindly; check
capacity limits, remaining cycles, and chunk size.

Normal commits publish safely on IC-compatible runtimes by writing dirty pages
in place before updating the superblock. On runtimes without equivalent trap
rollback, a failure after page writes and before superblock publish can leave
new page bytes behind the old superblock. Truncate and sparse re-extend paths
zero-fill the logical gap with page writes and v8 zero-mask metadata, so stale
physical bytes never become logical SQLite data. `orphan_bytes_estimate` is a
high-water slack observation, not a promise that a public reclaim operation
exists. Stable memory does not shrink.

## Integrity

Admin monitoring should check these values periodically:

- `db_integrity_check == "ok"`
- `db_meta.checksum_stale == false` or a controller checksum refresh job is in
  progress
- `db_meta.checksum_refreshing == false`
- `db_meta.orphan_ratio_basis_points`

`db_meta.checksum` is the last verified checksum. `checksum_stale == true` can
be normal after updates. When a fresh verified checksum is required, a
controller should run `db_refresh_checksum_chunk` to completion.

Capacity monitoring should also track repeated-operation shape, not only single
updates:

- many one-page updates should not make `db_meta.allocated_bytes` grow when the
  image already fits in existing capacity
- grow/truncate loops should keep `db_base_offset` stable and
  `page_table_bytes == 0`
- pathological truncate loops can exhaust the fixed zero-extent metadata limit;
  that condition is an error, not a compact or append-only fallback trigger

For the benchmark canister, `scripts/bench-kv-local.sh <rows> <writes>` runs
`bench_capacity_growth_guard` after the normal read/write probes. The guard
fails if existing-capacity updates move `db_base_offset`, publish page table
metadata, grow stable pages, grow `allocated_bytes`, or call stable grow.

The operational interpretation is strict:

| Signal | Expected v8 behavior | Incident meaning |
| --- | --- | --- |
| `db_meta.allocated_bytes` during existing-capacity updates | unchanged | possible append-only regression or unexpected stable grow |
| `db_meta.page_table_bytes` | `0` | obsolete page-table layout leaked into v8 state |
| `db_meta.zero_extent_count` near the fixed metadata limit | bounded truncate metadata | pathological truncate sequence may start returning zero-extent-limit errors |
| `db_base_offset` after normal updates | unchanged | image base replacement escaped normal in-place commit |
