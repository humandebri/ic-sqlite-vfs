#!/usr/bin/env bash
set -euo pipefail

if rg --line-number '(\.await|async fn)' src; then
  echo "SQLite transaction code must stay synchronous; async/await found." >&2
  exit 1
fi
