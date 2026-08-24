# `buildrs-hostile-write`

**Property asserted:** a `build.rs` cannot write outside the mission cache and
scratch. Every write it attempts elsewhere fails, and the marker files it tries
to create do not exist afterwards.

The build script writes to several plausible targets — the snapshot it is
compiling from, the workspace root, the host home, and `/tmp` outside the
sandbox's own tmpfs — because a boundary that holds for one and not the others
is not a boundary.
