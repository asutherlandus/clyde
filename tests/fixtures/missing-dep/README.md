# `missing-dep`

**Property asserted:** a lockfile requiring a crate that is absent from the
dependency bundle fails with `MissingDependencies` rather than succeeding, and
rather than reaching the network to fetch it.

That classification is the entry point to Phase 3's escalation flow, so the
distinction between "the code is wrong" and "the dependencies are not here" has
to be reliable — it is what decides whether the agent fixes its code or asks for
a fetch.
