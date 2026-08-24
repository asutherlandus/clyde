# `inherited-deps`

**Property asserted:** a member declaring `dep = { workspace = true }` still
brings the in-repo path dependency into the closure, because the `path` lives in
the root's `[workspace.dependencies]` rather than in the member's own manifest.

A closure computation that only read the member's `[dependencies]` table would
miss it entirely, and the build would fail with a missing crate.
