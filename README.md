# ic-sqlite-vfs

[![crates.io](https://img.shields.io/crates/v/ic-sqlite-vfs.svg)](https://crates.io/crates/ic-sqlite-vfs)
[![docs.rs](https://docs.rs/ic-sqlite-vfs/badge.svg)](https://docs.rs/ic-sqlite-vfs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

SQLite VFS for the Internet Computer that stores the SQLite database image
inside a dedicated `ic-stable-structures` virtual memory.

```text
SQLite pager
  -> custom sqlite3_vfs: icstable
  -> ic-stable-structures VirtualMemory
  -> selected MemoryId pages
```

`ic-sqlite-vfs` does not use POSIX files, WASI files, stable-fs, or wasi2ic.
SQLite sees `/main.db`; the VFS maps logical SQLite pages to immutable stable
memory pages through a segmented page table.

## Status

Initial public release: `0.1.0`.

The core VFS, transaction facade, import/export flow, and upgrade persistence
tests are in place. This project has not promised compatibility for deployed
canisters yet. `0.x` releases may introduce breaking changes.

`0.2.0` is a breaking release: the crate no longer owns raw stable memory.
Applications must pass a dedicated `VirtualMemory<DefaultMemoryImpl>` from their
own `MemoryManager` to `Db::init(memory)`.

See [docs/API_STABILITY.md](docs/API_STABILITY.md) for the `0.x` compatibility
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
SQLite -> sqlite3_io_methods xRead/xWrite -> selected VirtualMemory
```

Why not wasi2ic? In the local KV benchmark, the direct VFS path uses 5.4x fewer
instructions for reset + insert and 4.6x fewer for insert/update.

## Stable Memory Ownership

`ic-sqlite-vfs` does not reserve a `MemoryId`. The consuming canister chooses
one `MemoryId` for SQLite and must keep it stable forever. The examples use
`MemoryId::new(120)`, matching `ic-rusqlite`'s default mounted DB memory ID.

Do not reuse that `MemoryId` for any other stable structure. Inside the selected
virtual memory, this crate owns the full virtual address space:

```text
virtual offset 0..64KiB      superblock
virtual offset 64KiB..       immutable SQLite pages, segment tables, and root tables
```

The crate does not own the canister's raw stable memory. Raw stable memory is
managed by the application's single `MemoryManager<DefaultMemoryImpl>`.

`Db::init(memory)` is a single global initialization point for one SQLite
database facade in the current Wasm instance. Calling it twice returns
`DbError::StableMemoryAlreadyInitialized`. Use `DbHandle::init(memory)` for
multiple simultaneous SQLite databases, with a distinct stable `MemoryId` per
handle.

## Project Positioning

| Project | Layer | Storage model | Main value |
|---|---|---|---|
| `froghub-io/rusqlite` / `rusqlite-ic` | Rust `rusqlite` wrapper fork | Not the VFS/storage layer by itself | Lets `rusqlite` compile in IC-oriented Wasm builds |
| `froghub-io/ic-sqlite` | SDK using `rusqlite-ic` + VFS | Simple stable-memory-backed SQLite file | Early IC SQLite SDK |
| `wasm-forge/ic-rusqlite` | Convenience SDK | WASI/stable-fs via `wasi2ic` | Easy migration path and familiar `rusqlite` API |
| `humandebri/ic-sqlite-vfs` | SQLite VFS + DB facade | Direct SQLite page map inside a chosen `VirtualMemory` | Lower overhead, no WASI, IC-native transaction model |

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
  offset 64KiB..       immutable SQLite pages, segment tables, and root tables
```

The superblock stores magic, schema version, logical DB size, transaction id,
active root table offset, active segment count, last verified checksum, import
state, and flags. The SQLite database header is logical page 0; the VFS resolves
logical pages through a root table and fixed 256-page segment tables.

`checksum` is verification metadata. Normal update commits do not scan the full
DB image. They advance `last_tx_id` and set `checksum_stale`. A controller can
run `db_refresh_checksum` to recompute the checksum, store it, and clear
`checksum_stale`.

## SQLite Settings

Update connections use:

```sql
PRAGMA page_size = 16384;
PRAGMA journal_mode = MEMORY;
PRAGMA synchronous = OFF;
PRAGMA temp_store = MEMORY;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA foreign_keys = ON;
PRAGMA cache_size = -32768;
PRAGMA busy_timeout = 0;
```

Read-only query connections use:

```sql
PRAGMA cache_size = -32768;
PRAGMA query_only = ON;
PRAGMA foreign_keys = ON;
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 0;
```

Durability is based on IC message atomicity and a heap write overlay, not
`fsync`. During an update call, VFS writes stay in heap memory until SQLite
`COMMIT` succeeds. Dirty logical pages and a new page table are appended to
stable memory, then made active by the final superblock update.

Rules:

- one update call is one DB transaction
- no `await` inside a transaction
- query calls use read-only, query-only connections
- WAL is disabled
- journal and temp data stay in heap memory
- only the DB image is stored in stable memory
- failed update calls return `Err` without changing the active page table

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
ic-sqlite-vfs = { version = "0.2.0", default-features = false, features = ["sqlite-precompiled"] }
ic-stable-structures = "0.7"
```

`sqlite-precompiled` links the vendored `wasm32-unknown-unknown` SQLite archive
and does not require C compiler setup in the consuming canister workspace.
`sqlite-bundled` remains available for maintainers who need to rebuild SQLite.

See [docs/BUILD_SETUP.md](docs/BUILD_SETUP.md) for details and rationale.
For migration from `ic-sqlite` or `ic-rusqlite`, see
[docs/MIGRATING_FROM_IC_SQLITE.md](docs/MIGRATING_FROM_IC_SQLITE.md).

Minimal canister pattern:

```rust
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::{params, Db};
use ic_stable_structures::{
    memory_manager::{MemoryId, MemoryManager},
    DefaultMemoryImpl,
};
use std::cell::RefCell;

const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS kv (
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
with one dedicated `MemoryId` per handle. The global `Db` facade remains a
single default database for compatibility.

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

- `kv_put`, `kv_get`, `kv_get_many`, `kv_count`
- `db_meta`
- `db_integrity_check`
- `db_checksum`
- `db_refresh_checksum`
- `db_refresh_checksum_chunk`
- `db_export_chunk`
- `db_begin_import`, `db_import_chunk`, `db_finish_import`, `db_cancel_import`
- `db_compact`

Admin import/export and integrity methods require the caller to be a controller.

Recommended export sequence:

1. run `db_refresh_checksum_chunk(max_bytes)` until it returns `complete = true`
2. read `db_meta` and record `db_size`, `checksum`, and `last_tx_id`
3. read all chunks with `db_export_chunk`
4. read `db_meta` again and confirm `last_tx_id` did not change

`db_refresh_checksum` still exists for small databases. Large databases should
use the chunked API so checksum verification does not depend on one update
message scanning the whole DB image.

## Build Flags

The bundled SQLite build uses:

```text
SQLITE_OS_OTHER=1
SQLITE_THREADSAFE=0
SQLITE_OMIT_LOAD_EXTENSION
SQLITE_OMIT_SHARED_CACHE
SQLITE_OMIT_WAL
SQLITE_DEFAULT_MEMSTATUS=0
SQLITE_TEMP_STORE=3
```

`SQLITE_OS_OTHER=1` removes SQLite's default Unix/Windows/OS backends. This
crate provides `sqlite3_os_init()` and registers only the `icstable` VFS.

## Benchmarks

Measured locally on 2026-05-14 with PocketIC. The main metric is IC
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
point-read statement before instruction measurement.

| Workload | ic-sqlite-vfs | wasi2ic + ic-rusqlite | Result |
|---|---:|---:|---:|
| reset + insert, 1000 rows | 16.06M | 86.51M | 5.4x fewer instructions |
| insert only into empty table, 1000 rows | 15.55M | 85.90M | 5.5x fewer instructions |
| insert only into empty table, 5000 rows | 84.35M | 440.58M | 5.2x fewer instructions |
| append insert, 5000 existing + 1000 new | 19.50M | 88.97M | 4.6x fewer instructions |
| insert/update upsert, 1000 rows | 19.26M | 89.49M | 4.6x fewer instructions |
| update only by primary key, 1000 rows | 22.38M | 83.58M | 3.7x fewer instructions |
| update only by primary key, 5000 rows | 115.88M | 425.26M | 3.7x fewer instructions |
| point read, 1 key | 0.057M | 0.029M | wasi2ic lower on this harness |
| point read, 10 keys | 0.187M | 0.145M | wasi2ic lower on this harness |
| point read, 100 keys | 1.49M | 1.29M | wasi2ic lower on this harness |
| point read, 1000 keys | 14.82M | 12.92M | wasi2ic lower on this harness |
| bulk read ordered scan, 100 rows | 0.264M | 0.245M | roughly equal |
| bulk read ordered scan, 1000 rows | 1.60M | 1.67M | ic-sqlite-vfs slightly lower |
| bulk read ordered scan, 5000 rows | 7.59M | 8.00M | ic-sqlite-vfs slightly lower |
| `WHERE key IN (...)`, 100 keys | 1.78M | 1.68M | wasi2ic lower on this harness |
| `WHERE key IN (...)`, 1000 keys | 19.85M | 18.53M | wasi2ic lower on this harness |

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
read, and VFS read metrics.
The wasi2ic numbers are measured with `ic-rusqlite 0.5.0`, `precompiled`,
`wasm32-wasip1`, and `wasi2ic 0.2.16`.

Stable memory after the 1000-row clean reset:

| Implementation | Stable memory |
|---|---:|
| ic-sqlite-vfs | 0.50 MB |
| wasi2ic + ic-rusqlite | 80.06 MB |

Clean 5000-row DB stats:

| Implementation | DB size | SQLite page size | SQLite pages | Stable pages |
|---|---:|---:|---:|---:|
| ic-sqlite-vfs | 278,528 bytes | 16,384 bytes | 17 | 10 |
| wasi2ic + ic-rusqlite | 233,472 bytes | 4,096 bytes | 57 | 1281 |

Wasm size:

| Implementation | Wasm |
|---|---:|
| ic-sqlite-vfs reference canister | 1.68 MB |
| wasi2ic KV benchmark canister | 3.00 MB |

The instruction gap comes from removing WASI fd emulation and mapping SQLite
pager I/O directly to stable memory offsets.

Native performance probe, measured locally on 2026-05-13 with
`cargo test --test sqlite_perf_probe -- --ignored --nocapture`:

| Rows | batch insert | single update after insert | refresh checksum | db_size |
|---:|---:|---:|---:|---:|
| 100 | 0 ms | 0 ms | 0 ms | 64 KiB |
| 1,000 | 1 ms | 0 ms | 0 ms | 144 KiB |
| 10,000 | 14 ms | 0 ms | 3 ms | 672 KiB |
| 20,000 | 31 ms | 0 ms | 6 ms | 1.25 MiB |
| 100,000 | 174 ms | 0 ms | 32 ms | 6.09 MiB |

For 20,000 rows in the same native probe:

| Workload | elapsed | xRead calls | stable data reads | root hit/miss | segment hit/miss | superblock loads |
|---|---:|---:|---:|---:|---:|---:|
| indexed point reads | 36 ms | 20,080 | 20,080 | 79 / 1 | 79 / 1 | 0 |
| `LIKE '%stable%'` scan | 2 ms | 56 | 56 | 54 / 0 | 54 / 0 | 0 |
| full logical export | 0 ms | 0 | 80 | 80 / 0 | 80 / 0 | 0 |

The write workload numbers exclude a full DB checksum scan from the commit
path. `db_refresh_checksum` and `db_refresh_checksum_chunk` are separate
controller verification operations.

## Tests

```sh
cargo fmt --check
bash scripts/check-no-await.sh
cargo test
cargo test --features canister-api
cargo build --target wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --features canister-api
icp build
npm run test:pocketic
cargo package --no-verify --offline
wasm-objdump -x target/wasm32-unknown-unknown/debug/ic_sqlite_vfs.wasm
```

Current coverage:

- VFS read/write/truncate/filesize behavior
- rollback on SQL error
- read-only query mode
- reusable statements and 32-entry LRU cached prepared statements
- chunked export/import with checksum verification
- failed import preserving the existing database
- capacity and sparse write bounds
- failpoints for overlay write, truncate, commit capacity, page write, page table write, and superblock publish
- segmented page-map commit and truncate behavior
- stable write trap, grow failure, SQLite step error, and panic during update
- fuzz-style deterministic operation sequences
- long-running transaction endurance
- PocketIC upgrade persistence
- wasm import audit: only `ic0.*`

## Operations

See [docs/OPERATIONS.md](docs/OPERATIONS.md) for transaction rules, import
recovery, capacity handling, and integrity checks.

See [docs/RELEASE.md](docs/RELEASE.md) for release gates.

See [docs/API_STABILITY.md](docs/API_STABILITY.md) for `0.x` compatibility.

See [docs/BUILD_SETUP.md](docs/BUILD_SETUP.md) for consumer build setup.

## Limitations

- WAL is intentionally unsupported.
- mmap and SQLite shared-memory methods are not implemented.
- `VACUUM` should be treated as admin maintenance, not a normal API path.
- Transactions must not cross `await` boundaries.
- The stable memory layout should be considered unstable until a `1.0` release.

## License

Licensed under either MIT or Apache-2.0.
