#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-build-support.sh <canister-workspace> [--force]

Installs the Cargo and C compiler support files required to build
ic-sqlite-vfs on wasm32-unknown-unknown.

The script refuses to overwrite existing files unless --force is passed.
USAGE
}

if [[ "${1:-}" == "" || "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

TARGET_ROOT="$1"
FORCE="${2:-}"

if [[ "$FORCE" != "" && "$FORCE" != "--force" ]]; then
  usage >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

mkdir -p "$TARGET_ROOT/.cargo" "$TARGET_ROOT/scripts" "$TARGET_ROOT/c"

install_file() {
  local source="$1"
  local target="$2"
  if [[ -e "$target" && "$FORCE" != "--force" ]]; then
    echo "refusing to overwrite $target; pass --force to replace it" >&2
    exit 1
  fi
  cp "$source" "$target"
}

install_file "$SOURCE_ROOT/build-support/cargo-config.toml" "$TARGET_ROOT/.cargo/config.toml"
install_file "$SOURCE_ROOT/scripts/wasm32-unknown-unknown-cc.sh" \
  "$TARGET_ROOT/scripts/wasm32-unknown-unknown-cc.sh"

if [[ -e "$TARGET_ROOT/c/include" && "$FORCE" != "--force" ]]; then
  echo "refusing to overwrite $TARGET_ROOT/c/include; pass --force to replace it" >&2
  exit 1
fi

rm -rf "$TARGET_ROOT/c/include"
cp -R "$SOURCE_ROOT/c/include" "$TARGET_ROOT/c/include"
chmod +x "$TARGET_ROOT/scripts/wasm32-unknown-unknown-cc.sh"

echo "installed ic-sqlite-vfs build support into $TARGET_ROOT"
