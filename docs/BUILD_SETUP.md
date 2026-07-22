# Build Setup

`sqlite-precompiled` is the recommended canister build path. It links the
vendored `wasm32-unknown-unknown` SQLite archive and needs no workspace-level C
compiler setup.
The vendored archive is built with FTS5, UTC date/time functions, and JSON
functions enabled.

```toml
ic-sqlite-vfs = { version = "2.0.0", default-features = false, features = ["sqlite-precompiled"] }
```

When disabling default features, explicitly enable either `sqlite-precompiled`
or `sqlite-bundled`.

## Rebuilding SQLite

Maintainers can regenerate the vendored archive with:

```sh
scripts/build-sqlite-precompiled.sh
```

The script uses `wasm32-wasi-clang` by default. Set `WASI_CC` or `LLVM_AR` when
using non-standard tool locations.
Both `sqlite-precompiled` regeneration and `sqlite-bundled` use
`vendor/sqlite/build-flags.txt` as the SQLite definition source. Canister Wasm
builds also use `vendor/sqlite/wasm-compiler-flags.txt`, which selects `-O3`
and WebAssembly SIMD with `-msimd128`. Native test builds do not receive the
Wasm-only compiler flags.

The resulting canister module requires a WebAssembly runtime with `simd128`
support. Current ICP and the pinned PocketIC test environment support it.
Archive regeneration also writes
`vendor/sqlite/wasm32-unknown-unknown/lib/libsqlite3.build-metadata`. Run
`scripts/check-sqlite-precompiled.sh` to verify that the checked-in archive
matches the SQLite source, definition flags, Wasm compiler flags, shim headers,
and generation script recorded by that metadata.
Local time modifiers are intentionally omitted; use UTC date/time functions in
canister SQL.

## Legacy Bundled Path

`sqlite-bundled` compiles `vendor/sqlite/src/sqlite3.c` during the Cargo build.
It is useful for local development and archive regeneration checks, but
downstream canister workspaces then need a C compiler that can emit
`wasm32-unknown-unknown` compatible objects.

The old support installer is still available:

```sh
scripts/install-build-support.sh /path/to/canister-workspace
```

It installs `.cargo/config.toml`, `scripts/wasm32-unknown-unknown-cc.sh`, and
`c/include/*`. Prefer `sqlite-precompiled` unless rebuilding SQLite is required.
