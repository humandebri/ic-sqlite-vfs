# Verus layout proofs

このディレクトリは SQLite stable memory layout の算術モデルだけを検証する。
production code へ `verus!` macro は混入しない。

対象:

- page count
- import offset overflow 境界
- import chunk written_until 単調進行
- MemoryManager positive bucket-size address / max managed pages
- MemoryManager positive bucket-size grow bucket/page arithmetic
- MemoryManager allocation-table invariants
- stable memory grow page arithmetic
- overflow-safe virtual segment containment
- FNV-1a chunk folding equivalence
- arbitrary chunk-list FNV folding equivalence
- in-place commit offset and truncate invariants
- in-place zero-extent mask invariants
- in-place resource high-water invariants
- import state-machine transitions
- abstract Superblock fixed-field encode/decode round-trip
- Superblock byte offset / field-width layout
- Superblock little-endian byte round-trip
- overlay page slicing arithmetic
- no-op compact invariants

対象外:

- production Rust と抽象モデルの完全一致
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

`scripts/sqlite-critical-check.sh` は `VERUS` 環境変数、`verus` command、
`$HOME/.local/bin/verus`、`/opt/homebrew/bin/verus` の順で検出し、見つかった
場合は全 `proofs/verus/*.rs` を実行する。`VERUS_REQUIRED=1` の場合、Verus を
検出できなければ失敗する。

## Capacity proof mapping

`docs/CAPACITY_GROWTH_PROOF.md` の手証明は、次の抽象モデルと回帰テストに対応する。
IC message execution atomicity は外部公理であり、Verus 対象ではない。

| 手証明 | Verus proof | Regression coverage |
| --- | --- | --- |
| T3 dirty page fixed offset | `in_place_commit.rs` | stable blob dirty-offset tests |
| T4 existing-capacity no growth | `in_place_commit.rs` | Rust repeated update, PocketIC/local `bench_capacity_growth_guard` |
| T5 zero extent truncate | `in_place_commit.rs` normalized truncate model | zero extent limit, normalized merge, and truncate roundtrip tests |
| T6 compact no-op | `compact_model.rs` | compact resource no-op tests |
| T7 import exception / checksum mismatch | `import_state_machine.rs` | failed import and checksum mismatch tests |
