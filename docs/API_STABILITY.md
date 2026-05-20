# API Stability

`0.2.0` is the first public MemoryManager-backed line.

## Stability Contract

The project has not promised compatibility for deployed canisters yet.

For all `0.x` releases, breaking changes may include:

- stable memory superblock fields
- DB image region layout
- `Db` facade signatures
- migration API
- import/export API
- checksum meaning and algorithm
- compile-time SQLite flags

Patch releases should remain bug-fix only when practical. Minor `0.x` releases
may still break API or layout. Production users should pin exact versions.

## Release Notes

Applications initialize a `MemoryManager<DefaultMemoryImpl>` from this crate,
choose a dedicated `MemoryId`, and pass the resulting
`VirtualMemory<DefaultMemoryImpl>` to `Db::init(memory)`.
The crate does not reserve a `MemoryId`; the application must choose one,
persist that choice across upgrades, and never reuse it for another stable
structure.

`Db::update`, `Db::query`, migration, checksum, import/export, and compact APIs
now require successful `Db::init(memory)` first. Calling them before
initialization returns `DbError::StableMemoryNotInitialized`.
Calling `Db::init(memory)` twice in the same Wasm instance returns
`DbError::StableMemoryAlreadyInitialized`. `DbHandle::init(memory)` is the
additive multi-database API for advanced users that need several independent
SQLite images in one Wasm instance. Each handle must use a dedicated
`MemoryId`; `DbHandle` does not provide a mount-id namespace inside a single
SQLite image.

The bundled MemoryManager-compatible `MemoryId` is `u8`-backed. Values
`0..=254` are usable by applications. `MemoryId::new(255)` is invalid because
`255` is the internal unallocated-bucket marker. Per-archive and per-slot
database designs must treat that as a hard capacity bound, including any
catalog, index, metadata, and reserved memories chosen by the application. This
crate keeps the `ic-stable-structures` 0.7 MemoryManager stable-memory layout.
If an existing MemoryManager-compatible image is corrupt, internally
inconsistent, or physically truncated, initialization rejects it by panic/trap
rather than returning a recoverable error.

`stable::memory::read()` does not grow stable memory. Reads beyond the current
capacity return `StableMemoryError::ReadOutOfBounds`.

The multi-database API is still covered by the `0.x` compatibility rules.
Production deployments should pin exact versions.

`0.2.0` also adds `sqlite-precompiled`, which links the vendored
`wasm32-unknown-unknown` SQLite archive without downstream build-support files.

## Upgrade Contract

Canister upgrades are tested for the same crate version.

Cross-version upgrade compatibility is not promised during `0.x`.

## Current Layout

`0.2.0` uses:

```text
selected virtual memory:
  offset 0..64KiB      superblock
  offset 64KiB..       immutable SQLite pages and page tables
```

The superblock stores the active page table offset, logical page count, and
logical size. The SQLite database image itself is still portable through the
chunked export API.

In `0.2.0`, `checksum` is last verified checksum metadata. It is not a
durability boundary. Update commits use a heap write overlay, publish dirty
pages and a new page table through the superblock, advance `last_tx_id`, and may set
`checksum_stale`. `db_refresh_checksum` and `db_refresh_checksum_chunk` are the
only operations that persistently update the stored checksum after a normal
commit.

## Road To Stable

The `1.0` line requires:

- frozen superblock format
- documented migration path for layout changes
- stable `Db` facade
- stable import/export checksum format
- build setup that does not require copying repository support files
