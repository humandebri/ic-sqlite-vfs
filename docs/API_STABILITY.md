# API Stability

This file defines the active `2.x` compatibility contract. The current public
crate is `2.0.0`; production deployments should pin exact versions.

## Stability Contract

Starting with `2.0.0`, all rustdoc-visible public items under `src/` are part
of the `2.x` source compatibility contract. Additive public APIs are allowed.
Removing public items, changing public signatures, or changing documented
semantics requires a breaking major release.

`canister-api` is a reference canister implementation used by examples and
PocketIC tests. Generated DID compatibility and private Candid method names in
`src/api.rs` are not part of the `2.x` compatibility contract. The
`canister-api-test-failpoints` feature is also outside the stable contract.

The frozen public Rust surface is tracked by
`docs/PUBLIC_API_2_0.snapshot` and checked by
`scripts/check-public-api-snapshot.sh`.

## Release Notes

`2.0.0` is a breaking stable-layout release:

- stable layout version `8` stores the SQLite image in place after
  `db_base_offset`
- v6 segmented page-map images are not opened by direct upgrade; export with
  the old version and import into a fresh v8 canister instead
- public Rust constant `PAGE_MAP_LAYOUT_VERSION` was removed and replaced by
  `CURRENT_LAYOUT_VERSION`
- `stable_blob::invalidate_read_cache()` was removed because v8 no longer has
  a page-table read cache
- `compact()` remains public but is a no-op for v8

Applications initialize a `MemoryManager<DefaultMemoryImpl>` from this crate,
choose a dedicated `MemoryId`, and pass the resulting
`VirtualMemory<DefaultMemoryImpl>` to `Db::init(memory)`.
The crate does not reserve a `MemoryId`; the application must choose one,
persist that choice across upgrades, and never reuse it for another stable
structure.

`Db::update`, `Db::query`, migration, checksum, import/export, and compact APIs
require successful `Db::init(memory)` first. Calling them before initialization
returns `DbError::StableMemoryNotInitialized`. Calling `Db::init(memory)` twice
in the same Wasm instance returns `DbError::StableMemoryAlreadyInitialized`.
`DbHandle::init(memory)` is the multi-database API for advanced users that need
several independent SQLite images in one Wasm instance. Each handle must use a
dedicated `MemoryId`; `DbHandle` does not provide a mount-id namespace inside a
single SQLite image. Registering the same `MemoryId` twice in one Wasm instance
returns `StableMemoryError::MemoryAlreadyRegistered`.

Fresh initialization only occurs when the selected virtual memory has size
`0`. Non-empty memory without the `ICSQLITE` superblock returns
`StableMemoryError::ForeignStableMemoryImage` and is not rewritten. This
protects existing `ic-rusqlite` raw SQLite images whose first bytes are
`SQLite format 3\0`; migrate those images through export/import only.

The bundled MemoryManager-compatible `MemoryId` is `u8`-backed. Values
`0..=254` are usable by applications. `MemoryId::new(255)` is invalid because
`255` is the internal unallocated-bucket marker. This crate keeps the
`ic-stable-structures` 0.7 MemoryManager stable-memory layout. If an existing
MemoryManager-compatible image is corrupt, internally inconsistent, or
physically truncated, initialization rejects it by panic/trap rather than a
recoverable DB migration.

`MemoryManager::init_strict(memory)` is the safe initializer for callers that
want non-empty / non-MemoryManager memory and invalid layouts as typed errors
instead of implicit new-layout initialization or panic.

## Upgrade Contract

`2.0.0` is a breaking stable-layout release. It does not read `1.x` / v6
page-map images in place. A canister that still contains a v6 image must export
the logical SQLite image with the old version, install or create a fresh v8
canister, then import the exported image.

Compatibility gates verify logical export/import from old fixtures into the
current canister. Direct upgrade success from `0.2.x` or `1.x` stable memory is
not a `2.x` requirement.

## Stable Layout

The `2.0` stable-memory image uses:

```text
selected virtual memory:
  offset 0..64KiB      superblock
  offset 64KiB..       in-place SQLite image bytes
```

The superblock stores logical size, transaction id, last verified checksum,
import state, and flags. Page-table fields remain encoded for metadata
compatibility, but normal v8 operation sets `page_table_offset = 0`.

Logical SQLite page `n` lives at:

```text
db_base_offset + n * SQLITE_PAGE_SIZE
```

`checksum` is last verified checksum metadata. It is not a durability boundary.
Update commits use a heap write overlay, write dirty logical pages to fixed
stable-memory offsets, advance `last_tx_id`, and may set `checksum_stale`.
Truncate stores whole-page tail ranges as v8 zero-mask extents. Dirty writes
materialize pages by removing those ranges, and non-page-boundary truncate
physically zeroes only the boundary page tail. This prevents stale physical
bytes from becoming logical data without reintroducing page tables.
`db_refresh_checksum` and `db_refresh_checksum_chunk` are the only operations
that persistently update the stored checksum after a normal commit.

`compact()` is retained as a public API but is a no-op for v8. `db_meta`
continues to expose `orphan_bytes_estimate` as high-water slack observation;
`compact_recommended` is always `false` for v8.

## 2.0 Compatibility Contract

The `2.0` line freezes these surfaces for all `2.x` releases:

- stable memory superblock magic `ICSQLITE`, superblock version `8`, encoded
  little-endian field offsets, and meta-checksum semantics
- layout version `8`: selected virtual memory offset `0..64KiB` is the
  superblock, and logical SQLite pages live at fixed offsets after
  `db_base_offset`
- v8 zero-extent metadata masks truncated whole pages until a future write
  makes them logical data again
- v6 direct initialization returns
  `StableMemoryError::UnsupportedLayoutVersion(6)`
- bundled MemoryManager-compatible layout for `MemoryId` values `0..=254`,
  matching the `ic-stable-structures` 0.7 memory-manager layout
- logical export format: byte-for-byte SQLite image over `0..db_size`
- import/export checksum format: FNV-1a 64-bit checksum over the logical SQLite
  image bytes in ascending offset order
- public Rust API: top-level re-exports, `config`, `db`, and documented
  `Db`/`DbHandle` facade types. Low-level `read_metrics`, `sqlite_vfs`, and
  `stable` modules are not public compatibility surface
- downstream build path: `default-features = false` with `sqlite-precompiled`

`2.x` releases may add APIs and metadata fields, but must keep existing `2.0`
stable-memory images readable. A future layout change must either keep reading
version `8` in place or provide a documented migration that reads version `8`
and publishes the new layout atomically.
