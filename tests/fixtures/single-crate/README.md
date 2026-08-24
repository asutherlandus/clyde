# `single-crate`

**Property asserted:** for a project with no `[workspace]` table, the build
closure's root manifest is the crate's own `Cargo.toml`, and no member manifests
are pulled in.

This is the layout where "walk up to find the workspace root" has nothing to
find, and a closure computation that assumed a workspace would produce an empty
or wrong root.
