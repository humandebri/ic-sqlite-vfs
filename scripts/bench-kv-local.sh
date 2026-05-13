#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BENCH_DIR="$ROOT/benchmarks/kv-canister"
ROWS="${1:-1000}"

cd "$BENCH_DIR"
icp network start -d
trap 'icp network stop >/dev/null 2>&1 || true' EXIT INT TERM

icp deploy ic-sqlite-vfs-kv-bench
icp canister call ic-sqlite-vfs-kv-bench bench_reset "($ROWS : nat32)" -e local
icp canister call ic-sqlite-vfs-kv-bench bench_read "($ROWS : nat32)" -e local
icp canister call ic-sqlite-vfs-kv-bench bench_write "($ROWS : nat32)" -e local
icp canister status ic-sqlite-vfs-kv-bench -e local
