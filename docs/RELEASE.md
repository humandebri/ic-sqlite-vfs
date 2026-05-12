# Release

## Gates

Release 前に以下を全て通す。

```sh
cargo fmt --check
bash scripts/check-no-await.sh
cargo test
cargo build --target wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --features canister-api
npm run test:pocketic
wasm-objdump -x target/wasm32-unknown-unknown/debug/ic_sqlite_vfs.wasm
```

`wasm-objdump` の import は `ic0.*` のみ許可する。`env.*` が出た場合は release しない。

## Artifact

tag `v*` を push すると GitHub Actions が wasm を build し、release artifact としてアップロードする。
