# Release

## Gates

Release 前に以下を全て通す。

```sh
cargo fmt --check
bash scripts/sqlite-critical-check.sh
cargo package --no-verify
cargo build --target wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --features canister-api
npm run test:pocketic:perf
wasm-objdump -x target/wasm32-unknown-unknown/debug/ic_sqlite_vfs.wasm
```

`wasm-objdump` の import は `ic0.*` のみ許可する。`env.*` が出た場合は release しない。

tag は `Cargo.toml` の version と一致させる。例: `version = "0.2.0"` なら tag は `v0.2.0`。
crates.io publish は GitHub Actions では行わない。ローカルで `cargo login` 後に手動実行する。

```sh
cargo publish --no-verify
```

## Artifact

tag `v*` を push すると GitHub Actions が wasm を build し、GitHub Release artifact としてアップロードする。
今回の tag は `v0.2.2`。
