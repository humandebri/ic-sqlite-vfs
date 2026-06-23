# Migrating from ic-sqlite or ic-rusqlite

`ic-sqlite-vfs` is not a drop-in `rusqlite` wrapper. It stores SQLite directly
inside a MemoryManager-compatible virtual memory, then exposes database access
through synchronous `Db::update` and `Db::query` closures.

## Initialization

Choose one stable `MemoryId` for SQLite and keep it unchanged across upgrades.
Do not reuse that `MemoryId` for another stable structure. For migrations from
`ic-rusqlite`, this is the fresh destination slot, not direct reuse of the old
stable-memory image.

```rust
use ic_sqlite_vfs::{Db, DefaultMemoryImpl, MemoryId, MemoryManager};
use std::cell::RefCell;

const SQLITE_MEMORY_ID: MemoryId = MemoryId::new(120);

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        Db::init(manager.borrow().get(SQLITE_MEMORY_ID)).unwrap();
    });
}
```

Call `init_db()` from both `#[ic_cdk::init]` and `#[ic_cdk::post_upgrade]`,
then run `Db::migrate(...)`.

## Multiple databases / mount IDs

`DbHandle::init(memory)` supports several independent SQLite images in one Wasm
instance. Give each handle its own dedicated `MemoryId` and keep that mapping
stable across upgrades.

This is not a replacement for a mount-id or filename namespace inside one
SQLite image. Existing `index DB + user DB slots` designs should map each slot
to a stable `MemoryId`, then recreate the same handles after upgrade from the
stored slot catalog.

The consuming canister owns that slot catalog. Store
`archive_id -> slot_id -> MemoryId` in stable state, choose the usable
`MemoryId` range before launch, and never move an existing slot to another
`MemoryId`. The bundled MemoryManager-compatible layout allows `MemoryId`
values `0..=254`; `255` is reserved internally and `MemoryId::new(255)` is
invalid.
Index DBs, catalog DBs, metadata stores, and reserved ranges consume the same
finite ID space.

Use `MemoryId::new(120)` as the default fresh destination slot anchor when
preserving `ic-rusqlite` mounted DB conventions. Do not call `Db::init` on the
old `ic-rusqlite` `120` image. The current release has no supported direct
migration/import path from `ic-rusqlite`. For per-slot archives, reserve `120`
for the index/default DB or a future migration destination, then allocate
neighboring slot IDs from an application-owned range. Record the choice in the
slot catalog; the crate does not reserve the range.

Per-slot archives are therefore a bounded design. If no free slot remains,
reject archive creation. Reuse deleted slots only when the application tracks a
generation number or tombstone state so stale archive references cannot open the
new occupant's SQLite image.

Archive and restore are not provided by the current public API. A future
bounded staging design must operate on one logical SQLite image at a time and
preserve the slot catalog. Do not pack several independent SQLite databases
into one `VirtualMemory`, and do not depend on a forked `u16` `MemoryId` layout
for this crate.

## `ic-rusqlite` migration boundary

`ic-rusqlite` stores a raw SQLite file image. `ic-sqlite-vfs` v8 stores an
`ICSQLITE` superblock at offset `0` and the SQLite image after `64KiB`.
Direct stable-memory upgrade cannot reinterpret one layout as the other.

Current release behavior:

1. Keep the old canister running if the data must remain accessible.
2. Do not install v8 over the old raw SQLite stable-memory image.
3. Use a fresh selected virtual memory for new v8 deployments.
4. Wait for bounded staging import support before attempting direct migration.

If the selected memory already contains `SQLite format 3\0` or any other
non-`ICSQLITE` bytes, `Db::init` returns
`StableMemoryError::ForeignStableMemoryImage` and preserves the existing bytes.

## Access Pattern

Old `with_connection` style:

```rust
with_connection(|connection| {
    connection.execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
})
```

`ic-sqlite-vfs` update:

```rust
Db::update(|connection| {
    connection.execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
})
```

Old `rusqlite::Connection` read style:

```rust
let value = connection.query_row(
    "SELECT value FROM kv WHERE key = ?1",
    params![key],
    |row| row.get(0),
)?;
```

`ic-sqlite-vfs` query:

```rust
let value = Db::query(|connection| {
    connection.query_row(
        "SELECT value FROM kv WHERE key = ?1",
        params![key],
        |row| row.get::<String>(0),
    )
})?;
```

## API Mapping

| Existing pattern | `ic-sqlite-vfs` |
|---|---|
| `with_connection(|conn| ...)` for writes | `Db::update(|conn| ...)` |
| `with_connection(|conn| ...)` for reads | `Db::query(|conn| ...)` |
| `conn.execute(sql, params)` | `conn.execute(sql, params)` |
| `conn.execute_batch(sql)` | `conn.execute_batch(sql)` |
| `conn.changes()` | `conn.changes()` |
| `conn.query_row(sql, params, f)` | `conn.query_row(sql, params, f)` |
| `conn.query_map(sql, params, f)` | `conn.query_map(sql, params, f)` returning `Vec<T>` |
| `conn.query_all(sql, params, f)` | Same collected `Vec<T>` behavior as `query_map` |
| named params | `named_params![":name" => value]` |

`query_map` intentionally returns `Vec<T>`, not a `rusqlite` iterator. This
keeps statement lifetimes inside one synchronous canister message.

## Rules

- Do not hold a connection outside the closure.
- Do not `await` inside `Db::update` or `Db::query`.
- Put writes in `Db::update`; query connections are read-only/query-only.
- Do not call mutation helpers from inside `Db::query`; they return
  `DbError::ReadConnectionInUse`.
- Use migrations for schema changes:

`Db::migrate` records applied versions. Treat each SQL body as one strictly
increasing versioned step, not as an idempotent `IF NOT EXISTS` initializer.
Migration SQL must be static trusted SQL; do not build it from user input.

```rust
use ic_sqlite_vfs::db::migrate::Migration;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];
```
