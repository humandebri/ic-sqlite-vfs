#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

bash scripts/check-no-await.sh
cargo test
cargo test --features canister-api

for attempt in 1 2; do
  if npm run test:pocketic:regression; then
    break
  fi
  if [[ "$attempt" -eq 2 ]]; then
    exit 1
  fi
  echo "PocketIC regression failed; retrying once"
done

verus_bin="${VERUS:-}"
if [[ -z "$verus_bin" ]]; then
  if command -v verus >/dev/null 2>&1; then
    verus_bin="verus"
  fi
fi

if [[ -n "$verus_bin" ]]; then
  mkdir -p target/verus
  "$verus_bin" --crate-type=lib --out-dir target/verus proofs/verus/layout_math.rs
else
  echo "Verus not found; skipped proofs/verus/layout_math.rs"
fi
