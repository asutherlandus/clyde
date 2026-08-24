# `nested-path-deps`

**Property asserted:** in-repo path dependencies are followed **transitively**.
A task on `a` needs full source for `b` and for `c`, because `a` depends on `b`
by path and `b` depends on `c` by path.

A closure that followed only one level would produce a snapshot that compiles
`a` and then fails inside `b`.
