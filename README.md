# ic-sqlite-vfs

SQLite VFS for the Internet Computer that stores the SQLite database image
directly in IC stable memory.

```text
SQLite pager
  -> custom sqlite3_vfs: icstable
  -> ic0.stable64_read / ic0.stable64_write
  -> stable memory pages
```

`ic-sqlite-vfs` does not use POSIX files, WASI files, stable-fs, or wasi2ic.
SQLite sees `/main.db`; the canister stores it as a contiguous byte range in
stable memory.

## Status

Alpha. The core VFS, transaction facade, import/export flow, and upgrade
persistence tests are in place. The API is still expected to change before a
stable crates.io release.

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
offset 64KiB..       SQLite database image
```

The superblock stores magic, schema version, logical DB size, transaction id,
checksum, import state, and flags. The SQLite database header starts at byte 0
of the DB image region.

## SQLite Settings

The reference facade uses:

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

Durability is based on IC message atomicity, not `fsync`.

Rules:

- one update call is one DB transaction
- no `await` inside a transaction
- query calls open read-only connections
- WAL is disabled
- journal and temp data stay in heap memory
- only the DB image is stored in stable memory

## Usage

Library users should disable default features. The `canister-api` feature is
only for this repository's reference canister.

```toml
[dependencies]
ic-sqlite-vfs = { version = "0.1", default-features = false }
```

Consumers must build bundled SQLite with `SQLITE_OS_OTHER=1` and a C compiler
that can emit `wasm32-unknown-unknown` compatible objects. This repository uses
[.cargo/config.toml](.cargo/config.toml) and
[scripts/wasm32-unknown-unknown-cc.sh](scripts/wasm32-unknown-unknown-cc.sh) as
the reference setup. Copy the same settings into the consuming canister
workspace until this crate ships a dedicated build helper.

Minimal canister pattern:

```rust
use ic_sqlite_vfs::db::migrate::Migration;
use ic_sqlite_vfs::Db;

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
        connection.execute_with_texts(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            &[key.as_str(), value.as_str()],
        )
    })
    .map_err(|error| error.to_string())
}

#[ic_cdk::query]
fn get(key: String) -> Result<Option<String>, String> {
    Db::query(|connection| {
        connection.query_optional_string_with_text(
            "SELECT value FROM kv WHERE key = ?1",
            &key,
        )
    })
    .map_err(|error| error.to_string())
}
```

For repeated operations in one message, reuse a prepared statement:

```rust
Db::query(|connection| {
    let mut statement = connection.prepare("SELECT value FROM kv WHERE key = ?1")?;
    let value = statement.query_optional_string_with_text("alpha")?;
    Ok(value)
})
```

## Reference Canister

This repository includes a reference canister behind the `canister-api` feature.

```sh
icp build
icp network start -d
icp deploy
```

The reference canister exposes:

- `kv_put`, `kv_get`, `kv_count`
- `db_meta`
- `db_integrity_check`
- `db_checksum`
- `db_export_chunk`
- `db_begin_import`, `db_import_chunk`, `db_finish_import`

Admin import/export and integrity methods require the caller to be a controller.

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

KV workload, 1000 rows:

| Workload | ic-sqlite-vfs | wasi2ic + ic-rusqlite | Result |
|---|---:|---:|---:|
| reset + insert | 21.27M | 149.36M | 7.0x fewer instructions |
| point read | 22.63M | 44.53M | 2.0x fewer instructions |
| insert/update | 23.78M | 172.56M | 7.3x fewer instructions |

Memory after the 1000-row run:

| Implementation | Canister memory |
|---|---:|
| ic-sqlite-vfs | 3.58 MB |
| wasi2ic + ic-rusqlite | 89.64 MB |

Wasm size:

| Implementation | Wasm |
|---|---:|
| ic-sqlite-vfs reference canister | 1.84 MB |
| wasi2ic KV benchmark canister | 3.00 MB |

Wall-clock measurements are included only as a local sanity check because they
include `icp canister call` process startup:

| Workload | ic-sqlite-vfs | wasi2ic + ic-rusqlite |
|---|---:|---:|
| reset + insert 1000 | 0.26s | 0.20s |
| point read 1000 | 0.04s | 0.06s |
| insert/update 1000 | 0.21s | 0.22s |

The instruction gap comes from removing WASI fd emulation and mapping SQLite
pager I/O directly to stable memory offsets.

## Tests

```sh
cargo fmt --check
bash scripts/check-no-await.sh
cargo test
icp build
npm run test:pocketic
```

Current coverage:

- VFS read/write/truncate/filesize behavior
- rollback on SQL error
- read-only query mode
- reusable prepared statements
- chunked export/import with checksum verification
- failed import preserving the existing database
- capacity and sparse write bounds
- fuzz-style deterministic operation sequences
- long-running transaction endurance
- PocketIC upgrade persistence
- wasm import audit: only `ic0.*`

## Operations

See [docs/OPERATIONS.md](docs/OPERATIONS.md) for transaction rules, import
recovery, capacity handling, and integrity checks.

See [docs/RELEASE.md](docs/RELEASE.md) for release gates.

## Limitations

- WAL is intentionally unsupported.
- mmap and SQLite shared-memory methods are not implemented.
- `VACUUM` should be treated as admin maintenance, not a normal API path.
- Transactions must not cross `await` boundaries.
- The stable memory layout should be considered unstable until a `1.0` release.

## License

Licensed under either MIT or Apache-2.0.
