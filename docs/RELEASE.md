# Release

## Gates

Release gate は git tag checkout 上で実行する。crates.io tarball 単体で
PocketIC/npm gate を再現する設計ではない。

```sh
cargo fmt --check
scripts/check-release-version.sh
cargo check --release
cargo check --release --target wasm32-unknown-unknown --no-default-features --features sqlite-precompiled
cargo test --tests
cargo test --test public_api
bash scripts/sqlite-critical-check.sh
cargo build --target wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --features canister-api
cargo package --no-verify --allow-dirty
cargo package --list --allow-dirty
scripts/check-release-package.sh
npm run test:pocketic:compat
npm run test:pocketic:perf
npm run build:wasm
wasm-objdump -x target/pocketic/ic_sqlite_vfs.wasm
```

`wasm-objdump` の import は `ic0.*` のみ許可する。`env.*` が出た場合は release しない。
GitHub Release artifact は `sqlite-precompiled,canister-api` の release profile
で build した `target/pocketic/ic_sqlite_vfs.wasm` に統一する。
`scripts/sqlite-critical-check.sh` は基礎検査、Verus proof、PocketIC regression、
PocketIC performance/capacity regression を実行する。compat と package
contents は workflow と release gate で明示実行する。
Verus zero extent lemmas marked `#[verifier::external_body]` are tracked trusted
axioms for this release. The release gate compensates with Rust property tests
that compare the production normalizer against an independent model, but this is
not a full mechanical proof of the production Rust implementation.

`cargo package --list` では `docs/PUBLIC_API_2_0.snapshot` と release
check scripts を含め、`target/`、`node_modules/`、`package-lock.json` を
含めない。PocketIC fixture は nested Cargo package なので crates.io
tarballではなく git tag checkout 側のrelease gateで検証する。

## 2.0.0 Notes

2.0.0 は breaking stable-layout release。v8 は SQLite image を
`db_base_offset` 以降に in-place 保存する。v6 segmented page-map stable
layout の直接 upgrade success は要求しない。旧 canister で logical SQLite
image を export し、fresh v8 canister へ import する経路を release gate で
検証する。
In-place commit writes dirty pages before superblock publish and relies on IC
message execution atomicity plus trap rollback. Commit must not perform
inter-canister calls, `await`, or `ic0.call_perform`.

公開Rust APIでは `PAGE_MAP_LAYOUT_VERSION` を削除し、
`CURRENT_LAYOUT_VERSION` に改名する。v8 では page-table read cache がないため
`stable_blob::invalidate_read_cache()` も削除する。`compact()` は public API として
維持するが、v8 では no-op。
`read_metrics`、`sqlite_vfs`、`stable` は public compatibility surface から外す。
既存 `ic-rusqlite` raw SQLite image を直接 `Db::init` する経路は拒否し、
旧 canister export から fresh v8 import だけを正式移行経路にする。

## Toolchain

検証バージョン:

- Rust: `rustc 1.95.0`
- Node.js: `v22.22.0`
- npm: `11.14.1`
- dfx: `0.31.0`
- PocketIC: `@dfinity/pic` が起動する `pocket-ic`

`rust-toolchain.toml` は Rust toolchain と wasm targets を固定する。CI と
release workflow も `Cargo.toml` の `rust-version` と同じ `1.95.0` を使う。

## Perf Gate

`npm run test:pocketic:perf` は release blocking。閾値更新は、同じ
toolchain、同じマシン種別、同じ PocketIC version で3回以上実行し、最悪値が
新閾値内に収まる場合だけ行う。ローカル単発の高速化・低速化は advisory として
扱い、単独では閾値を変更しない。

## Compatibility Fixtures

`npm run test:pocketic:compat` は `0.2.2` と `1.0.0` の旧 canister から
logical SQLite image を export し、current fresh canister へ import できることを
検証する。2.0 では旧 stable layout の直接 upgrade success は要求しない。

tag は `Cargo.toml` の version と一致させる。例: `version = "0.2.0"` なら tag は `v0.2.0`。
crates.io publish は GitHub Actions では行わない。tag push と GitHub
Release 成功確認より先に `cargo publish` を実行しない。

release は以下の順序で行う。

```sh
VERSION="$(cargo metadata --no-deps --format-version 1 | node -e 'let input = ""; process.stdin.on("data", chunk => input += chunk); process.stdin.on("end", () => { const metadata = JSON.parse(input); process.stdout.write(metadata.packages.find(pkg => pkg.name === "ic-sqlite-vfs").version); });')"
git tag -a "v${VERSION}" -m "v${VERSION}"
git push origin "v${VERSION}"
git checkout "v${VERSION}"
scripts/check-release-version.sh --require-pushed-tag
```

GitHub Release の workflow 成功と wasm artifact を確認してから、ローカルで
`cargo login` 後に手動実行する。

```sh
cargo publish --no-verify
```

## Artifact

tag `v*` を push すると GitHub Actions が wasm を build し、GitHub Release artifact としてアップロードする。
`v2.0.0` 以降の artifact は `sqlite-precompiled,canister-api` の release
profile build とする。
`v1.0.0` は release guard の annotated tag commit 比較修正後、crates.io
publish 前に修正 commit へ付け替えた。公開済み tag、GitHub Release、
crates.io artifact は以後変更しない。

`scripts/check-release-version.sh --require-pushed-tag` は annotated tag object
ではなく tag が指す commit を比較する。これにより local tag と origin tag の
object SHA が異なっても、同じ commit を指す正常な annotated tag を許可する。
