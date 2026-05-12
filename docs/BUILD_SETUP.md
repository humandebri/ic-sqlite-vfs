# Build Setup

`ic-sqlite-vfs` builds bundled SQLite with `SQLITE_OS_OTHER=1`.

Cargo builds dependencies before the consuming crate's build script runs, so the
consumer must provide the SQLite C flags and wasm C compiler configuration at
the workspace level.

## Install Support Files

From this repository:

```sh
scripts/install-build-support.sh /path/to/canister-workspace
```

This installs:

- `.cargo/config.toml`
- `scripts/wasm32-unknown-unknown-cc.sh`
- `c/include/*`

The installer refuses to overwrite existing files. Pass `--force` only when the
workspace is disposable or already reviewed.

## Required Tools

- Rust target: `wasm32-unknown-unknown`
- `wasm32-wasi-clang`, or set `WASI_CC` to a compatible clang
- `wasi-libc` headers/libs available to that compiler

## Why This Exists

`libsqlite3-sys` compiles SQLite as a dependency. A dependency cannot reliably
configure the compiler environment for itself from the downstream canister's
build script. The workspace-level Cargo config is therefore required for `0.x`
releases.

The planned `1.0` path is a dedicated build helper that removes this manual
workspace setup.
