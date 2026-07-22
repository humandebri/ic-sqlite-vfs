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
METADATA="$OUT_DIR/lib/libsqlite3.build-metadata"

sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

build_inputs_sha256() {
  local path
  for path in \
    scripts/build-sqlite-precompiled.sh \
    vendor/sqlite/src/sqlite3.c \
    vendor/sqlite/build-flags.txt \
    vendor/sqlite/wasm-compiler-flags.txt \
    c/include/*.h; do
    printf '%s\n%s\n' "$path" "$(sha256_file "$path")"
  done | sha256_stream
}

mkdir -p "$(dirname "$OBJ")" "$(dirname "$LIB")"

DEFINE_FLAGS=()
while IFS= read -r flag; do
  flag="${flag#"${flag%%[![:space:]]*}"}"
  flag="${flag%"${flag##*[![:space:]]}"}"
  [[ -n "$flag" ]] || continue
  [[ "$flag" != \#* ]] || continue
  DEFINE_FLAGS+=("-D$flag")
done < vendor/sqlite/build-flags.txt

COMPILER_FLAGS=()
while IFS= read -r flag; do
  flag="${flag#"${flag%%[![:space:]]*}"}"
  flag="${flag%"${flag##*[![:space:]]}"}"
  [[ -n "$flag" ]] || continue
  [[ "$flag" != \#* ]] || continue
  COMPILER_FLAGS+=("$flag")
done < vendor/sqlite/wasm-compiler-flags.txt

"$CC_BIN" \
  --target=wasm32-wasip1 \
  "${COMPILER_FLAGS[@]}" \
  -I vendor/sqlite/src \
  -I c/include \
  "${DEFINE_FLAGS[@]}" \
  -c vendor/sqlite/src/sqlite3.c \
  -o "$OBJ"

"$AR_BIN" crs "$LIB" "$OBJ"

CC_VERSION="$("$CC_BIN" --version)"
CC_VERSION="${CC_VERSION%%$'\n'*}"
AR_VERSION="$("$AR_BIN" --version)"
AR_VERSION="${AR_VERSION%%$'\n'*}"
{
  printf 'format=1\n'
  printf 'build_inputs_sha256=%s\n' "$(build_inputs_sha256)"
  printf 'archive_sha256=%s\n' "$(sha256_file "$LIB")"
  printf 'compiler_version=%s\n' "$CC_VERSION"
  printf 'archiver_version=%s\n' "$AR_VERSION"
} > "$METADATA"

echo "built $LIB and $METADATA"
