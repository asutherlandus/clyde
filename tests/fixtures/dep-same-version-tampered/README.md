# `dep-same-version-tampered`

**Property asserted:** identical crate, identical version, different source
content is detected by the pinned content hash and reported as
`CodeExecContentChanged` — distinctly from a version change, and rendered with
`!` rather than `~`.

A same-version content change is a registry-tampering signal, not an upgrade, and
presenting the two identically would bury the one that matters.
