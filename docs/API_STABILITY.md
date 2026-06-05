# API Stability

This file defines the active `1.x` compatibility contract. The current public
crate is `1.0.0`; production deployments should pin exact versions.

## Stability Contract

Starting with `1.0.0`, all rustdoc-visible public items under `src/` are part
of the `1.x` source compatibility contract. Additive public APIs are allowed.
Removing public items, changing public signatures, or changing documented
semantics requires a breaking major release.

`canister-api` is a reference canister implementation used by examples and
PocketIC tests. Generated DID compatibility and private Candid method names in
`src/api.rs` are not part of the `1.x` compatibility contract. The
`canister-api-test-failpoints` feature is also outside the stable contract.

The frozen public Rust surface is tracked by
`docs/PUBLIC_API_1_0.snapshot` and checked by
`scripts/check-public-api-snapshot.sh`.

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

`MemoryManager::grow()` writes allocation-table owner bytes before committing
the header that records the new allocated bucket count and virtual memory size.
On the Internet Computer, successful message execution commits Wasm and stable
memory together, while a trap rolls back changes made during that message
execution. Under that execution model, a grow state with the allocation table
updated but the old header still committed is not persistently observable.
Images imported, restored, or manually constructed with that mismatch are
treated as corrupt MemoryManager-compatible images and rejected during
initialization.

`MemoryManager::init_strict(memory)` is the additive safe initializer for
callers that want non-empty / non-MemoryManager memory and invalid layouts as
typed errors instead of implicit new-layout initialization or panic.
MemoryManager metadata writes use stable memory grow internally; metadata grow
failure during initialization or header/allocation-table writes is still a
panic/trap boundary.

`stable::memory::read()` does not grow stable memory. Reads beyond the current
capacity return `StableMemoryError::ReadOutOfBounds`.

The multi-database API is part of the `1.0` public Rust surface.

`0.2.0` also adds `sqlite-precompiled`, which links the vendored
`wasm32-unknown-unknown` SQLite archive without downstream build-support files.

## Upgrade Contract

Canister upgrades, logical export, logical import, and post-import application
migration are release-gated by PocketIC tests. The compatibility gate keeps the
`0.2.2` fixture as the pre-`1.0` backward-compatibility baseline and the
`1.0.0` fixture as the published `1.x` baseline for later releases.

## Stable Layout

The `1.0` stable-memory image uses:

```text
selected virtual memory:
  offset 0..64KiB      superblock
  offset 64KiB..       immutable SQLite pages and page tables
```

The superblock stores the active page table offset, logical page count, and
logical size. The SQLite database image itself is still portable through the
chunked export API.

`checksum` is last verified checksum metadata. It is not a durability boundary.
Update commits use a heap write overlay, publish dirty pages and a new page
table through the superblock, advance `last_tx_id`, and may set
`checksum_stale`. `db_refresh_checksum` and `db_refresh_checksum_chunk` are
the only operations that persistently update the stored checksum after a
normal commit.

## 1.0 Compatibility Contract

The `1.0` line freezes these surfaces for all `1.x` releases:

- stable memory superblock magic `ICSQLITE`, superblock version `6`, encoded
  little-endian field offsets, and meta-checksum semantics
- page-map layout version `6`: selected virtual memory offset `0..64KiB` is the
  superblock, and logical SQLite pages are resolved through the segmented page
  table rooted by the superblock
- bundled MemoryManager-compatible layout for `MemoryId` values `0..=254`,
  matching the `ic-stable-structures` 0.7 memory-manager layout
- logical export format: byte-for-byte SQLite image over `0..db_size`
- import/export checksum format: FNV-1a 64-bit checksum over the logical SQLite
  image bytes in ascending offset order
- public Rust API: every rustdoc-visible public item under `src/`, including
  top-level re-exports, `api`, `config`, `db`, `read_metrics`, `sqlite_vfs`,
  and `stable`
- downstream build path: `default-features = false` with `sqlite-precompiled`

`1.x` releases may add APIs and metadata fields, but must keep existing `1.0`
stable-memory images readable. A future layout change must either keep reading
version `6` in place or provide a documented migration that reads version `6`
and publishes the new layout atomically.

Cross-version canister compatibility is release-gated by
`npm run test:pocketic:compat`. That test creates SQLite images with the
`0.2.2` and `1.0.0` compatibility canisters, upgrades them to the current
canister, exports the images, imports them into current canisters, and reruns the
current migration path. Import is a raw SQLite image restore; application
migrations must run after importing an older application schema.
