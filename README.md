# ic-sqlite-vfs

[![crates.io](https://img.shields.io/crates/v/ic-sqlite-vfs.svg)](https://crates.io/crates/ic-sqlite-vfs)
[![docs.rs](https://docs.rs/ic-sqlite-vfs/badge.svg)](https://docs.rs/ic-sqlite-vfs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

SQLite VFS for the Internet Computer that stores the SQLite database image
inside a byte-addressed stable-memory backend.

```text
SQLite pager
  -> custom sqlite3_vfs: icstable
  -> ic-sqlite-vfs Memory
  -> stable memory pages
```

`ic-sqlite-vfs` does not use POSIX files, WASI files, stable-fs, or wasi2ic.
SQLite sees `/main.db`; the VFS stores logical SQLite pages at fixed offsets in
stable memory.

## Status

Current public release: `2.0.0`.

The core VFS, transaction facade, checksum flow, and upgrade persistence tests
are in place. The repository carries the active `2.x` compatibility contract
and release gates; production deployments should pin exact versions.

`0.2.0` is the first public MemoryManager-backed release. The current crate
ships a minimal MemoryManager-compatible fork, so consumers no longer need a
direct `ic-stable-structures` dependency for SQLite storage.

See [docs/API_STABILITY.md](docs/API_STABILITY.md) for the `2.0` compatibility
contract.

## Why

SQLite already has the abstraction IC canisters need: `sqlite3_vfs` and
`sqlite3_io_methods`. A VFS receives reads and writes as `(offset, length)`.
That maps directly to IC stable memory.

wasi2ic is useful when an existing WASI program must run unchanged. For SQLite,
it adds a generic compatibility layer that SQLite does not need:

```text
SQLite -> WASI fd/read/write/seek -> wasi2ic -> file abstraction -> stable memory
```

This crate uses the shorter path:

```text
SQLite -> sqlite3_io_methods xRead/xWrite -> selected Memory backend
```

Why not wasi2ic? In the local KV benchmark, the direct VFS path uses 7.8x fewer
instructions for reset + insert and 6.5x fewer for insert/update.

## Stable Memory Ownership

`ic-sqlite-vfs` accepts any backend that implements `Memory`. The default path
uses `MemoryManager<DefaultMemoryImpl>` and a selected `MemoryId`; the
consuming canister chooses that `MemoryId` and must keep it stable forever.
The examples use `MemoryId::new(120)` as the fresh destination slot convention
matching `ic-rusqlite`'s default mounted DB memory ID.

Do not reuse that default-backend `MemoryId` for any other stable structure.
Inside the selected memory backend, this crate owns the full virtual address
space:

```text
virtual offset 0..64KiB      superblock
virtual offset 64KiB..       fresh/normal in-place SQLite image bytes
```

Fresh images start the SQLite bytes at `64KiB`.

The crate does not own the canister's raw stable memory. In the default path,
raw stable memory is managed by a `MemoryManager<DefaultMemoryImpl>` with the
same stable layout as the `ic-stable-structures` 0.7 MemoryManager.
Use `MemoryManager::init_strict` for upgrade-sensitive deployments. The
non-strict `MemoryManager::init` compatibility path may initialize MemoryManager
metadata on non-empty raw stable memory that does not already contain a
MemoryManager layout. Do not pass an existing raw stable memory image directly
to `MemoryManager::init`. `Db::init` protects only the selected virtual memory;
it does not validate or protect the whole raw backing memory.
If the selected virtual memory is non-empty and does not start with the
`ICSQLITE` superblock, initialization fails with
`StableMemoryError::ForeignStableMemoryImage` without rewriting bytes. Existing
`ic-rusqlite` raw SQLite images are not directly migrated by the current
release. A bounded staging import design is required before that path is
reintroduced.

> **Runtime durability contract:** write/update durability requires
> IC-compatible message atomicity and trap rollback. Transactions must not cross
> `await`, inter-canister calls, or `ic0.call_perform`. Native or custom stable
> memory backends are not crash-atomic unless they provide equivalent rollback.
> Without equivalent rollback, a failure after dirty page writes and before
> superblock publish can leave new page bytes behind the old superblock.

`Db::init(memory)` is a single global initialization point for one SQLite
database facade in the current Wasm instance. Calling it twice returns
`DbError::StableMemoryAlreadyInitialized`. Use `DbHandle::init(memory)` for
multiple simultaneous SQLite databases, with one distinct `Memory` identity per
handle. Each handle owns one independent SQLite image. This is not a mount-id
or filename namespace inside one image; SQLite still opens `/main.db` for each
handle, and the active context selects the backing `Memory`. Registering the
same memory identity twice in one Wasm instance returns
`StableMemoryError::MemoryAlreadyRegistered`.

The bundled MemoryManager-compatible layout supports `MemoryId` values
`0..=254`; `255` is reserved internally as the unallocated marker.
With the default MemoryManager backend, per-archive or per-slot databases are a
bounded design: one slot uses one `MemoryId`, one `DbHandle`, and one SQLite
image. A custom `Memory` implementation can provide a different storage policy,
for example a pool allocator, but that allocator and its integrity checks belong
outside this crate.

For compatibility-oriented layouts, use `MemoryId::new(120)` as the default
SQLite destination slot anchor. A new single-database canister can use `120`
directly. Do not point `Db::init` at an existing `ic-rusqlite` `120` image; use
`120` only for a fresh image. A per-slot archive can treat `120` as the default
slot, then allocate additional archive slots from an adjacent
application-owned range.

## Project Positioning

| Project | Layer | Storage model | Main value |
|---|---|---|---|
| `froghub-io/rusqlite` / `rusqlite-ic` | Rust `rusqlite` wrapper fork | Not the VFS/storage layer by itself | Lets `rusqlite` compile in IC-oriented Wasm builds |
| `froghub-io/ic-sqlite` | SDK using `rusqlite-ic` + VFS | Simple stable-memory-backed SQLite file | Early IC SQLite SDK |
| `wasm-forge/ic-rusqlite` | Convenience SDK | WASI/stable-fs via `wasi2ic` | Easy migration path and familiar `rusqlite` API |
| `humandebri/ic-sqlite-vfs` | SQLite VFS + DB facade | Direct SQLite image inside a chosen `Memory` backend | Lower overhead, no WASI, IC-native transaction model |

## Design

```text
Canister API
  -> Rust DB facade
  -> vendored SQLite C core
  -> custom sqlite3_vfs: icstable
  -> IC stable memory pages
```

Stable memory layout:

```text
selected virtual memory:
  offset 0..64KiB      superblock
  offset 64KiB..       fresh/normal in-place SQLite image bytes
```

The superblock stores magic, schema version, logical DB size, transaction id,
last verified checksum, import state, and flags. Import fields remain encoded
for stable-layout compatibility, but no public import API uses them in the
current release. The SQLite database header is logical page 0; logical page `n`
lives at `db_base_offset + n * SQLITE_PAGE_SIZE`. `db_base_offset` is normally
`64KiB` for a fresh image.

`checksum` is verification metadata. Normal update commits do not scan the full
DB image. They advance `last_tx_id` and set `checksum_stale`. In the reference
canister, a controller can run `db_refresh_checksum_chunk` until completion to
recompute the checksum, store it, and clear `checksum_stale`. The Rust facade
also provides `Db::refresh_checksum` for local or explicitly bounded use.

## SQLite Settings

Update connections use:

```sql
PRAGMA journal_mode = MEMORY;
PRAGMA synchronous = OFF;
PRAGMA temp_store = MEMORY;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA cache_size = -32768;
```

The first write against an empty image also applies `PRAGMA page_size = 16384`
before schema creation. Existing database images already carry their page size
in the SQLite header.

Read-only query connections use:

```sql
PRAGMA cache_size = -32768;
PRAGMA query_only = ON;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA temp_store = MEMORY;
```

Durability is based on IC message execution atomicity, trap rollback, and a heap
write overlay, not `fsync`. During an update call, VFS writes stay in heap
memory until SQLite `COMMIT` succeeds. The in-place commit then writes dirty
logical pages to their fixed stable-memory offsets before the final superblock
update publishes the new image.

This is a runtime contract. The layout is safe only when the whole commit runs
inside one IC-compatible message execution, trap/panic rolls back stable-memory
writes from that message, and commit performs no inter-canister call, `await`,
or `ic0.call_perform`. On runtimes without equivalent rollback semantics, a
failure after dirty page writes and before superblock publish can leave new page
bytes behind an old superblock.

Rules:

- one update call is one DB transaction
- no `await`, inter-canister call, or `ic0.call_perform` inside a transaction
- query calls use read-only, query-only connections
- WAL is disabled
- journal and temp data stay in heap memory
- only the DB image is stored in stable memory
- failed update calls return `Err` without changing the active image

Zero extents are fixed-size metadata for truncated whole pages. The v8 layout
stores at most `MAX_ZERO_EXTENTS = 1024` normalized ranges. Pathological
truncate/grow/sparse-write loops that exceed this limit return the recoverable
`StableMemoryError::ZeroExtentLimitExceeded` error; they do not panic and do
not fall back to append-only rewriting.

Query complexity is the consuming canister's responsibility. This crate does
not inspect arbitrary SQL for index use or planner cost. Public APIs should
expose bounded application queries with explicit `WHERE` clauses, indexes,
`LIMIT`/pagination, and input length caps. The reference canister intentionally
does not expose an arbitrary SQL endpoint.

Treat these patterns as unsafe for public canister APIs unless they are tightly
bounded and measured:

- full table scans and filters without a primary key or index
- huge result sets or unpaginated reads
- `LIKE '%foo%'`
- join-heavy queries
- unbounded `ORDER BY`
- huge `BLOB` values

SQLite `random()` and `randomblob()` are deterministic in this VFS so replicas
can agree on state. Do not use them for secrets, tokens, nonces, password reset
values, or cryptographic IDs. Fetch secure randomness outside SQLite and pass it
into SQL as a bound value.

An IC update or query has a finite instruction/cycles budget. Fetching many rows
in one call can exhaust that budget and trap even when SQLite itself is working
as designed. Prefer point reads, indexed range reads, and explicit page sizes.

## Why Not ic-stable-structures?

Use `ic-stable-structures` when the data model is a key-value store, BTree, or
append-only log. It is simpler, has fewer moving parts, and avoids SQL planner
costs.

Use this crate only when SQLite is worth the extra surface area: schema
migrations, compound indexes, relational constraints, or ad-hoc queries that
would otherwise become custom storage logic.

## Why Not rusqlite?

`rusqlite` is the usual choice for SQLite in normal Rust programs. This crate
is for IC canisters that store SQLite directly in stable memory.

The bundled SQLite build uses `SQLITE_THREADSAFE=0`, which removes SQLite's
internal mutex code. That fits the canister model because a `Db::update` or
`Db::query` closure runs synchronously inside one IC message and must not cross
an `await` boundary.

`rusqlite` assumes SQLite was built with thread-safety support before exposing
its safe Rust API. A `SQLITE_THREADSAFE=0` build violates that assumption, so
this crate uses a small SQLite C FFI facade instead of `rusqlite`.

Use this crate when SQLite must persist in IC stable memory. Use `rusqlite` for
ordinary Rust applications that store SQLite in regular files.

## Usage

Library users should disable default features. The `canister-api` feature is
only for this repository's reference canister.

```toml
[dependencies]
ic-sqlite-vfs = { version = "2.0.0", default-features = false, features = ["sqlite-precompiled"] }
```

`sqlite-precompiled` links the vendored `wasm32-unknown-unknown` SQLite archive
and does not require C compiler setup in the consuming canister workspace.
`sqlite-bundled` remains available for maintainers who need to rebuild SQLite.

See [docs/BUILD_SETUP.md](docs/BUILD_SETUP.md) for details and rationale.
For migration from `ic-sqlite` or `ic-rusqlite`, see
[docs/MIGRATING_FROM_IC_SQLITE.md](docs/MIGRATING_FROM_IC_SQLITE.md).

Minimal canister pattern:

`Db::migrate` records applied migration versions, so migration SQL should be a
strictly increasing, versioned step rather than an idempotent `IF NOT EXISTS`
schema initializer. Migration SQL must be static trusted SQL; do not build it
from user input. The migration registry stores only versions and does not
depend on SQLite date/time functions.

```rust
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::{params, Db, DefaultMemoryImpl, MemoryId, MemoryManager};
use std::cell::RefCell;

const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(
            MemoryManager::init_strict(DefaultMemoryImpl::default())
                .expect("stable memory must either be empty or use MemoryManager layout"),
        );
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];

#[ic_cdk::init]
fn init() {
    init_db();
    Db::migrate(MIGRATIONS).unwrap();
}

#[ic_cdk::post_upgrade]
fn post_upgrade() {
    init_db();
    Db::migrate(MIGRATIONS).unwrap();
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        Db::init(manager.borrow().get(SQLITE_MEMORY_ID)).unwrap();
    });
}

#[ic_cdk::update]
fn put(key: String, value: String) -> Result<(), String> {
    Db::update(|connection| {
        connection.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
    })
    .map_err(|error| error.to_string())
}

#[ic_cdk::query]
fn get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection.query_optional_scalar::<String>(
            "SELECT value FROM kv WHERE key = ?1",
            params![key],
        )
    })
    .map_err(|error| error.to_string())
}
```

For multiple SQLite databases in one Wasm instance, use `DbHandle::init(memory)`
with one distinct `Memory` identity per handle. The global `Db` facade remains a
single default database for compatibility. `DbHandle` models independent
SQLite images, not multiple mounted filenames inside one image. With the
default MemoryManager backend, a per-archive or per-slot design should keep a
stable external slot catalog and reject new archive creation when the chosen
`MemoryId` range is exhausted instead of moving existing slots. Pool allocators,
extent maps, free lists, tombstones, trap injection, fragmentation benchmarks,
and integrity checkers should live in a separate storage-policy crate and pass
their per-DB handle as a `Memory`.

For repeated operations in one message, reuse a prepared statement:

```rust
Db::query(|connection| {
    let mut statement = connection.prepare("SELECT value FROM kv WHERE key = ?1")?;
    let value = statement.query_optional_scalar::<String>(params!["alpha"])?;
    Ok(value)
})
```

Typed parameters and row reads are available for SQLite `TEXT`, `INTEGER`,
`REAL`, `BLOB`, and `NULL` values:

```rust
use ic_sqlite_vfs::db::NULL;
use ic_sqlite_vfs::params;

Db::update(|connection| {
    let blob = vec![0, 1, 2, 255];
    connection.execute(
        "INSERT INTO records(name, count, score, payload, note)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params!["alpha", 42_i64, 3.5_f64, blob, NULL],
    )
})?;

let values = Db::query(|connection| {
    connection.query_one(
        "SELECT name, count, score, payload, note FROM records WHERE name = ?1",
        params!["alpha"],
        |row| {
            Ok((
                row.get::<String>(0)?,
                row.get::<i64>(1)?,
                row.get::<f64>(2)?,
                row.get::<Vec<u8>>(3)?,
                row.get::<Option<String>>(4)?,
            ))
        },
    )
})?;
```

`Db::update` exposes savepoints only inside the update closure:

```rust
Db::update(|connection| {
    connection.execute("INSERT INTO logs(body) VALUES (?1)", params!["outer"])?;
    let inner = connection.savepoint(|connection| {
        connection.execute("INSERT INTO logs(body) VALUES (?1)", params!["inner"])?;
        connection.execute("INSERT INTO missing_table(value) VALUES (?1)", params![1_i64])
    });
    assert!(inner.is_err());
    Ok(())
})?;
```

## Reference Canister

This repository includes a reference canister behind the `canister-api` feature.

```sh
icp build
icp network start -d
icp deploy
```

The reference canister exposes:

- public `kv_get`, `kv_get_many`, `kv_get_note`
- controller-only `kv_put`, `kv_set_note`, `kv_count`
- `db_meta`
- `db_integrity_check`
- `db_checksum`
- `db_refresh_checksum`
- `db_refresh_checksum_chunk`

Admin checksum, integrity, writes, and count methods require the caller to be a
controller. `db_refresh_checksum` is present for DID compatibility but the
reference canister returns `Err`; use
`db_refresh_checksum_chunk` with bounded chunks instead.
In `db_meta`, `active_bytes` is the logical active payload
`SUPERBLOCK_SIZE + db_size`, not the physical end offset of the current image.

Import/export/compact are intentionally not exposed by the reference canister
or Rust facade in the current release. There is no direct `ic-rusqlite`
migration path. Migration/import can be reintroduced only after a bounded
staging design is implemented and tested.

The Rust facade still provides `Db::refresh_checksum` for local or explicitly
bounded use. Canister endpoints should prefer `db_refresh_checksum_chunk` so
checksum verification does not depend on one update message scanning the whole
DB image.

## Build Flags

The bundled SQLite build uses:

```text
SQLITE_OS_OTHER=1
SQLITE_THREADSAFE=0
SQLITE_ENABLE_FTS5
SQLITE_OMIT_LOCALTIME
SQLITE_OMIT_LOAD_EXTENSION
SQLITE_OMIT_SHARED_CACHE
SQLITE_OMIT_WAL
SQLITE_DEFAULT_MEMSTATUS=0
SQLITE_TEMP_STORE=3
```

The authoritative SQLite flag list is `vendor/sqlite/build-flags.txt`.
`sqlite-bundled` reads it during Cargo builds, and
`scripts/build-sqlite-precompiled.sh` uses it when regenerating the vendored
archive.
FTS5, UTC date/time functions, and JSON functions are enabled. Local time
modifiers are omitted because canister SQL should use UTC time.

`SQLITE_OS_OTHER=1` removes SQLite's default Unix/Windows/OS backends. This
crate provides `sqlite3_os_init()` and registers only the `icstable` VFS.

## Benchmarks

Measured locally on 2026-06-26 with PocketIC. The main metric is IC
instructions from `ic_cdk::api::performance_counter(0)`.

The benchmark harness lives in `benchmarks/kv-canister` and can be run with:

```sh
npm run test:pocketic:perf
```

The wasi2ic comparison harness lives in
`benchmarks/ic-rusqlite-kv-canister` and can be run with:

```sh
npm run test:pocketic:ic-rusqlite-perf
```

For manual local-network checks, run `scripts/bench-kv-local.sh 1000`.

KV workload, current PocketIC harness. Each workload runs in a fresh canister.
Read workloads use a warm read connection; point reads also warm the cached
point-read statement before instruction measurement. Instruction measurement
stops before `BenchReport` metadata collection.

| Workload | ic-sqlite-vfs | wasi2ic + ic-rusqlite | Result |
|---|---:|---:|---:|
| reset + insert, 1000 rows | 10.80M | 83.87M | 7.8x fewer instructions |
| insert only into empty table, 1000 rows | 10.29M | 83.27M | 8.1x fewer instructions |
| insert only into empty table, 5000 rows | 60.95M | 426.93M | 7.0x fewer instructions |
| append insert, 5000 existing + 1000 new | 13.55M | 86.21M | 6.4x fewer instructions |
| insert/update upsert, 1000 rows | 13.35M | 86.53M | 6.5x fewer instructions |
| update only by primary key, 1000 rows | 16.76M | 81.20M | 4.8x fewer instructions |
| update only by primary key, 5000 rows | 91.05M | 413.12M | 4.5x fewer instructions |
| point read, 1 key | 0.048M | 0.014M | wasi2ic lower on this harness |
| point read, 10 keys | 0.135M | 0.109M | wasi2ic lower on this harness |
| point read, 100 keys | 1.01M | 1.05M | ic-sqlite-vfs lower |
| point read, 1000 keys | 10.05M | 10.69M | ic-sqlite-vfs lower |
| bulk read ordered scan, 100 rows | 0.236M | 0.233M | wasi2ic lower on this harness |
| bulk read ordered scan, 1000 rows | 1.37M | 1.66M | ic-sqlite-vfs lower |
| bulk read ordered scan, 5000 rows | 6.46M | 7.99M | ic-sqlite-vfs lower |
| `WHERE key IN (...)`, 100 keys | 1.31M | 1.65M | ic-sqlite-vfs lower |
| `WHERE key IN (...)`, 1000 keys | 14.35M | 18.41M | ic-sqlite-vfs lower |

Additional read-helper checks from the same 1000-row PocketIC run:

| Workload | ic-sqlite-vfs |
|---|---:|
| repeated public helper point read, 1000 keys | 12.43M |
| repeated prepare-each point read, 1000 keys | 39.41M |

Additional limit-case checks from the same PocketIC run:

| Workload | ic-sqlite-vfs |
|---|---:|
| large blob insert/readback, 64 KiB | 0.97M |
| large blob insert/readback, 256 KiB | 2.26M |
| unbounded `ORDER BY`, 5000 rows | 66.48M |
| join, 2000 rows | 17.04M |
| repeated single-row update, 1000-row DB, 20 writes | 3.39M |
| repeated single-row update, 5000-row DB, 20 writes | 3.42M |

Repeated point reads execute one SQLite statement per key inside the canister.
They mostly measure bind/reset/step wrapper overhead, not stable-memory I/O.
Bulk reads and `IN` multi-gets reduce per-key SQL call overhead. These read
benchmarks sum TEXT lengths without allocating result strings.
The KV benchmark schema uses `WITHOUT ROWID`, so the primary key lookup and row
payload live in one SQLite B-tree instead of a rowid table plus a separate
unique index. The MemoryManager-backed path can coexist with other stable
structures under the application's memory layout.

`npm run test:pocketic:perf` also logs `bench_read_profile`, which breaks the
point-read path into open, prepare, key formatting, bind/reset, step, column
read, and VFS read metrics. `bench_get_many_in_profile` breaks the 1000-key
`WHERE key IN (...)` query into SQL build, key build, prepare, bind, row scan,
and VFS read metrics. It also logs `bench_write_profile`, which breaks the write
path into open, prepare, formatting, execute, VFS read/write, stable write,
stable grow, and commit phase metrics. `bench_growth_profile` breaks repeated
single-row updates into update open, formatting, prepare, execute, changes,
VFS/stable writes, stable grow, and commit metrics.
In the 1000-key `WHERE key IN (...)` profile, row scan is about 10.31M
instructions and SQLite prepare is about 3.52M instructions.
In the 1000-row upsert profile, SQLite statement execution dominates; update
open work is about 0.007M instructions after the write connection is warm.
In the 20-write growth profile, cached UPDATE statements reduce prepare work to
about 0.06M instructions total, and full-page overwrites avoid stable-memory
reads.
The wasi2ic numbers are measured with `ic-rusqlite 0.5.0`, `precompiled`,
`wasm32-wasip1`, and `wasi2ic 0.2.16`.

Write/delete capacity churn uses a separate `churn_bench` table. It seeds 5000
rows, then runs 100 cycles of 1000-row delete and 1000-row insert as separate
update calls. The row count returns to 5000 after each insert.

```sh
npm run test:pocketic:churn-capacity
```

| Implementation | Reset stable pages | Max stable pages | Stable grow | Reset stable bytes | Final stable bytes | Reset DB size | Max DB size | Final DB size | Final freelist pages |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| ic-sqlite-vfs | 6 | 6 | no | 393,216 | 393,216 | 294,912 | 311,296 | 311,296 | 0 |
| wasi2ic + ic-rusqlite | 1281 | 1281 | no | 83,951,616 | 83,951,616 | 237,568 | 241,664 | 241,664 | 1 |

Stable memory after the 1000-row clean reset:

| Implementation | Stable memory |
|---|---:|
| ic-sqlite-vfs | 196,608 bytes (0.19 MB) |
| wasi2ic + ic-rusqlite | 83,951,616 bytes (80.06 MB) |

Clean 5000-row DB stats:

| Implementation | DB size | SQLite page size | SQLite pages | Stable pages |
|---|---:|---:|---:|---:|
| ic-sqlite-vfs | 278,528 bytes | 16,384 bytes | 17 | 6 |
| wasi2ic + ic-rusqlite | 233,472 bytes | 4,096 bytes | 57 | 1281 |

Wasm size:

| Implementation | Wasm |
|---|---:|
| ic-sqlite-vfs reference canister | 1.62 MB |
| wasi2ic KV benchmark canister | 3.03 MB |

The instruction gap comes from removing WASI fd emulation and mapping SQLite
pager I/O directly to stable memory offsets.

Native performance probe, measured locally on 2026-05-19 with
`cargo test --test sqlite_perf_probe -- --ignored --nocapture`:

| Rows | batch insert | single update after insert | refresh checksum | db_size |
|---:|---:|---:|---:|---:|
| 100 | 0 ms | 0 ms | 0 ms | 64 KiB |
| 1,000 | 0 ms | 0 ms | 0 ms | 144 KiB |
| 10,000 | 12 ms | 0 ms | 2 ms | 672 KiB |
| 20,000 | 25 ms | 0 ms | 5 ms | 1.25 MiB |
| 100,000 | 129 ms | 0 ms | 25 ms | 6.09 MiB |

For 20,000 rows in the same native probe:

| Workload | elapsed | xRead calls | stable data reads | superblock loads |
|---|---:|---:|---:|---:|
| indexed point reads | 25 ms | 81 | 80 | 0 |
| `LIKE '%stable%'` scan | 2 ms | 0 | 0 | 0 |
| full logical export | 0 ms | 0 | 80 | 0 |

The write workload numbers exclude a full DB checksum scan from the commit
path. `db_refresh_checksum` and `db_refresh_checksum_chunk` are separate
controller verification operations.

## Tests

```sh
cargo fmt --check
bash scripts/check-no-await.sh
scripts/check-public-api-snapshot.sh
cargo test
cargo test --features canister-api
cargo +nightly fuzz run state_ops -- -max_total_time=30
cargo check --release
cargo check --release --target wasm32-unknown-unknown --no-default-features --features sqlite-precompiled
npm test
npm run build:wasm
icp build
npm run test:pocketic
cargo package
scripts/check-release-package.sh
wasm-objdump -x target/pocketic/ic_sqlite_vfs.wasm
```

Current coverage:

- VFS read/write/truncate/filesize behavior
- rollback on SQL error
- read-only query mode
- read and write connection cache invalidation around updates
- reusable statements and 32-entry LRU cached prepared statements
- capacity and sparse write bounds
- failpoints for overlay write, truncate, commit capacity, page write, and superblock publish
- in-place commit, truncate, and sparse-extend behavior
- stable write trap, grow failure, SQLite step error, and panic during update
- fuzz-style deterministic operation sequences
- property-based and libFuzzer state-machine operation sequences
- long-running transaction endurance
- PocketIC upgrade persistence
- wasm import audit: only `ic0.*`

## Operations

See [docs/OPERATIONS.md](docs/OPERATIONS.md) for transaction rules, capacity
handling, and integrity checks.

See [docs/RELEASE.md](docs/RELEASE.md) for release gates and publish-time
version/tag checks.

See [docs/API_STABILITY.md](docs/API_STABILITY.md) for the `2.0` compatibility
contract.

See [docs/BUILD_SETUP.md](docs/BUILD_SETUP.md) for consumer build setup.

## Limitations

- WAL is intentionally unsupported.
- mmap and SQLite shared-memory methods are not implemented.
- `VACUUM` should be treated as admin maintenance, not a normal API path.
- Transactions must not cross `await` boundaries.
- `canister-api` is a reference canister API, not the stable `2.x` Candid
  contract.

## License

Licensed under either MIT or Apache-2.0.
