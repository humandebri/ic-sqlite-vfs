#!/usr/bin/env bash
set -euo pipefail

command -v rg >/dev/null 2>&1 || {
  echo "ripgrep (rg) is required" >&2
  exit 1
}

if rg --line-number '(\.await|async[[:space:]]+fn|call_perform|ic_cdk::call|call_raw)' src; then
  echo "SQLite transaction code must stay synchronous and call-free; runtime contract violation found." >&2
  exit 1
fi
