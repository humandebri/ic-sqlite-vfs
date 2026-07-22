#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LIB="vendor/sqlite/wasm32-unknown-unknown/lib/libsqlite3.a"
METADATA="vendor/sqlite/wasm32-unknown-unknown/lib/libsqlite3.build-metadata"

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

metadata_value() {
  local key="$1"
  local value
  value="$(sed -n "s/^${key}=//p" "$METADATA")"
  if [[ -z "$value" || "$(grep -c "^${key}=" "$METADATA")" -ne 1 ]]; then
    echo "invalid or missing metadata key: $key" >&2
    exit 1
  fi
  printf '%s' "$value"
}

for path in "$LIB" "$METADATA"; do
  if [[ ! -f "$path" ]]; then
    echo "required precompiled SQLite artifact is missing: $path" >&2
    exit 1
  fi
done

if [[ "$(metadata_value format)" != "1" ]]; then
  echo "unsupported precompiled SQLite metadata format" >&2
  exit 1
fi

EXPECTED_INPUTS="$(metadata_value build_inputs_sha256)"
ACTUAL_INPUTS="$(build_inputs_sha256)"
if [[ "$ACTUAL_INPUTS" != "$EXPECTED_INPUTS" ]]; then
  echo "precompiled SQLite build inputs changed; regenerate with scripts/build-sqlite-precompiled.sh" >&2
  exit 1
fi

EXPECTED_ARCHIVE="$(metadata_value archive_sha256)"
ACTUAL_ARCHIVE="$(sha256_file "$LIB")"
if [[ "$ACTUAL_ARCHIVE" != "$EXPECTED_ARCHIVE" ]]; then
  echo "precompiled SQLite archive does not match its build metadata" >&2
  exit 1
fi

echo "precompiled SQLite provenance ok: inputs=$ACTUAL_INPUTS archive=$ACTUAL_ARCHIVE"
