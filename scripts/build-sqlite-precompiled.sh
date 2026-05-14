#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CC_BIN="${WASI_CC:-wasm32-wasi-clang}"
AR_BIN="${LLVM_AR:-}"
if [[ -z "$AR_BIN" ]]; then
  if command -v llvm-ar >/dev/null 2>&1; then
    AR_BIN="llvm-ar"
  elif [[ -x /opt/homebrew/opt/llvm/bin/llvm-ar ]]; then
    AR_BIN="/opt/homebrew/opt/llvm/bin/llvm-ar"
  elif [[ -x /opt/homebrew/Cellar/llvm/22.1.1/bin/llvm-ar ]]; then
    AR_BIN="/opt/homebrew/Cellar/llvm/22.1.1/bin/llvm-ar"
  else
    echo "llvm-ar not found; set LLVM_AR" >&2
    exit 1
  fi
fi

OUT_DIR="vendor/sqlite/wasm32-unknown-unknown"
OBJ="$OUT_DIR/obj/sqlite3.o"
LIB="$OUT_DIR/lib/libsqlite3.a"

mkdir -p "$(dirname "$OBJ")" "$(dirname "$LIB")"

DEFINE_FLAGS=()
while IFS= read -r flag; do
  [[ -n "$flag" ]] || continue
  DEFINE_FLAGS+=("-D$flag")
done < vendor/sqlite/build-flags.txt

"$CC_BIN" \
  --target=wasm32-wasip1 \
  -Oz \
  -I vendor/sqlite/src \
  -I c/include \
  "${DEFINE_FLAGS[@]}" \
  -c vendor/sqlite/src/sqlite3.c \
  -o "$OBJ"

"$AR_BIN" crs "$LIB" "$OBJ"
echo "built $LIB"
