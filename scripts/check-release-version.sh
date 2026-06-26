#!/usr/bin/env bash
# scripts/check-release-version.sh
# Checks release version/tag alignment before and after release.
# This keeps crate metadata, package metadata, local tag, and origin tag
# on the same release point before crates.io publish.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

require_pushed_tag=0

usage() {
  echo "usage: $0 [--require-pushed-tag]"
}

if [[ "${1:-}" == "--require-pushed-tag" ]]; then
  require_pushed_tag=1
  shift
fi

if [[ "$#" -ne 0 ]]; then
  usage >&2
  exit 2
fi

cargo_version="$(
  cargo metadata --no-deps --format-version 1 \
    | node -e '
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  input += chunk;
});
process.stdin.on("end", () => {
  const metadata = JSON.parse(input);
  const pkg = metadata.packages.find((item) => item.name === "ic-sqlite-vfs");
  if (!pkg || typeof pkg.version !== "string") {
    process.exit(1);
  }
  process.stdout.write(pkg.version);
});
'
)"

package_version="$(
  node -e '
const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json", "utf8"));
if (typeof pkg.version !== "string") {
  process.exit(1);
}
process.stdout.write(pkg.version);
'
)"

rust_version="$(
  node -e '
const fs = require("fs");
const manifest = fs.readFileSync("Cargo.toml", "utf8");
const match = manifest.match(/^rust-version\s*=\s*"([^"]+)"$/m);
if (!match) {
  process.exit(1);
}
process.stdout.write(match[1]);
'
)"

toolchain_channel="$(
  node -e '
const fs = require("fs");
const toolchain = fs.readFileSync("rust-toolchain.toml", "utf8");
const match = toolchain.match(/^channel\s*=\s*"([^"]+)"$/m);
if (!match) {
  process.exit(1);
}
process.stdout.write(match[1]);
'
)"

if [[ "$cargo_version" != "$package_version" ]]; then
  echo "Cargo.toml version ($cargo_version) differs from package.json version ($package_version)" >&2
  exit 1
fi

if [[ "$rust_version" != "$toolchain_channel" ]]; then
  echo "Cargo.toml rust-version ($rust_version) differs from rust-toolchain.toml channel ($toolchain_channel)" >&2
  exit 1
fi

build_wasm_script="$(
  node -e '
const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json", "utf8"));
if (!pkg.scripts || typeof pkg.scripts["build:wasm"] !== "string") {
  process.exit(1);
}
process.stdout.write(pkg.scripts["build:wasm"]);
'
)"

build_wasm_failpoints_script="$(
  node -e '
const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync("package.json", "utf8"));
if (!pkg.scripts || typeof pkg.scripts["build:wasm:failpoints"] !== "string") {
  process.exit(1);
}
process.stdout.write(pkg.scripts["build:wasm:failpoints"]);
'
)"

for required in \
  "cargo build --release --target wasm32-unknown-unknown" \
  "--no-default-features" \
  "--features sqlite-precompiled,canister-api" \
  "target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm" \
  "target/pocketic/ic_sqlite_vfs.wasm"; do
  if [[ "$build_wasm_script" != *"$required"* ]]; then
    echo "package.json build:wasm does not contain required release artifact fragment: $required" >&2
    exit 1
  fi
done

for required in \
  "cargo build --release --target wasm32-unknown-unknown" \
  "--no-default-features" \
  "--features sqlite-precompiled,canister-api-test-failpoints" \
  "target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm" \
  "target/pocketic/ic_sqlite_vfs_failpoints.wasm"; do
  if [[ "$build_wasm_failpoints_script" != *"$required"* ]]; then
    echo "package.json build:wasm:failpoints does not contain required release artifact fragment: $required" >&2
    exit 1
  fi
done

expected_rust_toolchain_action="dtolnay/rust-toolchain@${toolchain_channel}"
for workflow in .github/workflows/ci.yml .github/workflows/release.yml; do
  if grep -Fq "dtolnay/rust-toolchain@stable" "$workflow"; then
    echo "$workflow uses dtolnay/rust-toolchain@stable; expected $expected_rust_toolchain_action" >&2
    exit 1
  fi
  if ! grep -Fq "$expected_rust_toolchain_action" "$workflow"; then
    echo "$workflow does not use $expected_rust_toolchain_action" >&2
    exit 1
  fi
  if grep -Fq "target/wasm32-unknown-unknown/debug/ic_sqlite_vfs.wasm" "$workflow"; then
    echo "$workflow still checks or uploads debug wasm; expected target/pocketic/ic_sqlite_vfs.wasm" >&2
    exit 1
  fi
  if ! grep -Fq "target/pocketic/ic_sqlite_vfs.wasm" "$workflow"; then
    echo "$workflow does not check or upload target/pocketic/ic_sqlite_vfs.wasm" >&2
    exit 1
  fi
  if ! grep -Fxq "      - run: cargo build --target wasm32-unknown-unknown" "$workflow"; then
    echo "$workflow does not run default-feature wasm build" >&2
    exit 1
  fi
  if ! grep -Fxq "      - run: cargo build --target wasm32-unknown-unknown --features canister-api" "$workflow"; then
    echo "$workflow does not run default-feature canister-api wasm build" >&2
    exit 1
  fi
  if ! grep -Fxq "      - run: VERUS_REQUIRED=1 bash scripts/sqlite-critical-check.sh" "$workflow"; then
    echo "$workflow does not require Verus proofs in sqlite-critical-check.sh" >&2
    exit 1
  fi
  if ! grep -Fq "VERUS_VERSION: 0.2026.05.05.d03e906" "$workflow"; then
    echo "$workflow does not install the pinned Verus version" >&2
    exit 1
  fi
  if ! grep -Fq "VERUS_SHA256: 09c0ef9f59995a9c0e26c2a47ef6a6fc437d67816aca1685d05b872519af426c" "$workflow"; then
    echo "$workflow does not pin the Verus x86-linux archive checksum" >&2
    exit 1
  fi
  if ! grep -Fq 'verus-${VERUS_VERSION}-x86-linux.zip' "$workflow"; then
    echo "$workflow does not download the Verus x86-linux archive" >&2
    exit 1
  fi
  if ! grep -Fq "npm run test:pocketic:regression" "$workflow"; then
    echo "$workflow does not run npm run test:pocketic:regression" >&2
    exit 1
  fi
  if ! grep -Fq "npm run test:pocketic:perf" "$workflow"; then
    echo "$workflow does not run npm run test:pocketic:perf" >&2
    exit 1
  fi
  if ! grep -Fq "scripts/check-release-package.sh" "$workflow"; then
    echo "$workflow does not run scripts/check-release-package.sh" >&2
    exit 1
  fi
done

expected_tag="v${cargo_version}"
exact_tags="$(git tag --points-at HEAD)"

if [[ -n "$exact_tags" ]]; then
  if ! grep -Fxq "$expected_tag" <<<"$exact_tags"; then
    echo "HEAD has tag(s), but expected $expected_tag:" >&2
    echo "$exact_tags" >&2
    exit 1
  fi

  unexpected_release_tags="$(grep -E '^v[0-9]+[.][0-9]+[.][0-9]+' <<<"$exact_tags" | grep -Fxv "$expected_tag" || true)"
  if [[ -n "$unexpected_release_tags" ]]; then
    echo "HEAD has unexpected release tag(s):" >&2
    echo "$unexpected_release_tags" >&2
    exit 1
  fi
fi

if [[ "$require_pushed_tag" -eq 0 ]]; then
  exit 0
fi

if ! grep -Fxq "$expected_tag" <<<"$exact_tags"; then
  echo "HEAD is not tagged with $expected_tag" >&2
  exit 1
fi

local_tag_commit="$(git rev-parse "refs/tags/${expected_tag}^{commit}")"
remote_tag_commit="$(
  git ls-remote --tags origin "refs/tags/${expected_tag}^{}" \
    | awk -v ref="refs/tags/${expected_tag}^{}" '$2 == ref { print $1 }'
)"

if [[ -z "$remote_tag_commit" ]]; then
  remote_tag_commit="$(
    git ls-remote --tags origin "refs/tags/${expected_tag}" \
      | awk -v ref="refs/tags/${expected_tag}" '$2 == ref { print $1 }'
  )"
fi

if [[ -z "$remote_tag_commit" ]]; then
  echo "origin is missing tag $expected_tag" >&2
  exit 1
fi

if [[ "$local_tag_commit" != "$remote_tag_commit" ]]; then
  echo "local tag $expected_tag ($local_tag_commit) differs from origin ($remote_tag_commit)" >&2
  exit 1
fi
