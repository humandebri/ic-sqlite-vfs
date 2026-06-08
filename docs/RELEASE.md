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
npm run build:wasm
npm run test:pocketic:compat
npm run test:pocketic:perf
cargo package --no-verify --allow-dirty
cargo package --list --allow-dirty
scripts/check-release-package.sh
wasm-objdump -x target/pocketic/ic_sqlite_vfs.wasm
```

`wasm-objdump` の import は `ic0.*` のみ許可する。`env.*` が出た場合は release しない。
GitHub Release artifact は `sqlite-precompiled,canister-api` の release profile
で build した `target/pocketic/ic_sqlite_vfs.wasm` に統一する。

`cargo package --list` では `docs/PUBLIC_API_1_0.snapshot` と release
check scripts を含め、`target/`、`node_modules/`、`package-lock.json` を
含めない。PocketIC fixture は nested Cargo package なので crates.io
tarballではなく git tag checkout 側のrelease gateで検証する。

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

`npm run test:pocketic:compat` は `0.2.2 -> current` と `1.0.0 -> current`
の upgrade/export/import/migration を検証する。`0.2.2` fixture は pre-`1.0`
baseline、`1.0.0` fixture は published `1.x` baseline として維持する。

tag は `Cargo.toml` の version と一致させる。例: `version = "0.2.0"` なら tag は `v0.2.0`。
crates.io publish は GitHub Actions では行わない。tag push と GitHub
Release 成功確認より先に `cargo publish` を実行しない。

`1.0.1` release は以下の順序で行う。

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
`v1.0.1` 以降の artifact は `sqlite-precompiled,canister-api` の release
profile build とする。
`v1.0.0` は release guard の annotated tag commit 比較修正後、crates.io
publish 前に修正 commit へ付け替えた。公開済み tag、GitHub Release、
crates.io artifact は以後変更しない。

`scripts/check-release-version.sh --require-pushed-tag` は annotated tag object
ではなく tag が指す commit を比較する。これにより local tag と origin tag の
object SHA が異なっても、同じ commit を指す正常な annotated tag を許可する。
