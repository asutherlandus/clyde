# `hostile-git`

**Property asserted:** a repository whose `.git/hooks` and `.git/config` are
attacker-controlled executes **nothing** during a brokered push, and the push
lands on the allowlisted remote rather than on the decoy the repository's own
`url.*.insteadOf` names.

The fixture is built by `build.sh` rather than committed, because a committed
`.git` directory inside a git repository is not something git will track. The
script creates: eight hooks that each touch a marker file, a `url.*.insteadOf`
rewrite pointing at a decoy remote, a `core.sshCommand` that touches a marker, an
`uploadpack.packObjectsHook`, and content filters.

The property is that afterwards the marker directory is empty. The same fixture
is built inline by `crates/clyde-git/tests/hostile_repository.rs`, which is where
it is asserted; this script exists so it can be reproduced by hand.
