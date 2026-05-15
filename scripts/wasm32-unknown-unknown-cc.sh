#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CC_BIN="${WASI_CC:-wasm32-wasi-clang}"

ARGS=("--target=wasm32-wasip1")
SKIP_NEXT_TARGET=0
for arg in "$@"; do
  if [[ "$SKIP_NEXT_TARGET" == "1" ]]; then
    SKIP_NEXT_TARGET=0
    continue
  fi
  case "$arg" in
    --target)
      SKIP_NEXT_TARGET=1
      ;;
    --target=wasm32-wasi | --target=wasm32-wasip1 | --target=wasm32-unknown-unknown)
      ;;
    *)
      ARGS+=("$arg")
      ;;
  esac
done

exec "$CC_BIN" "-I$ROOT_DIR/c/include" "${ARGS[@]}"
