# Verus layout proofs

このディレクトリは SQLite stable memory layout の算術モデルだけを検証する。
production code へ `verus!` macro は混入しない。

対象:

- page count
- import offset overflow 境界
- import chunk written_until 単調進行
- MemoryManager bucket address / max managed pages
- MemoryManager grow bucket/page arithmetic
- MemoryManager allocation-table invariants
- stable memory grow page arithmetic
- overflow-safe virtual segment containment
- FNV-1a chunk folding equivalence
- arbitrary chunk-list FNV folding equivalence
- in-place commit offset and truncate invariants
- import state-machine transitions
- abstract Superblock fixed-field encode/decode round-trip
- Superblock byte offset / field-width layout
- Superblock little-endian byte round-trip
- overlay page slicing arithmetic
- no-op compact invariants

対象外:

- SQLite C core
- FFI
- IC stable memory API
- checksum 実装
- `Rc<RefCell<_>>` runtime borrowing
- `ic0.stable64_*` system API behavior
- SQLite C core behavior

実行:

```sh
mkdir -p target/verus
verus --crate-type=lib --out-dir target/verus proofs/verus/layout_math.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/memory_manager_layout.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/memory_manager_grow.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/memory_capacity.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/checksum_fnv.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/in_place_commit.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/import_state_machine.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/memory_manager_allocation.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/superblock_encoding.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/superblock_byte_encoding.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/superblock_byte_roundtrip.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/overlay_model.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/compact_model.rs
```

`scripts/sqlite-critical-check.sh` は `VERUS` 環境変数または `verus` command が存在する場合だけ proof を実行する。
