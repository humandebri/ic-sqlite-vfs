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
    if command -v lsof >/dev/null 2>&1; then
      echo "PocketIC listening ports for pid $pid:"
      lsof -a -nP -p "$pid" -iTCP -sTCP:LISTEN || true
    fi
  done < <(tracked_pids)
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

trap cleanup_run_dir EXIT

log "checking await-free canister API"
bash scripts/check-no-await.sh

log "running cargo test"
cargo test

log "running cargo test --features canister-api"
cargo test --features canister-api

for attempt in 1 2; do
  log "running PocketIC regression attempt ${attempt}"
  : > "$PID_FILE"
  if run_with_timeout 300 npm run test:pocketic:regression; then
    break
  fi
  diagnose_pocketic
  cleanup_pocketic
  if [[ "$attempt" -eq 2 ]]; then
    exit 1
  fi
  echo "PocketIC regression failed; retrying once"
done

log "running PocketIC perf"
: > "$PID_FILE"
run_with_timeout 300 npm run test:pocketic:perf

verus_bin="${VERUS:-}"
if [[ -z "$verus_bin" ]]; then
  if command -v verus >/dev/null 2>&1; then
    verus_bin="verus"
  fi
fi

if [[ -n "$verus_bin" ]]; then
  log "running Verus proof"
  mkdir -p target/verus
  "$verus_bin" --crate-type=lib --out-dir target/verus proofs/verus/layout_math.rs
else
  echo "Verus not found; skipped proofs/verus/layout_math.rs"
fi
