# API Stability

This file defines the active `2.x` compatibility contract. The current public
crate is `2.0.0`; production deployments should pin exact versions.

## Stability Contract

Starting with `2.0.0`, all rustdoc-visible public items under `src/` are part
of the `2.x` source compatibility contract. Additive public APIs are allowed.
Removing public items, changing public signatures, or changing documented
semantics requires a breaking major release.

`canister-api` is a reference canister implementation used by examples and
PocketIC tests. Its management method names are reference-only except for the
resource observation fields documented below: `db_meta.allocated_bytes`,
`db_meta.orphan_bytes_estimate`, `db_meta.page_table_bytes`,
`db_meta.zero_extent_count`, and `db_meta.compact_recommended` remain stable
monitoring semantics for `2.x`. Generated DID compatibility for unrelated
reference methods and the `canister-api-test-failpoints` feature are outside
the stable contract.

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

Fresh initialization only occurs when the selected MemoryManager virtual memory
has size `0`. Non-empty selected virtual memory without the `ICSQLITE`
superblock returns `StableMemoryError::ForeignStableMemoryImage` and is not
rewritten. This protection is scoped to the selected virtual memory; raw backing
memory that is not already a MemoryManager image should use strict
MemoryManager initialization in upgrade-sensitive code. The guard protects
existing `ic-rusqlite` raw SQLite images whose first bytes are
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
  offset 64KiB..       fresh/normal in-place SQLite image bytes
```

The superblock stores logical size, transaction id, last verified checksum,
import state, and flags. Page-table fields remain encoded for metadata
compatibility, but normal v8 operation sets `page_table_offset = 0`.
The resource state exposed through `db_meta` is part of the layout contract:
`db_size` is the logical image length, `db_base_offset` is the current physical
base, `active_bytes` is the logical active payload size
`SUPERBLOCK_SIZE + db_size`, `allocated_bytes` is the selected stable-memory
high-water mark, `orphan_bytes_estimate` is high-water slack outside
`active_bytes`, and `page_table_bytes` is always `0` for v8. `active_bytes` is
not the physical end offset `db_base_offset + db_size`; this matters after
import, because import can publish an appended `db_base_offset`.

Logical SQLite page `n` lives at:

```text
db_base_offset + n * SQLITE_PAGE_SIZE
```

`checksum` is last verified checksum metadata. It is not a durability boundary.
Update commits use a heap write overlay, write dirty logical pages to fixed
stable-memory offsets, advance `last_tx_id`, and may set `checksum_stale`.
Normal commits must not allocate a fresh append base, must keep
`db_base_offset` stable, must write dirty page `n` at
`db_base_offset + n * SQLITE_PAGE_SIZE`, and must publish `page_table_offset = 0`.
The in-place atomicity contract depends on IC message execution atomicity and
trap rollback. Dirty pages are written before the superblock is published; if a
panic/trap occurs after those writes, the implementation relies on IC-compatible
rollback of all stable-memory writes from the current message execution. A
normal commit must therefore perform no inter-canister call, `await`, or
`ic0.call_perform`. The grep-based await-free CI check is a guard for this
contract, not a standalone proof.
When the final image fits in existing stable-memory capacity, repeated normal
commits must not increase `allocated_bytes` or the high-water mark. When a
normal commit exceeds current capacity, it may grow stable memory only to the
stable-page-rounded end required by the final image and dirty page writes.
Truncate stores whole-page tail ranges as v8 zero-mask extents. Dirty writes
materialize pages by removing those ranges, and non-page-boundary truncate
physically zeroes only the boundary page tail. This prevents stale physical
bytes from becoming logical data without reintroducing page tables. Pathological
truncate sequences that exceed the fixed zero-extent metadata limit fail with a
zero-extent-limit error; they must not fall back to append-only rewriting.
`db_refresh_checksum` and `db_refresh_checksum_chunk` are the only operations
that persistently update the stored checksum after a normal commit.

`compact()` is retained as a public API but is a no-op for v8. Stable memory
does not shrink, so compact does not lower `allocated_bytes`, the high-water
mark, or `orphan_bytes_estimate`; it is not a reclaim operation in this layout.
`compact_recommended` is always `false` for v8.

The v8 resource proof obligations are:

| Operation | Forbidden behavior | Required resource state |
| --- | --- | --- |
| normal commit within existing capacity | allocate a fresh append base or call the append-base path | `db_base_offset` stable, `allocated_bytes` stable, high-water mark stable |
| normal commit requiring growth | grow beyond the stable-page-rounded required end | `db_base_offset` stable; growth is bounded by the computed physical write end |
| dirty page write | write dirty page data to a newly appended image copy | physical offset is `db_base_offset + page_no * SQLITE_PAGE_SIZE` |
| truncate | make truncated tail pages logically readable as stale physical data or fall back to append-only rewriting when zero extents are exhausted | inactive tail pages are masked by zero extents until a future write materializes them; zero-extent exhaustion returns an error |
| compact | claim reclaim or lower stable-memory high water | no-op; `allocated_bytes`, high-water mark, `orphan_bytes_estimate`, and `db_base_offset` stay unchanged |
| import or fresh base creation | reuse normal commit invariants while intentionally replacing the image base | append-base use is limited to import/fresh-base paths and must publish a complete logical image |
| failed write/grow/import step | publish partially updated layout metadata as committed state | committed superblock remains the authority; `page_table_offset == 0` for v8 committed state |

Design-review coverage for capacity-sensitive terms:

| Term | Contract location | Proof or regression coverage |
| --- | --- | --- |
| `append-only` | normal commit must not allocate a fresh append base | Verus in-place resource proof, repeated-update Rust/PocketIC/local guard |
| `compact` / `reclaim` | v8 compact is no-op, not reclaim | Verus compact no-op proof, repeated compact Rust test |
| `orphan` | high-water slack observation only | operations monitoring spec, compact non-reclaim regression |
| `fallback` | no alternate append path for normal commit | normal commit negative spec and capacity guard |
| `page_table` | v8 committed state publishes no page table | Verus current-layout proof and resource-shape tests |
| `base_offset` | normal commit preserves `db_base_offset` | Verus offset proof and local canister capacity guard |

These obligations do not prove the absence of all bugs. They define the finite
resource invariants that a `2.x` implementation must keep satisfying.
`docs/CAPACITY_GROWTH_PROOF.md` gives the HackMD-style proof that connects
these obligations to the Verus abstract models and the Rust/PocketIC/local
capacity guards.

Fresh images normally use `64KiB` as `db_base_offset`. Successful import
replaces the logical SQLite image by setting `db_base_offset` to the appended
`import_base_offset` and resets the superblock schema-version cache to `0`;
callers must run the configured migrations again to resynchronize the cache with
the imported image's migration table. Failed or cancelled import keeps the
committed image authoritative, but stable memory does not shrink, so staging
bytes may remain as high-water slack.

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
