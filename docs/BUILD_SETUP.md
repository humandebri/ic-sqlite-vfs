# Build Setup

`sqlite-precompiled` is the recommended canister build path. It links the
vendored `wasm32-unknown-unknown` SQLite archive and needs no workspace-level C
compiler setup.
The vendored archive uses SQLite **3.53.4**, built with FTS5, UTC date/time
functions, and JSON functions enabled. This includes the upstream fixes for
CVE-2026-11822 / CVE-2026-11824 (fixed upstream in 3.53.2).

Source: `https://www.sqlite.org/2026/sqlite-amalgamation-3530400.zip`.
SHA3-256 of the official archive:
`628a44cfe82c66aed1ccbbe85a562d2e33ebe64b3288981ed76285612227934e`.
SHA3-256 of `sqlite3.c`:
`67f423e9ebbbdc473cbc4772c872ee6b89f31fde4ed0279a5c25d5f65c043a16`.
Version/source-ID constants in the vendored bindings match this amalgamation;
the existing FFI declarations remain compatible with the used SQLite API.
The test-only feature probe checks the runtime version and source ID, FTS5,
JSON and date functions for the actual linked archive.

The root lockfile and current example/benchmark/fuzz lockfiles use anyhow
1.0.103, fixing RUSTSEC-2026-0190. Historical compatibility fixtures keep their
original dependency resolutions. Downstream applications with their own
lockfiles must also resolve the patched dependency version.

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

The updated archive was generated with the official WASI SDK 34.0 clang and
llvm-ar, using the existing `-Oz` and shared build flags.

The script uses `wasm32-wasi-clang` by default. Set `WASI_CC` or `LLVM_AR` when
using non-standard tool locations.
Both `sqlite-precompiled` regeneration and `sqlite-bundled` use
`vendor/sqlite/build-flags.txt` as the SQLite compile flag source.
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

## Checking an engine upgrade

Retain the previous canister Wasm before rebuilding. To test its stored image
against the current build:

```sh
UPGRADE_FROM_WASM=/path/to/previous-canister.wasm \
  node --test tests/pocketic/upgrade.test.mjs
```

The regression run for this update passed from a retained SQLite 3.51.3 Wasm
to the SQLite 3.53.4 build. The feature probe additionally rejects malformed
FTS5 leaf headers with declared sizes zero/three and a truncated two-byte
header, then confirms valid MATCH queries recover after savepoint rollback.
This covers the shared leaf-validation boundary; it does not reproduce a full
remote exploit chain or establish attacker reachability in an application.
