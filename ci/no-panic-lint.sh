#!/usr/bin/env bash
# Fails on unwrap()/expect()/panic!()/todo!()/unimplemented! outside test code in
# the crates/ and bin/ trees (AGENTS.md: production code must not panic).
#
# Test modules opt out by living behind `#[cfg(test)]` in a `mod tests` block, or
# by being under a `tests/` directory. Production call sites that genuinely need
# `expect` must carry an `// PANIC-JUSTIFIED:` comment on the same line, which
# documents the invariant at the use site where a reviewer will see it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

status=0

while IFS= read -r file; do
  # Strip everything from the first `#[cfg(test)]` module onward. Every crate in
  # this repository keeps its tests in a single trailing `mod tests` block, which
  # this lint relies on and which the layout test asserts.
  awk '
    /^#\[cfg\(test\)\]/ { exit }
    { print FILENAME ":" FNR ":" $0 }
  ' "$file"
done < <(find crates bin -name '*.rs' -not -path '*/tests/*' -print | sort) \
  | grep -nE '\.unwrap\(\)|\.expect\(|panic!\(|todo!\(|unimplemented!\(' \
  | grep -v 'PANIC-JUSTIFIED' \
  | grep -v 'unwrap_or' \
  | grep -v 'expect_err' \
  > /tmp/clyde-panic-lint.out || true

if [ -s /tmp/clyde-panic-lint.out ]; then
  echo "panic-prone constructs found in production code:" >&2
  cat /tmp/clyde-panic-lint.out >&2
  echo >&2
  echo "Fix by returning a typed error, or justify with '// PANIC-JUSTIFIED: <invariant>'." >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "no-panic lint: clean"
fi
exit $status
