# ic-sqlite-vfs

[![crates.io](https://img.shields.io/crates/v/ic-sqlite-vfs.svg)](https://crates.io/crates/ic-sqlite-vfs)
[![docs.rs](https://docs.rs/ic-sqlite-vfs/badge.svg)](https://docs.rs/ic-sqlite-vfs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

SQLite VFS for the Internet Computer that stores the SQLite database image
directly in IC stable memory.

```text
SQLite pager
  -> custom sqlite3_vfs: icstable
  -> ic0.stable64_read / ic0.stable64_write
  -> stable memory pages
```

`ic-sqlite-vfs` does not use POSIX files, WASI files, stable-fs, or wasi2ic.
SQLite sees `/main.db`; the VFS maps logical SQLite pages to immutable stable
memory pages through a segmented page table.

## Status

Initial public release: `0.1.0`.

The core VFS, transaction facade, import/export flow, and upgrade persistence
tests are in place. This project has not promised compatibility for deployed
canisters yet. `0.x` releases may introduce breaking changes.

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
SQLite -> sqlite3_io_methods xRead/xWrite -> stable memory
```

## Design

```text
Canister API
  -> Rust DB facade
  -> SQLite C core / libsqlite3-sys
  -> custom sqlite3_vfs: icstable
  -> IC stable memory pages
```

Stable memory layout:

```text
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
- query calls open read-only connections
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
ic-sqlite-vfs = { version = "0.1.2", default-features = false }
```

Consumers must build bundled SQLite with `SQLITE_OS_OTHER=1` and a C compiler
that can emit `wasm32-unknown-unknown` compatible objects. Install the reference
support files into the consuming canister workspace:

```sh
scripts/install-build-support.sh /path/to/canister-workspace
```

The installer adds `.cargo/config.toml`, `scripts/wasm32-unknown-unknown-cc.sh`,
and `c/include/*`. It refuses to overwrite existing files unless `--force` is
passed.

See [docs/BUILD_SETUP.md](docs/BUILD_SETUP.md) for details and rationale.

Minimal canister pattern:

```rust
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::{params, Db};

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];

#[ic_cdk::init]
fn init() {
    Db::migrate(MIGRATIONS).unwrap();
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

Measured locally on 2026-05-13 with `icp` local network. The main metric is IC
instructions from `ic_cdk::api::performance_counter(0)`.

The benchmark harness lives in `benchmarks/kv-canister` and can be run with:

```sh
scripts/bench-kv-local.sh 1000
```

The benchmark project uses local gateway port `8001` to avoid clashing with the
default `icp` local network on `8000`.

KV workload, 1000 rows:

| Workload | ic-sqlite-vfs | wasi2ic + ic-rusqlite | Result |
|---|---:|---:|---:|
| reset + insert | 18.27M | 125.49M | 6.9x fewer instructions |
| repeated point read | 23.99M | 18.71M | API-loop dominated |
| bulk read | 4.11M | not measured | use for multi-key reads |
| insert/update | 20.37M | 127.97M | 6.3x fewer instructions |

Repeated point reads execute one SQLite statement per key inside the canister.
They mostly measure bind/reset/step wrapper overhead, not stable-memory I/O.
Prefer batched reads such as `kv_get_many` or a single SQL query when one
canister method needs many keys.

Memory after the 1000-row run:

| Implementation | Canister memory |
|---|---:|
| ic-sqlite-vfs | 4.28 MB |
| wasi2ic + ic-rusqlite | 89.64 MB |

Wasm size:

| Implementation | Wasm |
|---|---:|
| ic-sqlite-vfs reference canister | 1.84 MB |
| wasi2ic KV benchmark canister | 3.00 MB |

The instruction gap comes from removing WASI fd emulation and mapping SQLite
pager I/O directly to stable memory offsets.

Native performance probe, measured locally on 2026-05-13 with
`cargo test --test sqlite_perf_probe -- --ignored --nocapture`:

| Rows | batch insert | single update after insert | refresh checksum | db_size |
|---:|---:|---:|---:|---:|
| 100 | 1 ms | 1 ms | 0 ms | 64 KiB |
| 1,000 | 5 ms | 1 ms | 1 ms | 144 KiB |
| 10,000 | 35 ms | 1 ms | 6 ms | 672 KiB |
| 20,000 | 65 ms | 1 ms | 13 ms | 1.25 MiB |
| 100,000 | 355 ms | 1 ms | 67 ms | 6.09 MiB |

For 20,000 rows in the same native probe:

| Workload | elapsed | xRead calls | stable data reads | root hit/miss | segment hit/miss | superblock loads |
|---|---:|---:|---:|---:|---:|---:|
| indexed point reads | 207 ms | 20,080 | 20,080 | 20,079 / 1 | 20,079 / 1 | 40,085 |
| `LIKE '%stable%'` scan | 7 ms | 56 | 56 | 56 / 0 | 56 / 0 | 62 |
| full logical export | 0 ms | 0 | 80 | 80 / 0 | 80 / 0 | 3 |

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
