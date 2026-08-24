# Phase 2: Snapshot Execution and Isolation Backends

## Goal

Make the autonomous edit / check / test loop both practical and isolated: untrusted project code executes against immutable snapshots, with no network and no credentials, fast enough that an agent can iterate.

Phase 2 has two sub-phases. **2a** brings up the pipeline on a namespace sandbox. **2b** adds the microVM backend behind the same trait, before any network-bearing task exists ([D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)).

Decisions in force: [D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only), [D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution), [D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift).

## Phase 2a: snapshots and the namespace sandbox

### 1. `SandboxBackend` trait

```rust
pub trait SandboxBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn isolation_level(&self) -> IsolationLevel;
    fn preflight(&self, spec: &SandboxSpec) -> Result<(), SandboxError>;
    async fn start(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;
    async fn wait(&self, handle: &SandboxHandle) -> Result<ExitStatus, SandboxError>;
    async fn terminate(&self, handle: &SandboxHandle) -> Result<(), SandboxError>;
}
```

`SandboxSpec` is backend-independent: runtime root closure, mount table with modes, egress profile, resource limits, environment, argv, and scratch policy. Anything a backend cannot honour is a `preflight` failure, never a silent relaxation — that rule is what keeps the trait from becoming a place where boundaries quietly weaken.

Backend selection is driven by the policy's `min_isolation`. There is no manual override in the MVP.

### 2. Bubblewrap backend

Namespace isolation (user, pid, ipc, uts, cgroup, net), read-only runtime root and snapshot binds, writable mission cache bind, tmpfs scratch, minimal `/dev`, `--clearenv`, `--new-session`, `--die-with-parent`, and a seccomp filter.

Resource limits come from cgroup v2 via systemd user delegation (`systemd-run --user --scope` with `MemoryMax`, `CPUQuota`, `TasksMax`), plus rlimits and a daemon-enforced wall-clock timeout.

Cgroup limits are **mandatory** for T2 and above. Without delegation, build and test tasks are refused — there is no rlimits-only fallback, because rlimits are a weaker bound rather than an equivalent one (`RLIMIT_NPROC` is per-user, `RLIMIT_AS` is per-process) and a flag that weakens a stated boundary tends to end up set ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)). The workspace environment may still run on rlimits and a timeout.

Bubblewrap is knowingly a weaker boundary than the design calls for on untrusted execution. It is acceptable here only because 2b follows immediately and nothing network-bearing ships on it.

### 3. Snapshot manager

- content-addressed store with BLAKE3 identity over the manifest
- materialisation by hardlink (default), reflink where the filesystem supports it, copy as fallback
- snapshots are **always** bound read-only; hardlink sharing with the content store makes this load-bearing, not stylistic
- exclusions applied before hashing and before grants

#### Canonical exclusion list
This is the authoritative list; other documents refer here rather than restating it. Exclusions are absolute — a subtree grant cannot admit an excluded path.

| Excluded | Reason |
|---|---|
| `target/`, `node_modules/`, `dist/`, `build/` | build output; belongs in the mission cache, not the snapshot |
| `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `credentials.json`, `secrets/` | secret-shaped; never an input to a build |
| `.git` | never available to build tasks, not pinnable ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)) |
| `.clyde/state` if present | Clyde's own state must never be a build input |
| configured additions from `snapshot.exclusions` | repository config may add exclusions, since narrowing is always permitted |

Repository config may **add** exclusions and never remove them.

#### Build-closure computation

Cargo cannot build a subtree in isolation. The closure for a task on `backend/auth` is:

- the workspace root `Cargo.toml` — `version.workspace`, `dependencies.*.workspace`, and `[lints] workspace` all resolve against it
- `Cargo.lock`, and `.cargo/config.toml` and `rust-toolchain.toml` from the root, since cargo discovers them by walking upward
- **every** workspace member's `Cargo.toml`, because cargo constructs the whole workspace graph before building anything and fails if a listed member's manifest is unreadable — manifests only, not their `src/`
- full source for the target crate and its transitive in-repo path dependencies, including their `build.rs`

This closure routinely exceeds the lease's edit scope, and that is expected: read-only input is a confidentiality delta, not an authority one, since the write set remains the lease's edit paths.

The closure is the **starting point for an access baseline**, not the snapshot contents. See the next section.

Getting closure computation wrong is the most likely cause of "Clyde can't build my project", so it deserves fixture-based tests across single-crate, virtual-workspace, inherited-workspace-dependency, and nested-path-dependency layouts.

### 3b. Access baseline: pin, enforce, detect drift

The primary threat for build tasks is code arriving in a transitive dependency and executing during compilation. What matters is detecting **change**, so build and test tasks run against a pinned access baseline ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

#### Repo read component: two tiers

The threat is dependency code changing; editing the project is the work, not the threat. So first-party code and out-of-scope reads are treated at different granularities ([D18](decisions.md#the-two-tiers-and-why)).

**Subtree grants** cover first-party project code: the mission-approved edit and read scopes, plus the build-closure subtrees within that scope. Files created, renamed, moved, or deleted inside a grant are ordinary work — materialised on the next snapshot, no prompt, no drift, no amendment. The human's review happened once, over the mission envelope's scope.

**File pins** cover everything outside those subtrees: an `include_str!` target in `docs/`, a shared fixture, a single sibling crate a build script reads. Each is confirmed individually and carries a recorded reason. A read outside grants ∪ pins is drift.

**Absolute exclusions** — `.env*` and similar secret-shaped files, key material, `target/`, `node_modules/`, `.git` by default — are applied *before* grants and cannot be admitted by one. A grant over `backend/auth` does not admit `backend/auth/.env.local`.

**Enforcement is by materialization**: the snapshot contains granted subtrees minus exclusions, plus pins, and nothing else, so a read outside fails with `ENOENT`. No tracing, no privilege, identical on both backends. Because grants are subtrees, new first-party files are picked up automatically — which is what keeps the inner loop prompt-free.

Baseline storage is Clyde-side state keyed by workspace, task type, and build target. It is never written into the repository, because repository content is untrusted and an attacker able to edit the baseline could conceal their own drift.

#### Code-execution inventory component
Computed **before the sandbox starts**, from the lockfile and the dependency bundle: every package with a `build.rs` or a proc-macro crate type, pinned by crate, version, and source content hash. Pre-execution ordering is the point — a newly arrived build script is caught before it runs.

Phase 2a computes and pins the inventory; the approval UX for inventory drift lands with the Phase 3 escalation flow.

#### Establishing a baseline
1. **Static proposal, the normal path.** Clyde proposes the computed build closure as the initial baseline for human confirmation.
2. **Learn mode, the fallback.** For reads static analysis cannot see — `include_str!` to an arbitrary path, a `build.rs` reading repo files — a human runs the task with auto-admit within the mission's read scope while Clyde observes actual reads via host-side `inotify` (`IN_OPEN`/`IN_ACCESS`) on the materialised tree, and produces a proposal for review.

Learn mode is a privilege: admin channel only, never reachable by an actor, never selected by Clyde as a fallback, single run, distinctly marked in the audit log, and without effect until a human confirms the proposal. The static-proposal path exists so that learn mode is rarely needed, since a learn run is a wide-scope execution of exactly the code being constrained.

A task with no baseline for its target is **refused**, with the static proposal offered. There is no implicit wide-scope first run.

#### Drift
In enforce mode, denial and drift are the same event. Clyde compares the failure against paths that exist in the workspace but were excluded from the snapshot, and renders an escalation naming the path rather than surfacing a raw cargo error. Under `ENOENT` inference this is a heuristic; exact reporting needs the FUSE path ([OQ5](decisions.md#oq5-per-open-enforcement-via-a-fuse-served-snapshot)).

**Not drift:** a new, renamed, moved, or deleted file inside a granted subtree; a new module or test inside one; a new in-repo path dependency on a crate inside the approved scope.

**Drift:** a read outside grants ∪ pins — including a new path dependency on an in-repo crate *outside* the approved scope, which is a real widening of what the mission reaches into; a new code-executing crate; a version change to one; a content-hash change at the same version.

Path drift and dependency drift are presented distinctly. The first should be uncommon once grants are set; the second is what the control exists for.

#### CLI
Admin channel only: `clyde access show`, `clyde access propose`, `clyde access learn`, `clyde access review`, `clyde access confirm`, `clyde access reset`.

### 4. Per-mission cache

At mission activation, create `missions/<id>/`:

- `cargo-home/` — seeded by hardlink from the read-only dependency bundle store, writable because cargo writes lock files into `CARGO_HOME` even offline
- `cargo-target/` — `CARGO_TARGET_DIR`, shared across task runs within the mission

Both are destroyed at mission closeout. Size counts against `max_cache_bytes` ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)).

### 4b. Dependency bundle import

Phase 2a has no fetch task, so offline builds need a bundle to exist. `clyde deps import` takes a host-produced cargo cache or vendor directory, content-addresses it into the read-only bundle store, and records the `Cargo.lock` digest it satisfies.

This is a human, admin-channel operation, and it is also what makes the code-execution inventory computable in Phase 2a. Phase 3 replaces it as the normal path but it stays useful for air-gapped setups.

### 5. `rust.check` and `rust.test.unit`

Offline by construction: egress profile `none`, `CARGO_NET_OFFLINE=true`, `--offline --frozen`. A missing dependency must fail rather than trigger a fetch — that failure is the entry point to Phase 3.

### 6. Structured failure classification

Task outcomes carry `TaskFailureClass` ([schema reference](schema-reference.md#task-outcome-and-failure-classification)). Phase 2a must reliably distinguish:

- `ProjectCodeError` — compilation or test failure in project code
- `MissingDependencies` — cargo cannot proceed offline with the present cache; drives Phase 3
- `EgressBlocked` — something attempted network access; for a `none`-profile task this is a finding, not a routine error
- `GitMetadataUnavailable` — the build tried to read `.git`, which is never available ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)). Diagnosed specifically, because it is neither drift nor a bug in the user's code, and git metadata support is a known MVP gap
- `ResourceExhausted`, `SandboxFailure`, `Internal` — never reported to the user as a problem with their code

Classification is derived from exit status plus structured cargo diagnostics — invoke with `--message-format=json-diagnostic-rendered-ansi` (or plain `json`) and match on diagnostic codes rather than scraping human-readable output, which changes between toolchain versions. Covered by fixture tests, because Phase 3's escalation flow keys off it.

### 7. Task execution pipeline, logs, artifacts

Admission (lease, policy, budget) → snapshot → sandbox spec → start → stream logs → collect outputs → classify → store artifacts → record audit events. Logs stream to the actor while running, are persisted as artifacts, and are bounded in size with truncation recorded rather than silent.

Budget is charged at admission, before execution, so a daemon crash cannot lose the charge.

### 8. Task CLI and actor surface

`clyde task run|status|logs|list`, and the equivalent MCP tools returning structured outcomes with the policy digest actually applied — so "which policy ran" is answerable from the result, not inferred.

## Phase 2b: microVM backend

### 1. Firecracker backend

A second `SandboxBackend` implementation with the same `SandboxSpec` contract:

- guest kernel and rootfs built from the same nix closure as the namespace backend's runtime root ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)), so runtime-root identity is stable across backends
- snapshot and cache surfaces passed as block devices or a host-shared mount, read-only where the spec says read-only
- **no network device in the guest at all**; egress, when a profile permits it, is vsock-bridged to the host proxy ([network egress model](network-egress-model.md#firecracker-phase-2b))
- resource limits from VM configuration rather than cgroups
- boot-time and teardown budgets tracked, because inner-loop latency is the acceptance criterion for this sub-phase

### 2. Policy-driven selection

`min_isolation: MicroVm` on a policy means the microVM backend runs it, or the task is refused. It never silently downgrades to the namespace backend. Downgrade requires an explicit configuration decision recorded in the audit log, and is not available for T3 tasks.

### 3. Parity and performance

Parity for `rust.check`, `rust.test.unit`, and the workspace environment. Latency measured for cold and warm mission cache, and reported — if the microVM inner loop is not usable, that is a finding for the roadmap, not something to discover during Phase 3.

Baseline enforcement carries over unchanged, since materialization is backend-independent. Learn-mode *observation* does not: host-side inotify cannot see guest reads. Until the FUSE/virtio-fs path exists ([OQ5](decisions.md#oq5-per-open-enforcement-via-a-fuse-served-snapshot)), learn mode runs on the namespace backend and the resulting baseline is used on both.

## Security properties this phase must demonstrate

- project code executes only against read-only snapshots, never the live workspace
- no egress whatsoever from `rust.check` / `rust.test.unit`, verified by a test that attempts a connection from inside the sandbox and asserts both the failure and the `EgressBlocked` record
- no credential material, host home, `~/.ssh`, `~/.gnupg`, browser profile, or container socket is reachable from a build sandbox, verified by a test that looks for each
- a hostile `build.rs` fixture cannot write outside the mission cache and scratch, and cannot read the workspace outside the snapshot
- cache state does not cross missions or projects, verified by a fixture that writes a marker in one mission and asserts its absence in the next
- snapshot content is immutable during a run, and the content store is not modified through a hardlink
- a build script reading a repo path outside grants ∪ pins fails, and the failure is rendered as a named escalation rather than a raw cargo error
- creating, renaming, and deleting files inside a granted subtree produces **no** drift and **no** prompt across a full edit/check/test loop, verified by a fixture that adds modules and tests between runs
- an absolute exclusion inside a granted subtree is not materialised, verified with a `.env.local` fixture
- a `build.rs` reading `.git` fails with `GitMetadataUnavailable`, and no configuration or pin can admit `.git`
- with cgroup delegation unavailable, a T2 task is refused and the workspace environment still starts
- a fixture that adds a `build.rs` to a previously script-free dependency is caught **before execution** by the inventory check
- a fixture that changes a build script's content at the same crate version is caught by the content hash
- learn mode cannot be initiated by an actor, and its proposals have no effect until confirmed

## Exit criteria

### 2a
- `rust.check` and `rust.test.unit` run from immutable, closure-aware snapshots on the namespace backend
- an agent iterates edit → check → test autonomously within a lease, with no repeated approval
- failure classification distinguishes project errors from missing dependencies, with fixture tests
- per-mission cache makes the second and later iterations materially faster than the first, with a measured figure recorded
- an access baseline can be proposed from the static closure, confirmed, and enforced; a task with no baseline is refused rather than run wide
- first-party editing generates no baseline prompts: a loop that adds source files, tests, and modules inside the approved scope runs uninterrupted
- learn mode records a baseline for a fixture whose `build.rs` reads a repo file that static analysis cannot see
- the code-execution inventory is pinned and drift is detected pre-execution for all three drift cases
- every security property above holds under test on the namespace backend
- mission closeout deletes the cache and stores the closing diff

### 2b
- the microVM backend passes the same suite as 2a, including the egress and credential-absence tests
- policy-driven selection works, with no silent downgrade path
- inner-loop latency on the microVM backend is measured and reported for cold and warm cache
- the namespace backend remains available for the workspace environment and for hosts without KVM

## Explicitly not in Phase 2

No dependency fetch (that is Phase 3 and requires egress), no broker operations, no frontend or browser tasks, no TUI.
