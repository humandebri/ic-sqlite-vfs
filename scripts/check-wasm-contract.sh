#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 <canister.wasm>" >&2
  exit 2
fi

WASM="$1"
if [[ ! -f "$WASM" ]]; then
  echo "wasm artifact not found: $WASM" >&2
  exit 1
fi

DETAILS="$(wasm-objdump -x "$WASM")"
IMPORTS="$(grep '<-' <<<"$DETAILS" || true)"
echo "$IMPORTS"
if grep -v '<- ic0\.' <<<"$IMPORTS" | grep -q .; then
  echo "unexpected non-ic0 import in $WASM" >&2
  exit 1
fi

if ! grep -Fq -- '- [+] simd128' <<<"$DETAILS"; then
  echo "missing required simd128 target feature in $WASM" >&2
  exit 1
fi

CODE_HEX="$(wasm-objdump -h "$WASM" | sed -nE 's/.*Code .*size=0x([0-9a-fA-F]+).*/\1/p')"
if [[ -z "$CODE_HEX" ]]; then
  echo "unable to read Wasm code section size from $WASM" >&2
  exit 1
fi
CODE_BYTES="$((16#$CODE_HEX))"
CODE_LIMIT="$((10 * 1024 * 1024))"
if (( CODE_BYTES > CODE_LIMIT )); then
  echo "Wasm code section exceeds 10 MiB: $CODE_BYTES bytes" >&2
  exit 1
fi

TOTAL_BYTES="$(wc -c < "$WASM" | tr -d '[:space:]')"
TOTAL_LIMIT="$((100 * 1024 * 1024))"
if (( TOTAL_BYTES > TOTAL_LIMIT )); then
  echo "Wasm module exceeds 100 MiB: $TOTAL_BYTES bytes" >&2
  exit 1
fi

echo "wasm contract ok: total_bytes=$TOTAL_BYTES code_bytes=$CODE_BYTES simd128=required"
