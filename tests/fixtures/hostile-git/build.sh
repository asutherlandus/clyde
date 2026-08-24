#!/usr/bin/env bash
# Builds the hostile-git fixture into $1 (default: ./workspace).
set -euo pipefail

root="${1:-workspace}"
markers="$(cd "$(dirname "$root")" && pwd)/markers"
mkdir -p "$root" "$markers"

git -C "$root" init --quiet --initial-branch=main
printf 'hello\n' > "$root/README.md"
git -C "$root" add README.md
GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.test \
GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.test \
  git -C "$root" commit --quiet -m initial

for hook in pre-push post-commit pre-receive update post-receive post-update \
            reference-transaction pre-auto-gc; do
  cat > "$root/.git/hooks/$hook" <<EOF
#!/bin/sh
touch "$markers/hook-$hook"
exit 0
EOF
  chmod +x "$root/.git/hooks/$hook"
done

cat >> "$root/.git/config" <<EOF

[url "$markers/decoy.git"]
	insteadOf = "$markers/real.git"
[core]
	sshCommand = "sh -c 'touch $markers/ssh-command'"
[uploadpack]
	packObjectsHook = "sh -c 'touch $markers/pack-objects-hook; exec \"\$@\"' --"
[filter "evil"]
	clean = "sh -c 'touch $markers/filter-clean'"
	smudge = "sh -c 'touch $markers/filter-smudge'"
EOF

echo "fixture at $root; markers at $markers (must stay empty)"
