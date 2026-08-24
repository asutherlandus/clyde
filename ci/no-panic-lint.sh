#!/usr/bin/env bash
# Fails on unwrap()/expect()/panic!()/todo!()/unimplemented! outside test code in
# the crates/ and bin/ trees (AGENTS.md: production code must not panic).
#
# Test code is identified structurally rather than by heuristics:
#   - everything from the first `#[cfg(test)]` line in a file onward, which is
#     where every crate here keeps its trailing `mod tests` block;
#   - whole files that are only compiled under `cfg(test)`, by convention named
#     `tests.rs` or living under a `tests/` directory.
#
# A production call site that genuinely needs `expect` must carry an
# `// PANIC-JUSTIFIED: <invariant>` comment on the same line, so the invariant is
# documented where a reviewer will see it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

while IFS= read -r file; do
  awk '
    /^[[:space:]]*#\[cfg\(test\)\]/ { exit }
    { print FILENAME ":" FNR ":" $0 }
  ' "$file"
done < <(find crates bin -name '*.rs' \
              -not -path '*/tests/*' \
              -not -name 'tests.rs' \
              -print | sort) \
  | grep -E '\.unwrap\(\)|\.expect\(|panic!\(|todo!\(|unimplemented!\(' \
  | grep -v 'PANIC-JUSTIFIED' \
  | grep -v 'unwrap_or' \
  | grep -v 'expect_err' \
  > "$report" || true

if [ -s "$report" ]; then
  echo "panic-prone constructs found in production code:" >&2
  cat "$report" >&2
  echo >&2
  echo "Fix by returning a typed error, or justify with '// PANIC-JUSTIFIED: <invariant>'." >&2
  exit 1
fi

echo "no-panic lint: clean"
