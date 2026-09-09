# Builder: Roadmap and Phase Specs

The phased build order for the builder, with each phase's deliverables, security properties, and exit criteria.

## The two products

The MVP ships as **two products** ([D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)), previously called Part 1 and Part 2.

**The builder** — immutable snapshots, access baselines, the code-execution inventory, typed build tasks, the fetch/compile split, artifacts, the credential broker, and the audit chain. Driven by a human operator on the admin socket, or by any machine driver over the actor socket. It has no notion of an agent process.

**The warden** — hosting a coding agent inside a Clyde-managed workspace environment. It closes the bypass the builder can only report. See [its spec](../warden/spec.md).

They split because they defend against different attackers: the builder's is the **dependency**, actively hostile and inside the workload, needing an execution boundary; the warden's is the **agent**, usually careless rather than hostile, the driver rather than the workload, needing an authority boundary. Containing the agent is not load-bearing against the first threat — a hostile `build.rs` does not care who requested the build.

### Posture, and why it is in this document

The builder without the warden is a large, genuine improvement and it is **not** what [D1](../warden/decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment) promised. A driver with `cargo` on `PATH` can route around the pipeline entirely.

So the deployment's **posture** is explicit, reported by `clyde doctor`, recorded on every task run, and stated in mission review ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)): `enforcing` when the driver cannot reach a project build toolchain outside Clyde, `advisory` when a driver can bypass the pipeline and Clyde says specifically how.

**Nothing in this roadmap should be read as delivering `enforcing` posture before the warden exists.**

## Phase order

| Phase | Summary |
|---|---|
| [0](#phase-0-foundations) | flake, CI, crate layout, typed schemas, pure policy functions, `clyde doctor` |
| [1](#phase-1-mission-lease-and-approval-core) | missions, leases, tokens, the operator and actor surfaces, confirmations, egress proxy, CLI |
| [1a](#part-1a-snapshots-and-the-namespace-backend) | snapshots, access baselines, the namespace backend, `rust.check`, `rust.test.unit`, per-mission cache |
| [1b](#part-1b-the-microvm-backend-as-the-default) | the microVM backend as the default for every build task, with measured inner-loop latency |
| [3](#phase-3-separate-dependency-resolution) | `rust.resolve-deps`, dependency bundles, registry-only egress, the escalation flow |
| [4](#phase-4-credential-broker-and-brokered-git-push) | broker gateway, brokered `git.push`, hostile-repo hardening, TUI, mission review |

The phase numbering is retained because the commit history and code comments cite it. Parts 1a and 1b are the same work as the former Phases 2a and 2b; there is no Phase 2.

## Principles

- preserve the core trust boundaries from the beginning
- make the safe inner loop usable as early as possible
- separate execution from authority before adding advanced features
- keep the build pipeline drivable without an agent, so the most important subsystem does not depend on the least essential one
- start with a narrow task catalog and deepen it incrementally
- optimize for one strong end-to-end workflow before broad platform coverage
- prefer explicit policy even when initial policy is simple
- never ship a network-bearing task on a boundary weaker than intended
- never let the weaker posture be the posture you are silently in

## What the MVP must prove

**The builder**

1. Untrusted build and test execution happen in isolated sandboxes against immutable snapshots.
2. Dependency fetch is separated from compile/test.
3. Credentials are not mounted into build environments.
4. Git push happens through a brokered path.
5. Boundary crossings trigger approvals or confirmations instead of silently inheriting privilege.
6. The task and audit history remain visible enough for a human to trust the system.
7. A human, CI, or an external agent can drive the whole pipeline without Clyde hosting anything.

**The warden** adds: a human can delegate a bounded mission to an agent; the agent can autonomously perform edit/check/test loops within a lease; and credentials and the build toolchain are unreachable from the agent's environment, so typed tasks are the only path to project execution.

## MVP scope

**The builder's primary workflow** — a Rust project; one human driving the pipeline from the CLI, and optionally an external driver holding a session token; mission creation and one active lease per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace)); `rust.check` and `rust.test.unit` against immutable, closure-aware snapshots on microVM isolation; separate, confirmation-gated dependency resolution; brokered `git.push`; task logs, egress records, posture, and a mission review surface; confirmation prompts on a channel no sandbox can reach.

**The warden adds** — one primary coding agent hosted in a Clyde-managed workspace environment; `AGENTS.md` guidance support; repeated editing inside lease scope enforced by mount topology; the same typed tasks requested by the agent; one level of sub-agent derivation; `enforcing` posture.

**Out of scope for both** — release publishing and signing; multi-agent orchestration beyond one level of sub-agent; broad language ecosystem coverage; enterprise policy federation; a remote runner fleet; browser and synthetic-service testing.

---

# Phase 0: Foundations

Establish the toolchain, repository structure, and typed data model every later phase builds against — with **no runtime behaviour**. Phase 0 is complete when the schemas are executable and checkable, CI enforces the project's standards, and every interface contract the later phases depend on has been written down.

Decisions in force: [D4](decisions.md#d4-phase-0-delivers-flake-ci-crate-layout-and-typed-schemas), [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only), [D15](decisions.md#d15-three-binaries-from-phase-0), [D16](decisions.md#d16-one-active-mission-per-workspace), [D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden), [D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded).

Phase 0 defines the `workspace` runtime root and the workspace task types even though they belong to the warden, because the catalog is closed and the runtime-root assertion is what makes the warden's central property mechanical rather than aspirational.

### 1. Nix flake

The flake is the source of truth for all tooling. It must provide:

- a pinned Rust toolchain from a pinned `nixpkgs`, with `clippy` and `rustfmt`. Use nixpkgs' Rust unless a specific version is needed that nixpkgs does not carry, in which case add `fenix`. Edition 2024. There is no separate MSRV policy: the flake-pinned toolchain *is* the supported version
- `cargo-nextest`, `cargo-deny`, `cargo-insta`
- `bubblewrap`, `sqlite`, `git`, `jq`
- from Part 1b, guest image tooling: `e2fsprogs` for `mke2fs -d`, which builds the source and mission-cache images unprivileged, and `erofs-utils` for `mkfs.erofs`, which builds the read-only roots ([R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)). Artifacts do not come back this way ([R11](decisions.md#r11-all-guest-output-leaves-over-vsock))
- a `devShell` that is the only supported development entry point
- `checks` outputs mirroring each CI job, so `nix flake check` reproduces CI locally

It must also define the runtime roots ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)), even though nothing executes them until Part 1a, and Part 1b builds the guest images from the same closures so runtime-root identity is stable across backends: `runtimeRoots.workspace` (text/code manipulation tooling, demonstrably no project build toolchain), `runtimeRoots.rust`, and `runtimeRoots.fetch`. Each exposes its store path and closure manifest so a task policy can reference it by content identity.

### 2. Runtime root assertion test

A test that inspects the `runtimeRoots.workspace` closure and fails if it contains `cargo`, `rustc`, `rustup`, `node`, `npm`, `pnpm`, `yarn`, a browser, `gpg`, `ssh`, `docker`, or `podman`.

This is the mechanical form of what the design documents state as a hard architectural requirement. **Until it is a test, it is a hope.**

### 3. Crate layout

```text
crates/
  clyde-core/        ids, entities, state machines, validation, error types
  clyde-policy/      built-in task policies, policy resolution, lease derivation,
                     egress profile ordering, approval requirements
  clyde-store/       repository traits + SQLite impl + in-memory impl, migrations
  clyde-snapshot/    content store, snapshot manifest, materialisation strategies
  clyde-sandbox/     SandboxBackend trait, cgroup/rlimit helpers  (backends: Part 1a)
  clyde-egress/      egress profiles, proxy, forwarder wire protocol  (Phase 1)
  clyde-git/         the single place that constructs a git command (R2)
  clyde-api/         wire types, actor-facing view types, MCP tool + JSON-RPC defs
  clyde-broker-api/  broker request/response types, shared by clyded and brokerd
bin/
  clyde/             CLI  (TUI added in Phase 4)
  clyded/            control plane daemon
  clyde-brokerd/     broker daemon (stub in Phase 0)
  clyde-forward/     in-sandbox egress forwarder (part of runtime roots)
nix/                 runtime root derivations
tests/fixtures/      fixture projects, each stating the property it asserts
bin/clyded/tests/    cross-crate integration tests, which need the daemon
```

Dependency direction is strictly one way: `core` ← `policy` ← everything else. `clyde-policy` must not depend on `clyde-store`, `clyde-sandbox`, or `tokio`, so policy decisions are pure and testable without I/O.

**No library crate may have a notion of an agent process.** The product split lives at the daemon layer: agent hosting, sub-agent derivation, and the workspace-environment sandbox specification belong in `bin/clyded`, in modules narrow enough that extracting them into a separate binary later is mechanical. A crate under `crates/` that needs to know whether an agent exists is a sign the boundary has slipped.

`clyde-brokerd` exists from Phase 0 as a stub answering capability queries only ([D15](decisions.md#d15-three-binaries-from-phase-0)).

### 4. Entity types

Every entity in [schema.md](schema.md) — including `AccessBaseline`, `CodeExecInventory`, and `AccessDrift` — as Rust types with typed newtype identifiers; `serde` with `deny_unknown_fields` on all config and request types; validation on construction returning typed errors; state machines as explicit transition functions returning `Result`, not mutable field assignment; and redaction-safe `Display`.

Credential-bearing types (from Phase 4) must not implement `Serialize` or a revealing `Debug`. Phase 0 establishes the pattern with a `Redacted<T>` wrapper.

### 5. Policy resolution and lease derivation

Pure functions, fully unit-tested in Phase 0 even though nothing calls them yet:

- `resolve_task_policy(task_type, repo_path, mission, lease, config) -> Result<ResolvedPolicy, PolicyReason>`
- `derive_lease(parent, request) -> Result<Lease, PolicyReason>`, implementing all eight [derivation rules](schema.md#derivation-rules-normative)
- `egress_profile_order(a, b) -> Option<Ordering>`, implementing the [profile ordering](tasks-and-policy.md#profile-ordering) including the incomparable pairs
- `charge_budget(budget, usage, cost) -> Result<BudgetUsage, PolicyReason>`

These four functions are where the security model actually lives. They are the highest-value test targets in the project and should be tested against denial cases first.

### 6. MVP task catalog

The closed catalog from [tasks-and-policy.md](tasks-and-policy.md#the-mvp-catalog), encoded as a Rust enum with a built-in policy per variant. `min_isolation` is the *minimum* the policy demands; the sandbox manager may select something stronger.

`rust.check` and `rust.test.unit` carry `NamespaceSandbox` through Part 1a because that is the floor Part 1a can meet on a host without guest images. Raising both to `MicroVm` is a [Part 1b](#part-1b-the-microvm-backend-as-the-default) deliverable, made in the same change that makes the microVM path work.

### 7. Configuration layering

Implement the loader and precedence rules ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)): built-in defaults → host config → user config → `.clyde/policy.toml`. Repo config may only narrow; a repo value that would widen authority is a **rejection with a diagnostic**, not a silent clamp. Each load records a `config_loads` audit event with the file digest and any rejected keys. `AGENTS.md` is read as prose and passed to agent context; there is no code path that parses it for authority.

**Keys repository config may never set**, because narrowing is not a meaningful operation on them ([D20](../warden/decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration)): `agent.command`, `agent.args`, `agent.env`; runtime root selection for any task type; egress profile host lists (a repo may narrow the set of allowed registries but cannot add a host); anything under `broker.*`.

The loader should cover at minimum `agent.*` (host/user only), `registries.*`, `egress.model_api_hosts` (host/user only), `snapshot.exclusions`, `mission.defaults.*`, `push.remotes`, `push.branch_patterns`, and `limits.*`.

### 8. Storage layer

SQLite schema, forward-only migrations, and the repository traits from the [persistence layout](schema.md#persistence), with an in-memory implementation used by the policy and mission tests.

The audit chain — `prev_hash`, monotonic `seq`, append-only API with no update or delete — is established here, because retrofitting tamper-evidence onto an existing log is meaningless.

### 9. CI

GitHub Actions wired to the flake, so CI and local development cannot diverge. Every job runs inside `nix develop` rather than using setup actions, so a green run means the flake is correct: `nix flake check`; `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo nextest run`; `cargo deny check`; and a lint failing on `unwrap()`/`expect()` outside `#[cfg(test)]` in the `crates/` tree.

### 10. Test fixture inventory

Security properties in later phases are asserted against fixtures, and several phases share them. Each fixture asserts a named property, stated in its own README so a failing test explains itself. Under `tests/fixtures/`:

**Project layouts**, for build-closure computation — `single-crate`; `virtual-workspace` (root with members, no root package); `inherited-deps` (members using `workspace = true`); `nested-path-deps` (member depending on a sibling by path).

**Access baseline behaviour** — `include-str-outside`; `buildrs-reads-repo`; `secret-shaped-files` (a `.env.local` inside a granted subtree, which must not be materialised); `churn` (modules, tests, and files changing between runs, asserting **zero** drift prompts); `revert-freshness` (a file edited, built, then reverted to its earlier content, asserting a rebuild rather than a stale pass — [D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)).

**Hostile execution** — `buildrs-hostile-write`; `buildrs-egress`; `buildrs-credential-hunt`.

**Dependency drift** — `dep-gains-buildscript` (two lockfile states, the second adding a build script); `dep-same-version-tampered`; `missing-dep` (lockfile requiring an absent crate, for `MissingDependencies` classification).

**Broker hardening** — `hostile-git` (`.git/hooks/pre-push` and `.git/config` with `url.*.insteadOf`, both of which must execute nothing).

### 11. `clyde doctor`

A host-prerequisite checker, shipped in Phase 0 because it is what makes bring-up diagnosable rather than mysterious. It also reports the deployment's **posture** ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)): `enforcing`, or `advisory` with the specific reason named — a `cargo`, `rustc`, or `rustup` reachable on the host's `PATH`, no `[agent]` section configured, or both.

This is a first-class check rather than a note about tooling hygiene, because it is the difference between what the builder claims and what [D1](../warden/decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment) promised. Two constraints, both worth tests: posture is **derived**, never configured; and posture changes **reporting only** — it must not appear in `can_run_build`, `strongest_isolation`, or any other admission decision.

`clyde doctor` falls back to probing the host directly when it cannot reach the admin socket, because bring-up is exactly when the daemon is not running ([R9](decisions.md#r9-clyde-doctor-works-without-a-daemon)), and selects each remedy from an observation of the host's enclosure rather than asserting one that cannot work ([R10](decisions.md#r10-a-remedy-that-cannot-work-is-a-defect-not-a-nicety)).

It also reports whether the content store and each registered workspace can be hardlinked between. They usually can; where they cannot, every snapshot silently falls back to copying, and the whole tree is then re-stamped on each run unless the copy path sets mtimes correctly ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)).

## Host prerequisites

Clyde is Linux-first and requires nix on any host that executes tasks. `clyde doctor` checks each item and prints the remediation rather than a bare failure.

| Requirement | Needed for | Notes |
|---|---|---|
| nix with flakes | everything | runtime roots are nix closures |
| KVM (`/dev/kvm` accessible) | build tasks, by default | the microVM backend is the default for every build task ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)) |
| unprivileged user namespaces | the namespace backend | needed for Part 1a, for hosts without KVM, and for the workspace environment |
| cgroup v2 with systemd user delegation | resource limits on the namespace backend | **mandatory** where the namespace backend runs build tasks; no fallback ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)). Not required where the microVM backend runs, since VM sizing bounds the workload by construction |
| a filesystem supporting hardlinks | snapshots | reflink (btrfs/XFS) used when available |

The two isolation requirements are **alternatives rather than a stack**: a host with KVM runs build tasks on the microVM backend and needs no cgroup delegation for them; a host without KVM runs them on the namespace backend and needs both user namespaces and delegation, or build tasks are refused. `clyde doctor` must report whichever case the host is in rather than a generic list.

**The Ubuntu 24.04 user namespace restriction.** Ubuntu 24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1`, which blocks unprivileged user-namespace creation for binaries without a permitting AppArmor profile. Distribution-packaged `bwrap` has such a profile; a nix-store `bwrap` does not, so the namespace backend fails on a default install. Three remediations, in preference order:

1. install an AppArmor profile for the nix-store `bwrap` path granting `userns create` — narrowest, survives reboot, and the profile can be shipped in the repository
2. `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` — broad; it re-enables unprivileged userns for everything on the host
3. use the distribution's `bwrap` instead of the flake's — contradicts the flake-as-source-of-truth rule; a last resort

`clyde doctor` must distinguish "userns unavailable" from "userns blocked by AppArmor", because the fixes are entirely different and the raw kernel error does not say which it is. It must draw the same distinction for cgroups — "no cgroup v2", "present but not delegated", "delegated but missing controllers" — and on a host **without** KVM report missing delegation as a hard failure for build capability rather than a warning. On a host with KVM the same observation is not a build-capability failure. The verdict follows from which backend the host can actually provide.

**Known MVP limitations to report**, so they are discovered before a confusing build failure: git metadata is unavailable to build tasks, so `vergen`-style crates and `build.rs` scripts shelling out to `git` will fail ([D21](decisions.md#d21-git-is-never-available-to-build-tasks)); on a host without KVM, build tasks require cgroup v2 delegation and run on a weaker boundary than the design intends; and in `advisory` posture a driver can execute project code outside Clyde entirely.

## Phase 0 exit criteria

- `nix develop` provides every tool the project uses; nothing depends on a host-global tool
- `nix flake check` and all CI jobs pass on a clean checkout
- every entity in [schema.md](schema.md) exists as a validated Rust type with state transitions as functions
- policy resolution, lease derivation, egress ordering, and budget charging are pure functions with denial-case tests
- the runtime root assertion test passes and would fail if a build toolchain were added to `runtimeRoots.workspace`
- the config loader rejects — not clamps — authority-widening repo config, with a test
- the audit store is append-only and hash-chained, with a test that detects truncation
- `clyde doctor` correctly reports every host prerequisite, distinguishing AppArmor-blocked from unavailable user namespaces, and reports the posture with a named reason when `advisory`
- posture is derived rather than configured, and a test asserts it cannot change an admission decision
- no crate under `crates/` refers to an agent process
- the fixture inventory exists, and each fixture states the property it asserts
- repository config setting `agent.command` is rejected with a diagnostic, with a test
- the MVP task catalog exists with a built-in policy per task type
- `clyde-brokerd` starts, answers a capability query, and holds no credentials

**Not in Phase 0:** no daemon behaviour, no sandbox execution, no snapshots, no MCP server, no agent hosting, no network.

---

# Phase 1: Mission, Lease, and Approval Core

Make bounded authority real: a human creates and approves a mission, Clyde issues a lease, work happens under it, and every side-effecting action is attributable to the principal that requested it.

Phase 1 deliberately contains **no project code execution**. Its purpose is the authority boundary, and that boundary is over *requests* rather than a hosted process — which is what makes it independent of whether anything hosts an agent.

Decisions in force: [D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4), [D16](decisions.md#d16-one-active-mission-per-workspace), [D19](decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc), [D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden), [D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket), [D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded).

### 1. clyded with two sockets

`clyded.sock` — actor API, MCP plus JSON-RPC, requires a valid session token, may be bind-mounted into sandboxes. `clyded-admin.sock` — human API, mode `0600`, `SO_PEERCRED`-checked, never mounted into any sandbox.

Available **only** on the admin socket: mission create, approve, deny, revoke, renew, workspace register, access-baseline confirmation, learn mode, and any read of another actor's data.

Startup fails closed: if either socket path is world-writable, already bound by an unexpected owner, or inside a registered workspace directory, the daemon refuses to start.

Both sockets exist in every deployment. In a human-only deployment the actor socket has no client and is still bound: it is what an external driver connects to, and retrofitting a channel split into a running protocol is the change that goes wrong.

### 2. Mission lifecycle

Create → propose envelope → human approval → active → closeout, with the full state machine from [schema.md](schema.md#mission) and the one-active-mission-per-workspace constraint.

Mission proposal takes the human's objective plus configuration defaults and produces a concrete envelope: edit scope, read scope, allowed tasks, egress profile, budget, expiry, escalation rules. It is shown for approval as the exact envelope that will be issued — no field is decided after approval.

**Mission activation must not depend on an agent.** With no `[agent]` section configured, an approved mission becomes active and its lease is usable immediately from the operator surface. With one configured, activation additionally starts the workspace environment.

Closeout, in one transaction: revoke leases, revoke sessions, tear down any sandboxes, compute and store the closing diff, delete the mission cache, write the summary.

### 3. Lease issuance, derivation, validation, revocation

Wire the Phase 0 pure functions to storage and enforce them on every request. `validate_action` runs before any side effect, and its `PolicyDecision` is recorded whether it allows or denies.

Renewal issues a **replacement** lease and marks the old one `superseded`, rather than mutating expiry in place, so the audit trail shows the extension as an event. Revocation is immediate for new work and fans out to derived leases and sessions in the same transaction.

### 4. Session tokens

256-bit random tokens, stored hashed, delivered to `/run/clyde/session-token` (`0400`) inside a sandbox or issued to an external driver over the admin socket, never in `argv` or environment, never logged, never returned by any query.

Every actor request resolves token → session → lease → mission before any other check ([R1](decisions.md#r1-the-actor-token-binds-a-connection-and-is-re-resolved-on-every-request)). An unknown, expired, or revoked token is rejected identically, with no information about which.

### 5. The acting principal

Every side-effecting record carries the **kind** of principal that acted, not merely a session identifier:

| Principal | Authenticated by | Reaches |
|---|---|---|
| human operator | `SO_PEERCRED` on the admin socket | operator surface |
| external actor | session token on the actor socket | actor surface |
| hosted agent | session token, from a file inside its sandbox | actor surface |

The distinction matters for review rather than admission: an operator-driven task is admitted against the same lease, policy, budget, and baseline as any other. "Who ran this" should be answerable from the record without inference.

### 6. The operator task surface

`task run|status|logs|list` on the admin socket, bound to an approved mission and lease, with the human recorded as the acting principal ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)).

There is no task type reachable only from this surface, and none reachable only from the actor surface. The surfaces differ in who authenticates and how; the admission path is one path.

This is what makes the builder testable and usable on its own.

### 7. Egress proxy

The proxy and forwarder from the [egress model](tasks-and-policy.md#the-egress-model), needed here so one mechanism serves every profile in every later phase: a host-side CONNECT proxy with per-profile host allowlists; `clyde-forward` in the runtime root bridging `127.0.0.1:<port>` to the bound socket; `EgressAttempt` recording for allowed and denied attempts alike; and per-task byte, request, and connection budgets charged to the lease.

Profile `none` is implemented by not binding the socket. There is no runtime flag that disables egress.

The `model-api` profile and its TLS-termination carve-out belong to [the warden](../warden/spec.md#3-the-model-api-egress-path). Phase 1 builds the pass-through proxy; the carve-out arrives with the environment that needs it, and building it earlier would put a plaintext-handling path in the daemon before anything used it.

### 8. Actor API and MCP server

Newline-delimited JSON-RPC directly over `clyded.sock`, sharing one message codec with the admin surface. Message size limits and backpressure must be explicit, since an actor is untrusted input.

The tool set here is the builder's — `mission_status`, `list_capabilities`, `run_task`, `task_status`, `task_logs`, `list_artifacts`, `commit_prepare`, `request_publish`. Tool descriptions are part of the security UX: each must state the policy consequences of the call.

In Phase 1, `run_task` accepts no build task type; requesting `rust.check` returns a structured denial naming the phase-gated capability. That denial path is worth building now, because it is the same code path Phase 3 uses for escalations.

### 9. Approval and confirmation manager

Approval requests carry a `request_digest` over the exact normalised request, an expiry, and the alternatives the policy engine suggested. Decisions are `ApproveOnce`, `ApproveForMission`, or `Deny`, and can only be made on the admin socket by a human. `ApproveOnce` consumption is single-use and transactional: consuming an approval and performing the approved action either both happen or neither does.

When the requesting principal **is** the human operator, the decision is a self-confirmation and must be recorded and rendered as such ([D2 amendment](decisions.md#amendment-confirmation-semantics-when-the-operator-is-the-driver)). The prompt is not removed: its value was never that a second party signed off, but that a human saw what was about to happen before it happened.

### 10. Posture, CLI, and audit

clyded derives and reports the deployment's posture, which appears in `clyde doctor`, on every task run, and in mission review.

CLI operator commands (admin socket): `workspace register`, `mission create|status|approve|deny|revoke|renew|close`, `approvals list|approve|deny`, `task run|status|logs|list`, `audit show`, `doctor`. Actor commands (actor socket, token): `task run|status|logs`, `mission status`. Every command supports `--json` with a stable shape from Phase 1, since the CLI is also the integration-test harness.

`clyde approve` refuses to run if it detects it is inside a sandbox, and says why. That refusal is unaffected by the operator task surface: it protects a different property, and self-approval by a *sandboxed actor* must remain impossible in every posture.

Audit covers the full [minimum event set](schema.md#minimum-event-set), hash-chained, with `audit show` rendering a mission timeline. Every event carries the acting principal and the posture in force.

### Diff-based edit auditing

Clyde records what changed rather than each write. Phase 1 computes and stores a workspace diff on demand (`clyde mission status --diff`), at mission closeout, and — from Part 1a — at every snapshot boundary. The diff is computed by the daemon against the live tree using sanitised git invocations, the same hardening Phase 4 needs for push.

**Under the builder alone this is the *only* record of editing, and it is detection rather than prevention:** nothing stops a driver editing outside the lease's scope, and the closing diff is where that becomes visible. Mount-topology enforcement of edit scope belongs to [the warden](../warden/design.md#editing-authority-is-mount-topology), and this asymmetry belongs in user-facing documentation rather than only here.

## Phase 1 security properties

- No side-effecting action succeeds without an active lease and, on the actor surface, a valid session token.
- No actor-facing operation can approve or confirm anything: the admin socket is unreachable from any sandbox, and `clyde approve` refuses inside one.
- A task requested on either surface is admitted against the same lease, policy, and budget, verified by a test that runs the same request both ways and compares the recorded decision.
- The acting principal on every record is the principal that actually acted, and cannot be set by the requester.
- Revoking a mission stops all further work under it immediately, including for derived leases.
- Posture is derived, and no configuration value changes what is reported. A test asserts posture cannot alter an admission decision.
- Profile `none` is the absence of a bound socket rather than a flag, verified over every generated sandbox specification.
- A daemon with no `[agent]` section starts, serves both surfaces, activates a mission, and runs a task.

## Phase 1 exit criteria

- a human can create, approve, inspect, renew, and revoke a mission end to end from the CLI
- a human can run a task under an approved mission with no agent configured anywhere, and the run is recorded with the human as the acting principal
- an external MCP-capable driver holding a session token can drive `mission_status`, `list_capabilities`, and `run_task` over the actor socket with no Clyde-specific modification
- out-of-scope task requests are denied with a structured, actionable reason on both surfaces
- lease expiry and revocation are enforced against in-flight work
- the audit chain reconstructs a full mission timeline including principal and posture, and truncating it is detectable
- `--json` output for every command is covered by CLI integration tests
- `clyde doctor` reports the posture, names the specific bypass when `advisory`, and cannot be made to report `enforcing` by configuration

**Not in Phase 1:** no snapshots, no project code execution, no dependency resolution, no broker operations, no TUI, no agent hosting, no workspace-environment sandbox, no `model-api` egress.

---

# Part 1a: Snapshots and the Namespace Backend

Make the edit/check/test loop both practical and isolated: untrusted project code executes against immutable snapshots, with no network and no credentials, fast enough to iterate.

Decisions in force: [D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only), [D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift), [D21](decisions.md#d21-git-is-never-available-to-build-tasks), [D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution), [D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket).

**Why the namespace backend first:** every part of this list except the sandbox itself is backend-independent, and debugging closure computation, baselines, inventory diffs, and classification through a VM boundary at the same time is the harder path. Bubblewrap is explicitly the scaffold.

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

`SandboxSpec` is backend-independent: runtime root closure, mount table with modes, egress profile, resource limits, environment, argv, and scratch policy. Anything a backend cannot honour is a `preflight` failure, never a silent relaxation. Backend selection is driven by the policy's `min_isolation`; there is no manual override in the MVP.

### 2. Bubblewrap backend

Namespace isolation (user, pid, ipc, uts, cgroup, net), read-only runtime root and snapshot binds, writable mission cache bind, tmpfs scratch, minimal `/dev`, `--clearenv`, `--new-session`, `--die-with-parent`, and a seccomp filter ([R5](decisions.md#r5-the-seccomp-filter-is-a-deny-list-and-is-passed-on-the-childs-stdin)).

Resource limits come from cgroup v2 via systemd user delegation (`systemd-run --user --scope` with `MemoryMax`, `CPUQuota`, `TasksMax`), plus rlimits and a daemon-enforced wall-clock timeout, applied by wrapping the command rather than in a `pre_exec` hook ([R4](decisions.md#r4-resource-limits-are-applied-by-wrapping-the-command)).

Cgroup limits are **mandatory** here: without delegation, build and test tasks are refused. Part 1b narrows that refusal to hosts without KVM, where this backend is the only option and it therefore still applies.

A test-only backend with no isolation lives behind a cargo feature no shipped binary enables ([R6](decisions.md#r6-a-test-only-backend-exists-and-never-ships)).

### 3. Snapshot manager

Content-addressed store with BLAKE3 identity over the manifest; materialisation by hardlink (default), reflink where the filesystem supports it, copy as fallback; snapshots **always** bound read-only; exclusions applied before hashing and before grants. The canonical exclusion list and the build-closure computation are in [tasks-and-policy.md](tasks-and-policy.md#access-baselines).

Materialisation also carries an **mtime contract** ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)): unchanged paths keep the mtime they had last run, changed paths get a fresh one, and a path reverted to content the store already holds counts as changed. Cargo has no content-hash freshness mode on stable, so this is what makes an incremental build correct rather than merely fast — and the copy fallback must set mtime explicitly, since `std::fs::copy` carries permissions and not timestamps.

Getting closure computation wrong is the most likely cause of "Clyde can't build my project", so it deserves fixture-based tests across the four project layouts.

### 4. Access baseline

Pin, enforce, detect drift, per [D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift); the full model is in [tasks-and-policy.md](tasks-and-policy.md#access-baselines). Part 1a delivers: the static proposal path; human confirmation on the admin channel; enforcement by materialisation; privileged learn mode with host-side `inotify` observation; drift classification and escalation rendering; and inventory computation and pinning. The confirmation UX for *inventory* drift lands with the Phase 3 escalation flow.

Admin-channel CLI: `clyde access show|propose|learn|review|confirm|reset`.

### 5. Per-mission cache

At mission activation, create `missions/<id>/`: `cargo-home/`, seeded by hardlink from the read-only dependency bundle store and writable because cargo writes lock files into `CARGO_HOME` even offline; and `cargo-target/`, the `CARGO_TARGET_DIR` shared across task runs within the mission. Both are destroyed at closeout, and size counts against `max_cache_bytes`.

### 6. Dependency bundle import

Part 1a has no fetch task, so offline builds need a bundle to exist. `clyde deps import` takes a host-produced cargo cache or vendor directory, content-addresses it into the read-only bundle store, and records the `Cargo.lock` digest it satisfies. A human, admin-channel operation, and also what makes the code-execution inventory computable before Phase 3. Phase 3 replaces it as the normal path but it stays useful for air-gapped setups.

### 7. `rust.check`, `rust.test.unit`, and failure classification

Offline by construction: egress `none`, `CARGO_NET_OFFLINE=true`, `--offline --frozen`. A missing dependency must fail rather than trigger a fetch — that failure is the entry point to Phase 3.

Task outcomes carry `TaskFailureClass` ([the classes](tasks-and-policy.md#failure-classification)), derived from exit status plus structured cargo diagnostics rather than by scraping human-readable output. Covered by fixture tests, because Phase 3's escalation flow keys off it.

### 8. Task execution pipeline and surfaces

Admission (lease, policy, budget) → snapshot → sandbox spec → start → stream logs → collect outputs → classify → store artifacts → record audit events. Logs stream to the requester while running, are persisted as artifacts, and are bounded in size with truncation recorded rather than silent. Budget is charged at admission, before execution, so a daemon crash cannot lose the charge.

`clyde task run|status|logs|list` on both the operator and actor surfaces, and the equivalent MCP tools, returning structured outcomes with the policy digest actually applied — so "which policy ran" is answerable from the result rather than inferred. Each result carries the acting principal and the posture in force.

The operator surface is what makes this sub-part demonstrable without an agent, and the full inner loop should be exercised that way before 1b begins.

---

# Part 1b: The MicroVM Backend as the Default

Every build task runs here by default ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)). The namespace backend remains for hosts without KVM and for the workspace environment.

The device model constrains this sub-part more than anything else in the project, so the constraints come before the deliverables.

**Status.** Sections 1–4 and 7 are built, and 8 is built as an operator's per-run choice rather than as the floor: `nix build .#guestVm` produces the kernel and the erofs roots, `clyde-guest-api` carries the job, `clyde-init` runs it, images are built with `mke2fs -d`, and all guest output leaves over vsock. An operator raises a task into a guest with `clyde task run … --isolation microvm`; [microvm-bring-up.md](microvm-bring-up.md) is the walkthrough. Still open: the egress bridge in §6, guest-side learn mode in §5, raising the policy floor in §8, and every measurement in §9. **Nothing has booted a guest yet**, so none of this has met an exit criterion — the host side is tested, Firecracker and the init under a real kernel are not.

### 0. What Firecracker provides, and what it does not

Firecracker's device model is virtio-block, virtio-net, virtio-vsock, virtio-balloon, virtio-rng, an 8250 serial UART, and an i8042 controller for reset. That is the whole list, and it is deliberate.

- **No filesystem passthrough of any kind.** No virtio-fs, no 9p, no shared directory. The only path for host bytes into the guest is a block device.
- **No virtio-console.** The 8250 UART is the only console, it is slow, and bulk output through it can stall the guest. It is not a log transport.
- **No hotplug.** Devices are discovered at boot from the kernel command line on x86_64 and the FDT on aarch64. `mem_size_mib` and `vcpu_count` are fixed for the VM's life; the balloon device can only return memory, never raise the ceiling.
- **What can change at runtime:** a drive's `path_on_host`, by `PATCH /drives/{id}` before the guest mounts it. This is the mechanism any future warm-start path depends on, and its exact semantics should be verified against the deployed version rather than assumed.
- **A cap on total virtio devices**, bounded by the legacy IRQ range available for MMIO. The backend attaches one drive per mount with a host source; the ceiling must be confirmed and the mount table budgeted against it rather than discovered by a boot failure.
- **Drive names are positional.** Drives appear as `/dev/vda`, `/dev/vdb`, … in attachment order, and `drive_id` is host-side metadata the guest cannot read.

### 1. Guest images

Built from the same nix closures as the namespace backend's runtime roots: an uncompressed guest kernel, and one root image per runtime root kind attached read-only as the root device.

Read-only roots are **uncompressed erofs** rather than ext4 or squashfs — journal-free, immutable by construction, and built for the random reads a nix closure gets at every `exec` ([R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)). The image must place the closure at the identical `/nix/store/...` paths, since store paths are baked into every binary in it. Turning a closure into a bootable image belongs in the flake, so image identity follows closure identity.

### 2. Guest init and the job contract

A small guest `init` that mounts its drives, learns what to run, runs it, and reports what happened. The contract is a **versioned type shared between host and guest**, not an ad hoc encoding, carrying:

- argv, environment, and working directory
- the drive-label → mount-point mapping, because guest device names are positional and `drive_id` is invisible in the guest. Set a filesystem label or UUID at `mkfs` time and mount by label; anything else silently breaks the first time the mount table changes shape
- an exit status distinguishable from the VM's own exit code, so `SandboxFailure` and `ProjectCodeError` stay distinguishable

Two constraints: the task process should drop to a non-root uid in the guest — guest root is defensible because the VM is the boundary, but a guest kernel exploit then needs two steps rather than one. And the job must arrive **over a channel** rather than baked into boot arguments, because a VM booted before its job exists cannot carry that job on its kernel command line, and that is precisely the option a future warm pool needs left open.

### 3. The file surface

Block devices only. The design works because the expensive surface and the per-run surface are different things:

| Surface | Lifetime | Representation |
|---|---|---|
| runtime root | immutable | read-only uncompressed erofs image, root device |
| mission cache (`cargo-target`, `cargo-home`) | per mission | **one** ext4 image created at mission activation, attached read-write to every run |
| source snapshot | per run | ext4 image built from the materialised tree, read-only |
| dependency bundle | content-addressed | read-only image |

The mission cache is never rebuilt per run, which is what keeps the inner loop viable; creating it at the mission's `max_cache_bytes` also makes the cache budget enforced by the filesystem's size rather than by accounting. The per-run source image is built with `mke2fs -d` from the materialised snapshot tree, which is **unprivileged** — no loop mount, no root — and costs time proportional to source size rather than to `target/`.

`cache_type: Unsafe` is correct for the mission cache: it is disposable by design, so paying host `fsync` for it buys nothing. `io_engine: Async` (io_uring) is worth using where the host kernel supports it.

If measurement shows the per-run image build is too slow, the refinement is a read-only base image plus a small writable delta with an overlay mounted in the guest, dropping per-run cost to the size of the diff. Do not build it speculatively — deletions need overlayfs whiteouts, which are fiddly to synthesise unprivileged.

One rule with no exceptions: **a drive attached read-write to a running guest is never touched from the host.** ext4 is not a cluster filesystem.

### 4. Artifact and log output

Outputs written by the guest live inside an image the host cannot safely read while the guest runs. **All guest output leaves over vsock** ([R11](decisions.md#r11-all-guest-output-leaves-over-vsock)) — logs and cargo's JSON diagnostics streamed while the task runs, the learn-mode read set and the task artifacts on the same multiplex. Nothing is extracted from a guest-written image: there is no `debugfs` or `fuse2fs` step, and the mission cache image is never read by the host at all.

The serial console is not an alternative for the log half — it cannot carry `cargo --message-format=json` volume without stalling the guest — and once a vsock protocol exists for that, a second output path with its own failure modes buys nothing. It also means a guest that dies mid-run has already delivered everything it emitted, rather than leaving it in an image whose last writes never landed.

Two obligations this puts on the host side:

- the host reader enforces `max_artifact_bytes` as it reads and truncates the way the namespace backend already does, rather than trusting a length the guest declares. A hostile guest can flood the channel, and this is the only thing bounding it
- artifacts stream *out*; compilation output is not an artifact. `target/` stays in the mission cache image across runs ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)), so nothing large crosses the channel

### 5. Learn-mode observation inside the guest

Host-side `inotify` cannot see guest reads; `fanotify` inside the guest can, over the mounted snapshot, reporting the observed read set back over the job channel. This is strictly better than the earlier arrangement, in which learn mode — the widest-scope run of exactly the code being constrained — had to happen on the weaker backend. The privilege constraints are unchanged, and the observation must report what it could not watch.

### 6. Egress over the vsock multiplex

One vsock device per VM, multiplexed by port: job control, log stream, and the egress bridge ([D7 amendment](decisions.md#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port)). The guest has **no network device** in any configuration and `VmConfig` cannot express one; the host binds **no listener** on the egress port unless the profile permits egress; and a VM booted for one egress profile never serves a task under another. `clyde-forward` gains its guest half here: it currently speaks to a Unix socket and needs to speak vsock.

### 7. Resource limits from VM configuration

Memory and vCPU allocation bound the workload by construction, so cgroup delegation is not required where this backend runs. On a host without KVM the namespace backend is the only option and [D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)'s refusal applies unchanged. The block device `rate_limiter` is an available and underused lever: it bounds a hostile build's I/O the same way VM sizing bounds its memory.

### 8. Policy-driven selection, and raising the floor

`rust.check` and `rust.test.unit` carry `min_isolation: NamespaceSandbox` through Part 1a. **Raising both to `MicroVm` is a deliverable of this sub-part**, made in the same change that makes the microVM path work. Deferring it leaves a weak floor in a closed catalog after the stronger backend exists, which is a floor some deployment keeps running against.

`min_isolation` is a floor and the manager never silently downgrades. A downgrade requires an explicit configuration decision, is unavailable for T3 tasks, and must appear in the task result and the audit record rather than only in a log line. On a host without KVM that downgrade is how build tasks still run, under D22's cgroup requirement — a deliberate, visible, recorded state rather than a silent fallback.

### 9. Parity and measured latency

Parity for `rust.check`, `rust.test.unit`, and `rust.resolve-deps`. Latency measured and reported for cold and warm mission cache — an acceptance criterion, not a curiosity, because every inner-loop iteration now pays VM cost.

**The target.** Measured end to end, from task admission to a result the operator can read, on a warm mission cache after a one-file edit: the microVM backend adds **no more than 2s at p50 and 4s at p95** over the same task on the namespace backend, on the same host and the same tree. That is the whole per-run overhead — snapshot materialisation, source image build, boot, job handshake, teardown, and artifact drain — not the boot figure alone.

The target exists to be acted on rather than recorded, so each way of missing it names its own remedy:

- **source image build dominates** → the read-only base plus writable delta in [§3](#3-the-file-surface), which is held in reserve for exactly this
- **boot and teardown dominate** → the warm pool below, which the job contract already permits
- **neither dominates and the warm loop is still slow on a real repository** → 1b does not exit. An inner loop that is not interactive is a product failure, not a number to write down

Cold cost at a mission boundary is **reported without a target**, because it is dominated by dependency compilation rather than by the backend, and it is the figure [D28](decisions.md#d28-dependency-build-output-stays-in-the-per-mission-cache-for-the-mvp) is waiting on.

A warm pool via snapshot/restore is **not** in 1b. If the numbers demand one, the job contract already permits it. Persistent VMs reused across task runs are refused outright: a VM that has executed a hostile build script and then serves the next run is the in-memory form of the cache-persistence vector D3 exists to close.

## Parts 1a/1b security properties

On both backends unless stated otherwise:

- project code executes only against read-only snapshots, never the live workspace
- no egress whatsoever from `rust.check` / `rust.test.unit`, verified by a test that attempts a connection from inside the sandbox and asserts both the failure and the `EgressBlocked` record
- no credential material, host home, `~/.ssh`, `~/.gnupg`, browser profile, or container socket is reachable from a build sandbox, verified by a test that looks for each
- a hostile `build.rs` fixture cannot write outside the mission cache and scratch, and cannot read the workspace outside the snapshot
- cache state does not cross missions or projects, verified by a fixture that writes a marker in one mission and asserts its absence in the next
- snapshot content is immutable during a run, and the content store is not modified through a hardlink
- a task never reports a pass for a tree it did not build: reverting a file to content the store already holds forces a rebuild rather than being seen as fresh ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)). Task evidence is what a push approval rests on, so a stale pass is an integrity failure and not only a nuisance
- a build script reading a repo path outside grants ∪ pins fails, and the failure is rendered as a named escalation rather than a raw cargo error
- creating, renaming, and deleting files inside a granted subtree produces **no** drift and **no** prompt across a full edit/check/test loop, verified by a fixture that adds modules and tests between runs
- an absolute exclusion inside a granted subtree is not materialised, verified with a `.env.local` fixture
- a `build.rs` reading `.git` fails with `GitMetadataUnavailable`, and no configuration or pin can admit `.git`
- a fixture adding a `build.rs` to a previously script-free dependency is caught **before execution** by the inventory check
- a fixture changing a build script's content at the same crate version is caught by the content hash
- learn mode cannot be initiated by an actor, and its proposals have no effect until confirmed
- a task requested on the operator surface and the same task requested on the actor surface produce the same policy decision

**Namespace backend only:** with cgroup delegation unavailable, a T2 task is refused.

**MicroVM backend only:** a generated `VmConfig` contains no guest network device under any egress profile, verified as a value; no host listener exists on the egress vsock port for a `none`-profile VM; every drive whose spec says read-only is attached read-only, verified as a value; a task's exit status is distinguishable from a VM-level failure, verified by a fixture that kills the guest.

## Part 1a exit criteria

- `rust.check` and `rust.test.unit` run from immutable, closure-aware snapshots on the namespace backend
- a **human operator** drives a full edit → check → test loop with no agent configured anywhere, and the loop needs no repeated approval
- failure classification distinguishes project errors from missing dependencies, with fixture tests
- the per-mission cache makes the second and later iterations materially faster than the first, with a measured figure recorded
- reverting an edited file causes a rebuild rather than a stale pass, and forcing the copy materialisation strategy produces the same rebuild decisions as hardlinking ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime))
- an access baseline can be proposed from the static closure, confirmed, and enforced; a task with no baseline is refused rather than run wide
- first-party editing generates no baseline prompts: a loop adding source files, tests, and modules inside the approved scope runs uninterrupted
- learn mode records a baseline for a fixture whose `build.rs` reads a repo file static analysis cannot see
- the code-execution inventory is pinned and drift is detected pre-execution for all three drift cases
- every security property above holds under test on the namespace backend
- mission closeout deletes the cache and stores the closing diff

## Part 1b exit criteria

- the microVM backend passes the same suite as 1a, including the egress and credential-absence tests
- guest images are built by the flake from the runtime-root closures, and image identity follows closure identity: the same closure produces a byte-identical erofs root ([R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs))
- the job contract conveys argv, environment, working directory, and drive labels, and returns an exit status distinguishable from a VM failure
- logs, diagnostics, and artifacts all leave the guest over vsock, no pipeline path reads a guest-written image ([R11](decisions.md#r11-all-guest-output-leaves-over-vsock)), and a guest killed mid-run still yields everything it had emitted
- learn mode observes reads from inside the guest and reports what it could not watch
- `rust.check` and `rust.test.unit` select the microVM backend by policy, with no silent downgrade path
- inner-loop latency is measured and reported for cold and warm cache, and judged against the [stated target](#9-parity-and-measured-latency) — p50 within 2s and p95 within 4s of the namespace backend on a warm cache — rather than merely recorded
- cold-build cost **at a mission boundary** is measured and reported alongside inner-loop latency, since that is the figure a shared dependency cache would exist to reduce ([D28](decisions.md#d28-dependency-build-output-stays-in-the-per-mission-cache-for-the-mvp))
- the namespace backend remains available for hosts without KVM and for the workspace environment, and a host without KVM still runs build tasks under D22's cgroup requirement
- `clyde doctor` reports KVM as a hard requirement for the default configuration, and distinguishes "no KVM, using the namespace backend under D22" from "no KVM and no cgroup delegation, so build tasks are refused"

**Not in this part:** no dependency fetch, no broker operations, no frontend or browser tasks, no TUI, no agent hosting, no warm microVM pool.

---

# Phase 3: Separate Dependency Resolution

Enforce the fetch-versus-compile split: compilation and testing never reach the network, dependency retrieval is a distinct confirmation-gated task with a narrow allowlisted egress profile, and its output is an explicit artifact.

This is the phase that most directly addresses the Rust supply-chain threat model, because it removes any legitimate reason for a compile step to have network access.

**Prerequisite:** [Part 1b](#part-1b-the-microvm-backend-as-the-default). `rust.resolve-deps` has `min_isolation: MicroVm`, and under D24 so does every other build task — but this is the one that both executes untrusted code paths *and* has network reachability, so it is the task the microVM boundary was made non-negotiable for first. It is also the first task to use the egress port of the vsock multiplex, and the first to prove that a `none`-profile VM and a `rust-registry` VM differ in whether a host listener exists rather than in a configuration flag.

### Deliverables

1. **`rust.resolve-deps`** and the **dependency bundle artifact** — the task semantics, the input narrowing, the bundle contents, and the `--locked` requirement are in [tasks-and-policy.md](tasks-and-policy.md#rustresolve-deps).
2. **Lockfile-aware fetch policy.** Before fetching, compare the requested lockfile against the previously satisfied one and classify the change: unchanged, additions only, version changes, source changes, or a new git dependency. The classification appears in the approval prompt, because "fetch dependencies" and "fetch dependencies, including four new crates from a source you have not used before" deserve different scrutiny, and only Clyde is in a position to tell them apart.
3. **Code-execution inventory diff.** The higher-signal companion to the lockfile diff: after a fetch and *before any build task runs against the new bundle*, recompute the inventory and diff it against the pinned baseline, reporting new, changed-version, and changed-content-at-same-version cases distinctly ([the prompt shape](tasks-and-policy.md#drift)). Configuration may pre-approve the low-risk classes for a mission; a repository may narrow that but never widen it. Git dependencies and unknown registries are never pre-approvable.
4. **Escalation flow**, driven by the `MissingDependencies` classification. Two entry paths — actor-driven via `request_escalation`, and operator-driven where the failure names the next step — and one decision path. There is no `request_escalation` in the second because there is nobody to ask. What must **not** differ is the prompt or the record: the same host allowlist, lockfile classification, inventory diff, destination-only caveat, and audit entry, marked as a self-confirmation rather than an approval. Whichever path is taken, the prompt must explain *why* the previous profile failed.
5. **Rerun path.** After a successful fetch, the mission's `CARGO_HOME` is re-seeded by hardlink from the new bundle and the offline build reruns with egress `none`, with no further approval — because the boundary crossing was the fetch, not the compile — **unless** the inventory diff is non-empty, in which case the human confirms the new inventory before any build task runs against the new bundle. That is the point of the whole phase for this threat model: fetching a hostile crate is harmless until something executes it, and the confirmation sits in between.
6. **Egress accounting.** Byte and connection budgets enforced per task and charged to the lease. The fetch manifest records every destination and every refusal, and a refusal during a fetch is surfaced prominently in the task result and mission review, not buried in a log.

## Phase 3 security properties

- `rust.check` and `rust.test.unit` remain at egress `none` and cannot fetch, verified by a fixture whose lockfile requires an absent crate: the build must fail with `MissingDependencies`, not succeed
- the fetch sandbox can reach allowlisted registry hosts and nothing else, verified by attempting a non-allowlisted host and asserting refusal plus an `EgressAttempt` record
- the fetch sandbox holds no credentials, and a private-source requirement is denied rather than silently attempted
- the fetch task's input snapshot contains no application source, verified by inspecting the manifest
- the dependency bundle is read-only wherever mounted, and a task cannot mutate it
- the decision is bound to the exact request: a lockfile change after approval invalidates the approval rather than being fetched under it
- a fetch introducing a new build-script or proc-macro crate does not permit a build against the new bundle until the inventory change is confirmed
- a same-version content change in a code-executing crate is detected and reported distinctly from a version change

## Phase 3 exit criteria

- compile and test never fetch, and fail informatively when dependencies are absent
- the full escalation cycle — offline failure, escalation, narrow approval, fetch, offline rerun, success — works end to end and is legible in mission review
- the approval prompt shows the exact host allowlist, the lockfile change classification, and the destination-only caveat
- egress attempts, allowed and refused, are recorded for every fetch and visible in the task result
- pre-approval of low-risk lockfile classes works per configuration and cannot be widened by repository configuration
- the approval prompt reports the code-execution inventory diff alongside the lockfile diff
- no build task runs against a bundle whose inventory diff is unconfirmed
- git dependencies and unknown registries are refused with an actionable explanation

**Not in Phase 3:** no private registry or private git credential support, no `node.resolve-deps`, no vendoring workflow, no SBOM generation beyond the bundle contents.

---

# Phase 4: Credential Broker and Brokered Git Push

Separate code execution from authority: a requester can prepare a commit and request a push, and the push happens through the broker with human approval, without any credential ever being reachable from a build sandbox — or, where the requester is not the credential's owner, from the requester either.

**Phase 4 completes the builder.** Which of the broker's three functions is load-bearing depends on who is driving; see the [D8 amendment](decisions.md#amendment-the-brokers-value-is-posture-dependent). The documentation and the approval prompt should reflect which is in force rather than implying the third always is.

Decisions in force: [D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel), [D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only), [D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4), [D15](decisions.md#d15-three-binaries-from-phase-0), [D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden), [D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded).

### Deliverables

1. **Broker gateway in clyded.** A single adapter through which all privileged external operations pass. It translates a typed authority request into a broker call, attaches the approval reference, and records the operation. No other part of clyded may call the broker.
2. **`clyde-brokerd` gains real operations.** Capability-oriented, never secret-oriented. It listens on `brokerd.sock`, mode `0600`, never mounted into any sandbox; accepts requests only from clyded, verified by `SO_PEERCRED`; **validates every request independently** — it does not trust clyded's word that an approval exists, it verifies the approval record itself; holds credentials in process only, never writes or logs them, and its types do not implement `Serialize` or a revealing `Debug`; and refuses any operation whose approval is missing, expired, already consumed, or whose digest does not match exactly. Its own replay protection is the operation state ([R8](decisions.md#r8-replay-protection-is-the-brokered-operations-state)).
3. **`git.commit.prepare`** and **`git.push`**, including the sanitised temporary repository and the full validation order — see [tasks-and-policy.md](tasks-and-policy.md#gitcommitprepare). Under the warden, `.git` is read-only inside the workspace environment so a hosted agent cannot create commits directly or plant hooks; under the builder alone that mount does not exist and `.git` is whatever the driver made it. This changes nothing about the design, because the hardening already treats `.git/config` and `.git/hooks` as attacker-controlled content in every posture. **The read-only mount is defence in depth; the sanitised temporary repository is the control.**
4. **Approval UX for push.** The prompt shows remote, resolved URL, branch, commit id and subject, diff statistics, requesting principal, mission, posture, and the passing task evidence for that tree. Approval is per-push by default; `ApproveForMission` is available but scoped to a branch pattern, never to "any push". Where the requesting principal is the human operator, the decision is rendered and recorded as a self-confirmation — the evidence shown is unchanged, and that is the part doing the work.
5. **TUI.** The ratatui client, now that the mission, task, and approval surfaces are stable: mission and lease status with budget, live task list and logs, pending approvals with full policy detail, and a mission timeline. It is a client of the same admin and actor APIs — no privileged path exists only in the TUI.
6. **Mission review.** The end-to-end surface: objective, files changed, tasks run with pass/fail and policy digests, escalations and outcomes, approvals granted, egress attempts, brokered operations, budget consumed, posture, and the closing diff. This is what makes autonomous work trustworthy after the fact, and it is the last MVP deliverable for a reason — everything before it feeds it.

## Phase 4 security properties

- no SSH agent socket, key file, or token is present in any sandbox mount table, verified by a test that inspects every spec the system can generate
- no actor can invoke the broker: the socket is absent from every sandbox and no actor-facing operation reaches the broker except `request_publish`, which only creates an approval request
- a push cannot occur without a matching, unexpired, unconsumed approval, verified including the tamper cases — altered commit, altered refspec, altered remote
- a repository containing a hostile `pre-push` hook and a hostile `.git/config` cannot execute anything during a brokered push, verified by a fixture that would create a marker file if either ran
- protected-branch and non-allowlisted-remote pushes are refused
- mission revocation freezes in-flight brokered operations rather than cancelling them silently

## Phase 4 exit criteria

- a human operator prepares a commit, requests a push, and the push succeeds only after confirmation on the admin socket — with no agent configured anywhere
- an external actor holding a session token does the same, and cannot itself confirm it
- no credential is reachable from any sandbox, demonstrated by the mount-table test
- the push prompt states which of the broker's three functions is in force for the posture it ran under
- the hostile-hook and hostile-config fixtures execute nothing
- push prompts show remote, branch, commit, actor, and task evidence
- the broker independently verifies approvals rather than trusting the caller
- the TUI presents mission, task, approval, and audit surfaces over the same APIs as the CLI
- mission review shows the complete chain from objective to pushed commit

**Not in Phase 4:** no signing, no publishing, no private registry credentials, no scoped token minting, no multi-user or shared-runner support.

---

## Sequencing rationale

**Why the build pipeline before the warden?** Because it is where the security value is, and because it was previously unreachable without the warden: tasks were actor-only, so exercising snapshots, baselines, inventory diffs, and classification required first configuring and hosting an agent — the most important subsystem depending on the least essential one.

**Why mission/lease first?** Without it, autonomy falls back to implicit session authority and later hardening becomes messy. The mission model is also driver-agnostic, so it is not work the warden repeats.

**Why snapshots and check/test next?** Because the inner loop must be usable or the system will be rejected by developers.

**Why the namespace backend before the microVM?** Because everything in Part 1a except the sandbox is backend-independent, and it is cheaper to bring the microVM up against a pipeline that already works than to debug both through a VM boundary.

**Why the microVM before dependency fetch?** Because fetch is the first task with any network reachability, and the egress plumbing should be built once against the intended backend.

**Why dependency fetch before publish features?** Because separating fetch from compile is the core security property for Rust and full-stack ecosystems.

**Why git push broker before release publishing?** Because push is a frequent workflow and a common privilege exposure path.

**Why the warden last?** Because it is a product rather than a prerequisite. Nothing in the builder needs it, `advisory` posture is honest about its absence, and a developer who already runs a coding agent gets most of the value by pointing that agent at the builder's socket.

## Engineering workstreams

- **A. Control plane and schemas** — mission manager, lease manager, task request model, audit schema, session tokens, acting principal, posture
- **B. Workspace and UX** — CLI, mission and task views, confirmation UX, mission review, TUI
- **C. Execution** — snapshot manager, sandbox backends, guest images and the job contract, cache management, log streaming, egress proxy
- **D. Broker** — broker gateway, git push broker, sanitised git invocation helper, approval linkage
- **E. Policy and testing** — task policy resolver, lease validation, mission defaults, threat-model tests
- **F. Warden** — workspace-environment sandbox, agent hosting, `model-api` egress and credential injection, sub-agent derivation

## Testing strategy

**Unit** — mission state transitions; lease derivation including every denial case; budget exhaustion; approval decision logic and digest binding; policy resolution; egress profile ordering including incomparable pairs; posture classification.

**Integration** — snapshot correctness and build-closure computation across project layouts; snapshot freshness semantics across edit and revert, on both materialisation strategies; no-network enforcement; dependency fetch escalation and offline rerun; brokered push; revocation while tasks are pending or running; mission closeout and cache teardown; the operator task path end to end with no agent configured.

**Security assertions** — assertions about the system's claims, each failing loudly if the claim stops holding:

- no `~/.ssh`, `~/.gnupg`, cloud config, browser profile, host home, or container socket appears in any generated sandbox mount table
- a connection attempt from inside a `none`-profile sandbox fails and is recorded as `EgressBlocked`
- a non-allowlisted destination from a `rust-registry` sandbox is refused and recorded
- a hostile `build.rs` fixture cannot write outside the mission cache and scratch, or read outside the snapshot
- a `build.rs` fixture reading a repo path outside the confirmed baseline fails, and the failure is reported as named drift
- a fixture adding a `build.rs` to a previously script-free dependency is caught before execution
- a fixture changing build-script content at the same crate version is caught by source hash
- learn mode cannot be initiated by an actor
- cache state does not cross missions
- forbidden path access and unauthorized task requests are denied with structured reasons
- a hostile `pre-push` hook and hostile `.git/config` execute nothing during a brokered push
- an approval cannot be replayed, and a tampered request does not match its approval
- posture is derived, never configured, and cannot change an admission decision
- a microVM configuration contains no guest network device under any egress profile, and no host egress listener exists for a `none`-profile VM

## Risks

### Risk 1: inner-loop latency is too high
A Part 1b acceptance criterion rather than a footnote, because every build iteration pays microVM cost. **Mitigation:** the per-mission warm cache as a long-lived block image, so the expensive surface is never rebuilt per run; hardlink and reflink materialisation for the per-run source surface; measured latency as an exit criterion; a warm pool via snapshot/restore held in reserve rather than built speculatively.

This risk is about latency *within* a mission. The cost *across* missions — one cold build per mission under [D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only) — is plausibly the larger figure and is tracked separately as [D28](decisions.md#d28-dependency-build-output-stays-in-the-per-mission-cache-for-the-mvp), which defers a shared dependency cache until Part 1b has measured both.

### Risk 2: UX becomes too approval-heavy
**Mitigation:** mission-level pre-approval for safe inner-loop tasks; approvals only at boundary crossings; narrow prompts stating what changed and why the safer profile failed.

### Risk 3: escape hatch becomes the default
**Mitigation:** `shell.untrusted` is deliberately absent from the MVP catalog; under the warden the workspace environment simply lacks the build toolchain, so ad hoc project execution is not available to route around typed tasks. Under the builder alone the escape hatch is the host itself, which is what posture reports.

### Risk 4: complexity overwhelms implementation
**Mitigation:** ship one strong Rust workflow first; keep interface contracts separate from backend sophistication; defer multi-agent, publishing, and enterprise features. The product split is itself a mitigation: it lets the security-critical half ship without the half that is a product feature.

### Risk 5: host prerequisites block adoption
**Mitigation:** `clyde doctor` ships in Phase 0 with per-item remediation, including the Ubuntu 24.04 user namespace restriction and, from Part 1b, KVM as a hard requirement for build capability. Host requirements are documented rather than discovered during bring-up.

### Risk 6: baseline churn makes drift prompts routine
If baselines churn on ordinary development, drift prompts become noise, the human starts confirming reflexively, and the control loses its value for the thing it exists to catch — new dependency code.

**Mitigation is structural rather than procedural:** first-party project code is granted **by subtree**, so creating, renaming, or deleting files inside the mission's approved scope is not drift and produces no prompt at all. Only two things prompt: reaching outside the approved scope, and dependency change. Path drift and dependency drift are also presented distinctly, since the first is low-signal and the second is the point.

Prompt frequency should still be measured against a real repository during Part 1a rather than after. **The target is that a full feature's worth of editing produces zero baseline prompts.**

### Risk 7: the workspace environment's model-API egress is an exfiltration path
A **warden** risk. **Mitigation:** it is the only egress from that environment, it is allowlisted and logged, and untrusted dependency code never runs there. The residual risk — a misbehaving or prompt-injected agent — is stated plainly rather than papered over, and is not claimed to be solved in the MVP.

The builder alone does not carry this risk, because Clyde is not providing the agent's channel to anything.

### Risk 8: the builder gets described as though the warden existed
The risk the split creates. Shipping the strong half and talking about it as if the bypass were closed would be worse than not splitting at all, because the claim would be believed.

**Mitigation is [D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)**, and it has to be loud rather than technically present: posture in `clyde doctor`'s summary, on every task run, in mission review, and in the README rather than a footnote. Posture is derived state with no configuration that can fake it, and a test asserts it cannot change an admission decision.

### Risk 9: the guest-side contract gets designed twice
Part 1b's job contract, drive identity, log transport, and artifact extraction are cheap to decide now and expensive to change once a guest init exists. Getting the transport wrong also forecloses the warm-pool option, since a VM booted before its job exists cannot have that job in its boot arguments.

**Mitigation:** decide all four before writing the guest init, and design the job channel so a pre-booted VM remains possible even though the pool is not built.

## Milestones

1. Flake, CI, crate layout, typed schemas, pure policy functions, `clyde doctor` with posture
2. Mission and lease creation, the operator task surface, confirmations on a separate channel
3. Snapshot-based `rust.check` and `rust.test.unit` with a warm per-mission cache, driven by a human with no agent involved
4. The same loop on the microVM backend as the default, with measured latency
5. `rust.resolve-deps` with confirmation-gated registry-only egress and offline rerun
6. Brokered `git.push`, TUI, and mission review — **the builder is complete**
7. The agent harness, and `enforcing` posture — the warden

## Post-MVP

- **Phase 5: frontend and full-stack** — `node.resolve-deps`, `web.build`, synthetic service support, `browser.test.synthetic`, isolated browser profiles
- **Phase 6: sub-agent depth** — multi-level derivation, parent-child lease graph views, derived budget accounting, revocation fan-out at depth
- **Phase 7: publishing and provenance** — `artifact.sign`, `artifact.publish`, release plan review, artifact lineage, in-toto/SLSA-style provenance
- **Phase 8: broader deployment** — Postgres-backed shared control plane, remote runners, editor integrations, org policy overlays
- **Raised by the split** — extracting the warden into its own binary if it earns one; a warm microVM pool if measured latency demands it; the builder as a CI component, which is the forcing function that keeps the driver interface from quietly becoming agent-shaped
- **Other** — access baseline export/import for backup and machine migration; per-open snapshot enforcement ([OQ5](decisions.md#oq5-per-open-enforcement-of-the-build-snapshot)); OpenTelemetry export; Sigstore/Cosign signing
