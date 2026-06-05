#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SNAPSHOT="docs/PUBLIC_API_1_0.snapshot"

generate_snapshot() {
  while IFS= read -r file; do
    awk -v file="$file" '
      function normalize(text) {
        gsub(/[[:space:]]+/, " ", text)
        sub(/^ /, "", text)
        sub(/ $/, "", text)
        gsub(/ :/, ":", text)
        return text
      }
      function count_char(text, char, tmp) {
        tmp = text
        return gsub(char, char, tmp)
      }
      function is_public_line(text) {
        return text ~ /^[[:space:]]*pub([[:space:]]|\(|$)/ &&
          text !~ /^[[:space:]]*pub\((crate|super|in )/
      }
      function is_public_type_start(text) {
        return text ~ /^[[:space:]]*pub[[:space:]]+(struct|enum)[[:space:]]/
      }
      {
        line = $0
        public_line = is_public_line(line)
        field_like = line ~ /^[[:space:]]*pub[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*:/
        public_type_start = is_public_type_start(line)

        if (public_line && (!field_like || public_type_depth > 0 || public_type_start)) {
          print file ":" normalize(line)
        }

        if (public_type_start || public_type_depth > 0) {
          public_type_depth += count_char(line, "{") - count_char(line, "}")
          if (public_type_depth < 0) {
            public_type_depth = 0
          }
        }
      }
    ' "$file"
  done < <(rg --files src -g '*.rs' | rg -v '^src/api/sqlite_feature_probe\.rs$' | sort) \
    | rg -v '(StepFailpoint|set_step_failpoint|clear_step_failpoint|MemoryFailpoint|set_failpoint|clear_failpoint)' \
    | sort
}

if [[ "${1:-}" == "--print" ]]; then
  generate_snapshot
  exit 0
fi

if [[ ! -f "$SNAPSHOT" ]]; then
  echo "missing public API snapshot: $SNAPSHOT"
  echo "create it with: scripts/check-public-api-snapshot.sh --print > $SNAPSHOT"
  exit 1
fi

TMP="$(mktemp "${TMPDIR:-/tmp}/ic-sqlite-vfs-public-api.XXXXXX")"
trap 'rm -f "$TMP"' EXIT
generate_snapshot > "$TMP"

if ! diff -u "$SNAPSHOT" "$TMP"; then
  echo "public API snapshot changed"
  echo "If this is intentional for 1.0, update $SNAPSHOT and docs/API_STABILITY.md."
  exit 1
fi
