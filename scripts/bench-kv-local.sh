#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BENCH_DIR="$ROOT/benchmarks/kv-canister"
ROWS="${1:-1000}"
WRITES="${2:-256}"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf '%s\n' "missing required command: $1" >&2
    exit 1
  fi
}

call_ok() {
  RESULT=$(icp canister call ic-sqlite-vfs-kv-bench "$1" "$2" -e local)
  printf '%s\n' "$RESULT"
  case "$RESULT" in
    *'Ok = record'*) ;;
    *)
      printf '%s\n' "$1 failed" >&2
      exit 1
      ;;
  esac
}

require_command icp
require_command ic-wasm
icp --version
ic-wasm --version

cd "$BENCH_DIR"
icp network start -d
trap 'icp network stop >/dev/null 2>&1 || true' EXIT INT TERM

icp deploy ic-sqlite-vfs-kv-bench
call_ok bench_reset "($ROWS : nat32)"
call_ok bench_read "($ROWS : nat32)"
call_ok bench_write "($ROWS : nat32)"
call_ok bench_capacity_growth_guard "($ROWS : nat32, $WRITES : nat32)"
icp canister status ic-sqlite-vfs-kv-bench -e local
