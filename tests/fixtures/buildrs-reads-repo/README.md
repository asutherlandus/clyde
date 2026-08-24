# `buildrs-reads-repo`

**Property asserted:** a `build.rs` reading a repository file outside its own
crate is a read the manifest does not describe. The build fails under a
closure-derived baseline, and the failure names the path.

This is path drift rather than dependency drift, and the two are presented
differently: this one should be uncommon once grants are set, while dependency
drift is what the control exists for.
