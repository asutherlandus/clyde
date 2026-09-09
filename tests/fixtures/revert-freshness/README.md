# `revert-freshness`

**Property asserted:** reverting a file to content the store already holds forces
a rebuild, and never reports a pass for a tree that was not built
([D27](../../../docs/builder/decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)).

Cargo decides freshness for local sources by comparing source mtimes against
build output mtimes; it has no content-hash freshness mode on the stable
toolchain. Content-addressed materialisation breaks the assumption that makes
that work, because an unchanged file's mtime comes from the blob's ingest time
rather than from the working tree.

The dangerous direction is a revert. Edit `lib.rs`, build, then `git checkout --`
it: the snapshot re-links a blob ingested *before* the outputs, so the source
looks older than the build products and cargo calls the crate fresh. The task
then reports success for a tree it never compiled.

That matters beyond a stale binary. Task evidence is what a push approval rests
on, so a stale pass is an integrity failure and not only a nuisance — it is the
one failure mode in this pipeline that produces a *confident wrong answer* rather
than an error.

The sequence the fixture exists for:

1. build — `answer()` returns `42`
2. edit `lib.rs` so `answer()` returns `43`, build again
3. revert `lib.rs` to its original bytes, build a third time

The third build must recompile. Under plain hardlink materialisation it would
not, because step 3's snapshot identity is identical to step 1's and every blob
it names already exists with an old mtime.
