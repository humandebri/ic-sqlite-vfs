# Verus layout proofs

このディレクトリは SQLite stable memory layout の算術モデルだけを検証する。
production code へ `verus!` macro は混入しない。

対象:

- page count
- segment count
- segment index
- root table byte count
- import offset overflow 境界
- MemoryManager bucket address / max managed pages
- overflow-safe virtual segment containment

対象外:

- SQLite C core
- FFI
- IC stable memory API
- checksum 実装
- thread-local read cache
- `Rc<RefCell<_>>` runtime borrowing
- `ic0.stable64_*` system API behavior
- SQLite C core behavior

実行:

```sh
mkdir -p target/verus
verus --crate-type=lib --out-dir target/verus proofs/verus/layout_math.rs
verus --crate-type=lib --out-dir target/verus proofs/verus/memory_manager_layout.rs
```

`scripts/sqlite-critical-check.sh` は `VERUS` 環境変数または `verus` command が存在する場合だけ proof を実行する。
