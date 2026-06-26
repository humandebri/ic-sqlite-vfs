#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SNAPSHOT="docs/PUBLIC_API_2_0.snapshot"

generate_core_snapshot() {
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
  done < <(
    {
      printf '%s\n' src/lib.rs src/config.rs src/stable/memory_manager.rs
      rg --files src/db -g '*.rs'
    } | sort
  )
}

generate_reexported_snapshot() {
  awk -v file="src/stable/raw_memory.rs" '
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
    {
      line = $0
      if (line ~ /^[[:space:]]*pub[[:space:]]+enum[[:space:]]+MemoryIdentity[[:space:]]*/) {
        in_identity = 1
        depth = 0
        print file ":" normalize(line)
      } else if (in_identity && line !~ /^[[:space:]]*(#|"|\)?\]|$|})/) {
        print file ":" normalize(line)
      }
      if (in_identity) {
        depth += count_char(line, "{") - count_char(line, "}")
        if (depth <= 0 && line ~ /}/) {
          in_identity = 0
          depth = 0
        }
      }

      if (line ~ /^[[:space:]]*pub[[:space:]]+trait[[:space:]]+Memory[[:space:]]*/) {
        in_trait = 1
        depth = 0
        print file ":" normalize(line)
      } else if (in_trait && line ~ /^[[:space:]]*(unsafe[[:space:]]+)?fn[[:space:]]+/) {
        print file ":" normalize(line)
      } else if (line ~ /^[[:space:]]*pub[[:space:]]+(const[[:space:]]+)?fn[[:space:]]+(custom|virtual_memory)[[:space:]]*\(/) {
        print file ":" normalize(line)
      }
      if (in_trait) {
        depth += count_char(line, "{") - count_char(line, "}")
        if (depth <= 0 && line ~ /}/) {
          in_trait = 0
          depth = 0
        }
      }
    }
  ' src/stable/raw_memory.rs

  awk -v file="src/stable/memory.rs" '
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
    {
      line = $0
      if (line ~ /^[[:space:]]*pub[[:space:]]+type[[:space:]]+DbMemory[[:space:]]*=/) {
        print file ":" normalize(line)
      }
      if (line ~ /^[[:space:]]*pub[[:space:]]+enum[[:space:]]+StableMemoryError[[:space:]]*/) {
        in_error = 1
        depth = 0
        print file ":" normalize(line)
      } else if (in_error && line !~ /^[[:space:]]*(#|"|\)?\]|$|})/) {
        print file ":" normalize(line)
      }
      if (in_error) {
        depth += count_char(line, "{") - count_char(line, "}")
        if (depth <= 0 && line ~ /}/) {
          in_error = 0
          depth = 0
        }
      }
    }
  ' src/stable/memory.rs

  awk -v file="src/stable/memory_layout.rs" '
    function normalize(text) {
      gsub(/[[:space:]]+/, " ", text)
      sub(/^ /, "", text)
      sub(/ $/, "", text)
      gsub(/ :/, ":", text)
      return text
    }
    {
      line = $0
      if (line ~ /^[[:space:]]*pub[[:space:]]+struct[[:space:]]+MemoryId/) {
        print file ":" normalize(line)
      }
      if (line ~ /^[[:space:]]*pub[[:space:]]+const[[:space:]]+fn[[:space:]]+new[[:space:]]*\(/) {
        print file ":" normalize(line)
      }
    }
  ' src/stable/memory_layout.rs
}

generate_snapshot() {
  {
    generate_core_snapshot
    generate_reexported_snapshot
  } \
    | rg -v '^src/lib\.rs:pub mod (api|bench_support|test_support);$' \
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
  echo "If this is intentional for 2.0, update $SNAPSHOT and docs/API_STABILITY.md."
  exit 1
fi
