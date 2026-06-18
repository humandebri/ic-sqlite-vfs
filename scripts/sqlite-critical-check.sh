#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ic-sqlite-vfs-critical.XXXXXX")"
PID_FILE="$RUN_DIR/pocketic.pids"
export IC_SQLITE_VFS_POCKETIC_RUN_DIR="$RUN_DIR"

log() {
  printf '\n==> %s\n' "$*"
}

node_bin() {
  if [[ -n "${NODE:-}" ]]; then
    echo "$NODE"
  elif command -v node >/dev/null 2>&1; then
    command -v node
  elif [[ -x "$HOME/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node" ]]; then
    echo "$HOME/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node"
  else
    echo "node not found" >&2
    return 1
  fi
}

build_pocketic_wasm() {
  cargo build --release --target wasm32-unknown-unknown --no-default-features --features sqlite-precompiled,canister-api
  mkdir -p target/pocketic
  cp target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm target/pocketic/ic_sqlite_vfs.wasm
}

build_pocketic_failpoint_wasm() {
  cargo build --release --target wasm32-unknown-unknown --no-default-features --features sqlite-precompiled,canister-api-test-failpoints
  mkdir -p target/pocketic
  cp target/wasm32-unknown-unknown/release/ic_sqlite_vfs.wasm target/pocketic/ic_sqlite_vfs_failpoints.wasm
}

build_bench_wasm() {
  cargo build --manifest-path benchmarks/kv-canister/Cargo.toml --target wasm32-unknown-unknown --release
  mkdir -p target/pocketic
  cp benchmarks/kv-canister/target/wasm32-unknown-unknown/release/ic_sqlite_vfs_kv_bench.wasm target/pocketic/ic_sqlite_vfs_kv_bench.wasm
}

run_pocketic_regression() {
  if command -v npm >/dev/null 2>&1; then
    npm run test:pocketic:regression
  else
    build_pocketic_wasm
    build_pocketic_failpoint_wasm
    "$(node_bin)" --test tests/pocketic/upgrade.test.mjs
  fi
}

run_pocketic_perf() {
  if command -v npm >/dev/null 2>&1; then
    npm run test:pocketic:perf
  else
    build_bench_wasm
    "$(node_bin)" --test tests/pocketic/perf_regression.test.mjs
  fi
}

tracked_pids() {
  if [[ -f "$PID_FILE" ]]; then
    sort -u "$PID_FILE" | while IFS= read -r pid; do
      [[ "$pid" =~ ^[0-9]+$ ]] || continue
      kill -0 "$pid" 2>/dev/null || continue
      echo "$pid"
    done
  fi
}

diagnose_pocketic() {
  echo "PocketIC process snapshot:"
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    ps -p "$pid" -o pid,ppid,stat,command 2>/dev/null || true
  done < <(tracked_pids)
  if [[ -s "$RUN_DIR/pocketic-start-errors.log" ]]; then
    echo "PocketIC start errors:"
    tail -n 80 "$RUN_DIR/pocketic-start-errors.log" || true
  fi
  while IFS= read -r log_file; do
    [[ -n "$log_file" ]] || continue
    echo "PocketIC log tail: $log_file"
    tail -n 120 "$log_file" || true
  done < <(find "$RUN_DIR" -maxdepth 1 -type f -name 'pocketic-*.log' 2>/dev/null | sort)
}

kill_tree() {
  local pid="$1"
  local signal="${2:-TERM}"
  local child
  if command -v pgrep >/dev/null 2>&1; then
    while IFS= read -r child; do
      kill_tree "$child" "$signal"
    done < <(pgrep -P "$pid" 2>/dev/null || true)
  fi
  kill "-$signal" "$pid" 2>/dev/null || true
}

kill_tracked_tree() {
  local signal="$1"
  local pid
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    kill_tree "$pid" "$signal"
  done < <(tracked_pids)
}

cleanup_pocketic() {
  kill_tracked_tree TERM
  sleep 1
  kill_tracked_tree KILL
}

cleanup_run_dir() {
  cleanup_pocketic
  rm -rf "$RUN_DIR"
}

run_with_timeout() {
  local seconds="$1"
  shift
  "$@" &
  local pid="$!"
  (
    sleep "$seconds"
    if kill -0 "$pid" 2>/dev/null; then
      echo "Command timed out after ${seconds}s: $*"
      diagnose_pocketic
      kill_tree "$pid" TERM
      sleep 2
      kill_tree "$pid" KILL
      cleanup_pocketic
    fi
  ) &
  local watchdog="$!"

  local status=0
  wait "$pid" || status="$?"
  kill "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  return "$status"
}

run_verus_proofs() {
  local verus_bin="${VERUS:-}"
  if [[ -z "$verus_bin" ]]; then
    if command -v verus >/dev/null 2>&1; then
      verus_bin="verus"
    elif [[ -x "$HOME/.local/bin/verus" ]]; then
      verus_bin="$HOME/.local/bin/verus"
    elif [[ -x "/opt/homebrew/bin/verus" ]]; then
      verus_bin="/opt/homebrew/bin/verus"
    fi
  fi

  if [[ -n "$verus_bin" ]]; then
    log "running Verus proof"
    mkdir -p target/verus
    while IFS= read -r proof; do
      "$verus_bin" --crate-type=lib --out-dir target/verus "$proof"
    done < <(find proofs/verus -maxdepth 1 -type f -name '*.rs' | sort)
  else
    if [[ "${VERUS_REQUIRED:-0}" == "1" ]]; then
      echo "Verus not found but VERUS_REQUIRED=1"
      exit 1
    fi
    echo "Verus not found; skipped proofs/verus/*.rs"
  fi
}

trap cleanup_run_dir EXIT

log "checking await-free canister API"
bash scripts/check-no-await.sh

log "checking bidi control characters"
if rg -nP '[\x{061C}\x{200E}\x{200F}\x{202A}-\x{202E}\x{2066}-\x{2069}]' \
  Cargo.toml README.md compat-fixtures docs scripts src tests; then
  echo "bidi control characters found"
  exit 1
fi

log "checking public API snapshot"
scripts/check-public-api-snapshot.sh

log "running cargo test"
cargo test

log "running cargo test --features canister-api"
cargo test --features canister-api

log "checking release profile"
cargo check --release

log "checking wasm release profile with sqlite-precompiled"
cargo check --release --target wasm32-unknown-unknown --no-default-features --features sqlite-precompiled

log "running MemoryManager compatibility fixture for ic-stable-structures 0.7.0"
cargo test --manifest-path compat-fixtures/ic-stable-structures-070/Cargo.toml --locked

log "running MemoryManager compatibility fixture for ic-stable-structures 0.7.2"
cargo test --manifest-path compat-fixtures/ic-stable-structures-072/Cargo.toml --locked

log "updating MemoryManager compatibility fixture to latest ic-stable-structures 0.7.x"
cargo update --manifest-path compat-fixtures/ic-stable-structures-latest-07/Cargo.toml -p ic-stable-structures

log "running MemoryManager compatibility fixture for latest ic-stable-structures 0.7.x"
cargo test --manifest-path compat-fixtures/ic-stable-structures-latest-07/Cargo.toml

run_verus_proofs

for attempt in 1 2; do
  log "running PocketIC regression attempt ${attempt}"
  : > "$PID_FILE"
  if run_with_timeout 600 run_pocketic_regression; then
    break
  fi
  diagnose_pocketic
  cleanup_pocketic
  if [[ "$attempt" -eq 2 ]]; then
    exit 1
  fi
  echo "PocketIC regression failed; retrying once"
done

for attempt in 1 2; do
  log "running PocketIC performance and capacity regression attempt ${attempt}"
  : > "$PID_FILE"
  if run_with_timeout 600 run_pocketic_perf; then
    break
  fi
  diagnose_pocketic
  cleanup_pocketic
  if [[ "$attempt" -eq 2 ]]; then
    exit 1
  fi
  echo "PocketIC performance and capacity regression failed; retrying once"
done
