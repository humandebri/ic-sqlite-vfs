# Migrating from ic-sqlite or ic-rusqlite

`ic-sqlite-vfs` is not a drop-in `rusqlite` wrapper. It stores SQLite directly
inside a caller-provided `MemoryManager` virtual memory, then exposes database
access through synchronous `Db::update` and `Db::query` closures.

## Initialization

Choose one stable `MemoryId` for SQLite and keep it unchanged across upgrades.
Do not reuse that `MemoryId` for another stable structure.

```rust
use ic_sqlite_vfs::Db;
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

fn init_db() {
    MEMORY_MANAGER.with(|manager| {
        Db::init(manager.borrow().get(SQLITE_MEMORY_ID)).unwrap();
    });
}
```

Call `init_db()` from both `#[ic_cdk::init]` and `#[ic_cdk::post_upgrade]`,
then run `Db::migrate(...)`.

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

```rust
use ic_sqlite_vfs::db::migrate::Migration;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );",
}];
```
