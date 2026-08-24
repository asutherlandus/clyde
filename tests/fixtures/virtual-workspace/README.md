# `virtual-workspace`

**Property asserted:** a task on one member pulls in **every** member's manifest
— manifests only, not their sources — because cargo constructs the whole
workspace graph before building anything and fails if a listed member's manifest
is unreadable.

The second member's `src/` must **not** appear in the closure: admitting it would
widen the read set past what the build needs.
