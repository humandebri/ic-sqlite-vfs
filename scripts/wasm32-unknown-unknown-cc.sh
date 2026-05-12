#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CC_BIN="${WASI_CC:-wasm32-wasi-clang}"

exec "$CC_BIN" "-I$ROOT_DIR/c/include" "$@"
