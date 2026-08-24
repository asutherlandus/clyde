# Phase 3: Separate Dependency Resolution

## Goal

Enforce the fetch-versus-compile split: compilation and testing never reach the network, dependency retrieval is a distinct approval-gated task with a narrow allowlisted egress profile, and its output is an explicit artifact.

This is the phase that most directly addresses the Rust supply-chain threat model, because it removes any legitimate reason for a compile step to have network access.

Decisions in force: [D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only), [D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution), [D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift).

## Prerequisite

`rust.resolve-deps` has `min_isolation: MicroVm`. Phase 2b must be complete before this phase begins: this is the first task that both executes untrusted code paths *and* has any network reachability, and it should not ship on the namespace backend.

## Deliverables

### 1. `rust.resolve-deps`

- **input snapshot**: manifests, `Cargo.lock`, and cargo configuration only — not the full source tree. A fetch task has no reason to see application code, and narrowing the input narrows what a compromised fetch can exfiltrate through an allowlisted registry connection.
- **egress**: `rust-registry` profile ([network egress model](network-egress-model.md#egress-profiles))
- **outputs**: a dependency bundle artifact, a fetch manifest, and nothing else
- **approval**: human, by default
- **credentials**: none; private sources are out of MVP scope and must be denied rather than half-supported

Cargo runs with `--locked` so the lockfile is authoritative and resolution cannot drift during a fetch.

### 2. Dependency bundle artifact

Content-addressed, immutable, and mounted read-only wherever it is used ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)). The bundle records:

- the `Cargo.lock` digest it satisfies
- the set of crates with name, version, and content hash
- the registries each crate came from
- the fetch manifest, including every egress attempt

A build task states which bundle it used, so "what were the inputs to this build" is answerable from the task run alone.

### 3. Lockfile-aware fetch policy

Before fetching, compare the requested lockfile against the previously satisfied one and classify the change: unchanged, additions only, version changes, source changes, or a new git dependency.

The classification appears in the approval prompt, because "fetch dependencies" and "fetch dependencies, including four new crates from a source you have not used before" deserve different scrutiny from the human, and only Clyde is in a position to tell them apart.

### 3b. Code-execution inventory diff

The higher-signal companion to the lockfile diff ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). After a fetch, and before any build task runs against the new bundle, Clyde recomputes the inventory of packages that execute code at build time — `build.rs` and proc-macro crates, by crate, version, and source content hash — and diffs it against the pinned baseline.

The approval prompt reports it in those terms:

```text
Lockfile: 4 additions, 1 version change, 0 source changes
Code execution at build time:
  + serde_derive_internals 0.29.1   (new proc-macro crate)
  ~ ring 0.17.8 -> 0.17.9           (build.rs content changed)
  ! zstd-sys 2.0.10                 (same version, different source hash)
```

A lockfile addition of a crate that never executes code is a materially different risk from one that runs a build script, and the human should not have to work that out from crate names. The `!` case — same version, different content — is a registry-tampering signal and should be visually distinct.

Ordering matters: the inventory diff is a pre-execution check, so a newly arrived build script is surfaced before it has run.

Configuration may pre-approve the low-risk classes for a mission ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)); a repository may narrow that but never widen it. Git dependencies and unknown registries are never pre-approvable.

### 4. Escalation flow

Driven by the `MissingDependencies` classification from Phase 2a:

```text
rust.check fails with MissingDependencies
  → agent calls request_escalation(rust.resolve-deps, reason, scope)
  → policy engine evaluates: allowed with human approval, egress rust-registry
  → approval prompt on the admin socket shows:
      task, input scope, egress profile and exact host allowlist,
      credentials (none), outputs (bundle only), lockfile change classification,
      and the stated caveat that allowlisting is by destination, not content
  → human approves once / for mission / denies
  → side lease or lease amendment recorded, bound to the approval digest
  → rust.resolve-deps runs
  → agent reruns rust.check with egress `none` and the new bundle
```

The prompt must explain *why* the previous profile failed. An approval request that does not say what went wrong invites reflexive approval, which is the failure mode this whole design exists to avoid.

### 5. Rerun path

After a successful fetch, the mission's `CARGO_HOME` is re-seeded by hardlink from the new bundle and the offline build reruns with egress `none`. The agent should be able to do this without a further approval, because the boundary crossing was the fetch, not the compile — **unless** the inventory diff is non-empty, in which case the human confirms the new inventory before any build task runs against the new bundle.

That is the point of the whole phase for this threat model: fetching a hostile crate is harmless until something executes it, and the confirmation sits in between.

### 6. Egress accounting

Byte and connection budgets are enforced per task and charged to the lease. The fetch manifest records every destination and every refusal. A refusal during `rust.resolve-deps` — a crate attempting a source that is not allowlisted — is surfaced prominently in the task result and in mission review, not buried in a log.

## Security properties this phase must demonstrate

- `rust.check` and `rust.test.unit` remain at egress profile `none` and cannot fetch, verified by a fixture whose lockfile requires an absent crate: the build must fail with `MissingDependencies`, not succeed
- the fetch sandbox can reach allowlisted registry hosts and nothing else, verified by attempting a non-allowlisted host and asserting refusal plus an `EgressAttempt` record
- the fetch sandbox holds no credentials, and a private-source requirement is denied rather than silently attempted
- the fetch task's input snapshot contains no application source, verified by inspecting the manifest
- the dependency bundle is read-only wherever mounted, and a task cannot mutate it
- approval is bound to the exact request: a lockfile change after approval invalidates the approval rather than being fetched under it
- a fetch that introduces a new build-script or proc-macro crate does not permit a build against the new bundle until the inventory change is confirmed, verified by a fixture
- a same-version content change in a code-executing crate is detected and reported distinctly from a version change

## Exit criteria

- compile and test never fetch, and fail informatively when dependencies are absent
- the full escalation cycle — offline failure, escalation, narrow approval, fetch, offline rerun, success — works end to end and is legible in mission review
- the approval prompt shows the exact host allowlist, the lockfile change classification, and the destination-only caveat
- egress attempts, allowed and refused, are recorded for every fetch and visible in the task result
- pre-approval of low-risk lockfile classes works per configuration, and cannot be widened by repository configuration
- the approval prompt reports the code-execution inventory diff alongside the lockfile diff, distinguishing new, changed-version, and changed-content-at-same-version cases
- no build task runs against a bundle whose inventory diff is unconfirmed
- git dependencies and unknown registries are refused with an actionable explanation

## Explicitly not in Phase 3

No private registry or private git credential support (brokered private access is post-MVP), no `node.resolve-deps`, no vendoring workflow, no SBOM generation beyond the bundle contents.
