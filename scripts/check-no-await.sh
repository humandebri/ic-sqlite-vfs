#!/usr/bin/env bash
set -euo pipefail

command -v rg >/dev/null 2>&1 || {
  echo "ripgrep (rg) is required" >&2
  exit 1
}

if rg --line-number '(\.await|async fn)' src; then
  echo "SQLite transaction code must stay synchronous; async/await found." >&2
  exit 1
fi
