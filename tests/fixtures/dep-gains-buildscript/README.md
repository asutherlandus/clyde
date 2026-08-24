# `dep-gains-buildscript`

**Property asserted:** a dependency that gains a `build.rs` at the same version
appears as `NewCodeExecCrate` in the inventory diff, and is caught **before the
sandbox starts** rather than after the script has run.

This is the threat the whole baseline control exists for: malicious code arriving
in a transitive dependency and executing during compilation. Pre-execution
ordering is the point.

Two states are provided. `before/` is the vendored source with no build script;
`after/` is the same crate at the same version with one added. The lockfile is
identical between them, so nothing a lockfile diff can see has changed.
