# Test fixtures

Each fixture asserts one named property, and states it in its own `README.md`, so
a failing test explains itself rather than sending a reader to the test source to
work out what was meant.

They are a Phase 0 deliverable because several phases share them: the project
layouts drive build-closure computation in Phase 2a, the hostile-execution
fixtures drive the isolation properties, the dependency-drift fixtures drive the
inventory check that Phase 3's approval prompt reports, and `hostile-git` drives
Phase 4's broker hardening.

| Fixture | Property |
|---|---|
| `single-crate` | a project with no `[workspace]` has its own manifest as the closure root |
| `virtual-workspace` | every member's manifest is in the closure; other members' sources are not |
| `inherited-deps` | `workspace = true` inheritance still brings in the path dependency |
| `nested-path-deps` | in-repo path dependencies are followed transitively |
| `include-str-outside` | a read static analysis cannot see fails, and is named |
| `buildrs-reads-repo` | a build script reading outside its crate is path drift |
| `secret-shaped-files` | a subtree grant does not admit `.env.local`, `*.pem`, or `id_rsa` |
| `churn` | editing inside a grant produces **zero** drift prompts |
| `buildrs-hostile-write` | a build script cannot write outside the cache and scratch |
| `buildrs-egress` | a build script cannot reach the network, and the attempt is a finding |
| `buildrs-credential-hunt` | no credential path is reachable from a build sandbox |
| `dep-gains-buildscript` | a newly arrived build script is caught before it runs |
| `dep-same-version-tampered` | same version, different content is detected and reported distinctly |
| `missing-dep` | an absent crate fails as `MissingDependencies`, not as a project error |
| `hostile-git` | hostile hooks and config execute nothing during a brokered push |

## Why some fixtures have two states

`dep-gains-buildscript` and `dep-same-version-tampered` each provide `before/`
and `after/` vendor directories with an identical lockfile. That is deliberate:
the whole point is that nothing a lockfile diff can see has changed, so the
detection has to come from the code-execution inventory and its content hashes.
