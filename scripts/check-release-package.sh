#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LIST="$(mktemp "${TMPDIR:-/tmp}/ic-sqlite-vfs-package.XXXXXX")"
trap 'rm -f "$LIST"' EXIT

cargo package --list --allow-dirty > "$LIST"

require_path() {
  local path="$1"
  if ! grep -Fxq "$path" "$LIST"; then
    echo "release package is missing required path: $path"
    echo "Run this check from a git tag or after staging/tracking release files."
    exit 1
  fi
}

reject_pattern() {
  local pattern="$1"
  if grep -Eq "$pattern" "$LIST"; then
    echo "release package contains forbidden path matching: $pattern"
    grep -E "$pattern" "$LIST"
    exit 1
  fi
}

require_path "docs/PUBLIC_API_2_0.snapshot"
require_path "LICENSE-APACHE"
require_path "LICENSE-MIT"
require_path "scripts/check-release-version.sh"
require_path "scripts/check-sqlite-precompiled.sh"
require_path "scripts/check-wasm-contract.sh"
require_path "tests/public_api.rs"
require_path "vendor/sqlite/wasm-compiler-flags.txt"
require_path "vendor/sqlite/wasm32-unknown-unknown/lib/libsqlite3.build-metadata"

reject_pattern '(^|/)target/'
reject_pattern '(^|/)[.]DS_Store$'
reject_pattern '^node_modules/'
reject_pattern '^package-lock\.json$'
