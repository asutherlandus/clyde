# Clyde Next: Implementation Decision Log

## Purpose

This document records the binding implementation decisions for Clyde Next Phases 0-4, with rationale and consequences.

It exists because the rest of the design document set describes *what* Clyde should be, while implementation requires committing to *how*. Where a decision here contradicts an earlier design document, this document wins and the other document has been updated to match.

Each entry has a stable identifier (`D1`, `D2`, ...). Implementation specs and code comments should reference these identifiers rather than restating rationale.

## Status legend

- **accepted**: decided, in force for Phases 0-4
- **provisional**: decided for MVP, expected to be revisited at a named point
- **open**: identified but not yet decided (see [Open Questions](#open-questions))

## D1: The coding agent runs inside a Clyde-managed workspace environment

**Status:** accepted

**Decision**
The primary coding agent process (and each sub-agent) runs inside a Clyde-managed workspace-environment sandbox, not on the host. That sandbox receives:
- the lease-scoped repository subtrees, bind-mounted read-write
- the remainder of the repository, bind-mounted read-only, where the build closure requires it
- `.git` read-only (see [D8](#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only) and [Phase 4](phase-4-credential-broker.md))
- the actor-facing Clyde socket
- an isolated scratch directory
- no host home directory, no credentials, and no egress except the Clyde proxy allowlist ([D11](#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy))

**Rationale**
If the agent runs on the host with ordinary filesystem and network access, it can invoke `cargo` directly and Clyde's typed-task pipeline becomes advisory. Phase 2's snapshot isolation would then protect only against hostile dependencies reached through Clyde, not against agent-directed execution — which is attack vector 4 in the [threat model](problem-statement-threat-model.md). Typed tasks must be the only available execution path for project code, and that is a property of the agent's environment, not of the agent's good behaviour.

**Consequences**
- Phase 1 must ship enough workspace-environment plumbing to host an agent, not just a socket API.
- Edit scope is enforced by **mount topology** rather than by proxying every file write through clyded. A lease's writable paths are exactly its read-write binds.
- The separate "edit-helper runtime" described in earlier drafts collapses into the workspace environment itself; see [D17](#d17-workspaceedit-helper-execution-is-the-workspace-environment).
- Each actor session gets its own workspace-environment sandbox with its own mount scope, which is how sub-agent narrowing is enforced.
- Clyde cannot enumerate every command the agent runs inside its own environment. Auditing of editing work is diff-based, not command-based, unless the optional exec-logging shim is enabled.

## D2: Per-actor capability tokens, with a separate human approval channel

**Status:** accepted

**Decision**
- Every actor session is issued an opaque capability token (256-bit random) when clyded binds it to a lease. Every actor-facing request carries that token. Tokens are stored hashed, expire with the lease, and are revoked immediately on lease revocation.
- clyded listens on **two** sockets:
  - `clyded.sock` — the actor API (MCP + JSON-RPC). May be bind-mounted into workspace-environment sandboxes. Requires a valid actor token.
  - `clyded-admin.sock` — the human API: mission creation, approval, denial, revocation, renewal. Never mounted into any sandbox. Host-only, mode `0600`, and callers are checked with `SO_PEERCRED`.
- Approvals are only accepted on the admin socket. There is no actor-facing operation that can approve anything.

**Rationale**
"The human approves boundary crossings" is only meaningful if an agent cannot impersonate the human. A single socket with a self-declared actor identity means any process that can reach the socket — including the agent whose escalation is being reviewed — can approve it. Splitting the channel makes self-approval structurally impossible rather than policy-prohibited.

**Consequences**
- Token delivery to the agent is by a file at `/run/clyde/session-token` (mode `0400`) inside the sandbox, never in `argv`.
- The CLI has two modes: actor commands (token, `clyded.sock`) and operator commands (admin socket). `clyde approve` is an operator command and fails if run inside a sandbox.
- Sub-agent tokens are issued by clyded on `request_subagent`, bound to the derived lease.

## D3: Build caches are per-mission and writable; dependency caches are read-only

**Status:** accepted

**Decision**
- The dependency bundle store is content-addressed and mounted **read-only**.
- Each mission gets a writable cache directory created at mission activation and destroyed at mission closeout. It holds `CARGO_TARGET_DIR` and a per-mission `CARGO_HOME` seeded by hardlink from the read-only dependency bundle.
- No cache is shared across missions, across projects, or with the host's own `~/.cargo`.

**Rationale**
A cold `target/` per task run makes the inner loop unusable for anything but toy crates ([Risk 1](mvp-implementation-roadmap.md#risk-1-inner-loop-latency-is-too-high)). A long-lived per-project cache is exactly the cache-poisoning persistence vector in the threat model. Per-mission scoping keeps the loop warm within the unit of work a human actually approved, and makes the blast radius of a hostile `build.rs` the mission it ran in.

**Consequences**
- One cold build per mission is accepted and should be surfaced in the UX as such.
- Mission closeout must delete the cache; cache size counts against mission budget.
- `CARGO_HOME` is per-mission and writable because cargo writes lockfiles into it even when offline; it is seeded from the read-only bundle rather than mounted from it.

## D4: Phase 0 delivers flake, CI, crate layout, and typed schemas

**Status:** accepted

**Decision**
Phase 0 commits: the Nix flake (Rust toolchain, `nextest`, `clippy`, `rustfmt`, `cargo-deny`, auxiliary CLIs), CI wired to that flake, the cargo workspace crate layout, and Rust type definitions for every Phase 0 entity with `serde`, validation, state machines, and unit tests — but no runtime behaviour.

**Rationale**
The schemas are the interface contract for Phases 1-4. Prose schemas are not checkable; typed ones are. Landing the flake first also satisfies the standing rule in [AGENTS.md](../AGENTS.md) that the flake is the source of truth for tooling, which cannot be honoured retroactively.

**Consequences**
See [Phase 0](phase-0-foundations.md) for the full deliverable list and exit criteria.

## D5: Bubblewrap first, behind a `SandboxBackend` trait

**Status:** provisional — revisited at Phase 2b ([D9](#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution))

**Decision**
Phase 2a implements a bubblewrap-based sandbox backend behind a `SandboxBackend` trait: user/pid/ipc/uts/cgroup namespace isolation, read-only binds for runtime roots and snapshots, tmpfs scratch, `--unshare-net` for offline tasks, seccomp filter, `--die-with-parent`, `--new-session`, cleared environment, and cgroup v2 resource limits applied by clyded. Rootless Podman is **not** implemented.

**Rationale**
The stated goal is to reach Firecracker quickly. Bubblewrap gets the snapshot/task/artifact pipeline exercised end-to-end at very low startup cost and with no image-build pipeline, and it consumes the same nix-closure runtime roots that the Firecracker rootfs will be built from ([D6](#d6-runtime-roots-are-nix-closures-not-oci-images)). Building a Podman backend in between would be work thrown away.

**Consequences**
- Bubblewrap is explicitly a weaker boundary than the design calls for on untrusted project execution. It is acceptable only because Phase 2b follows immediately and no network-bearing task ships on it ([D9](#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)).
- Host prerequisites are non-trivial on Ubuntu 24.04 (`kernel.apparmor_restrict_unprivileged_userns=1` blocks unprivileged user namespaces for unconfined binaries, which includes nix-store binaries). `clyde doctor` must detect this and explain the fix. See [Phase 0 host prerequisites](phase-0-foundations.md#host-prerequisites).
- Resource limits come from cgroup v2 delegation (`systemd-run --user --scope`), and are mandatory for T2 and above: without delegation, build tasks are refused rather than degraded ([D22](#d22-no-degraded-resource-limits-for-untrusted-execution)).

## D6: Runtime roots are nix closures, not OCI images

**Status:** accepted

**Decision**
Each task family's execution root is a nix derivation in the project flake, identified by its store path. Sandboxes bind the closure read-only. There is no OCI image build, registry, or digest-pinning pipeline.

Named runtime roots for the MVP:
- `runtimeRoots.workspace` — text and code manipulation tooling; **no** rustc/cargo/node/browser/signing tooling
- `runtimeRoots.rust` — Rust toolchain for check/test/build
- `runtimeRoots.fetch` — cargo plus network client tooling for dependency resolution

**Rationale**
The flake is already the source of truth for tooling. Reusing it for runtime roots gives reproducibility and content-pinning for free, keeps one definition of "the Rust toolchain we use", and removes upstream base-image trust from the supply chain. It also composes with Firecracker, where the guest rootfs is built from the same closure.

**Consequences**
- nix is required on any host that executes tasks, not just on developer machines. This is a deliberate narrowing of portability for the MVP.
- The Firecracker rootfs in Phase 2b is a squashfs (or equivalent) built from the same closure, so the runtime-root identity is stable across backends.
- The hard architectural requirement that the workspace runtime root must not contain the project build toolchain is now enforceable by inspecting a derivation, and should be asserted in a test.

## D7: `registry-only` egress is enforced by a Clyde-managed proxy

**Status:** accepted

**Decision**
Network-bearing tasks are given a loopback-only network namespace plus a bind-mounted Unix socket. A trusted in-sandbox forwarder exposes `127.0.0.1:<port>` and bridges to the host-side Clyde egress proxy over that socket. The proxy terminates nothing: it accepts HTTP `CONNECT`, allowlists by destination host, and logs every attempt (allowed and denied) into the task's fetch manifest.

**Rationale**
Rootless network namespaces cannot create veth pairs, so host-side firewalling of a sandbox netns is not available without privilege. Socket-bridging a CONNECT proxy needs no privilege, works identically under bubblewrap and (over vsock) under Firecracker, and makes the allowlist decision host-side and trusted. It is what makes the approval prompt's "network: registry-only" claim true rather than aspirational.

### Amendment: selective TLS termination for `model-api` only
The proxy terminates TLS for hosts in the `model-api` profile, and **only** those hosts, so that it can inject the model API credential host-side ([D11](#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)). The agent then never holds that credential.

Every other profile — `rust-registry` included — remains pure `CONNECT` pass-through with end-to-end TLS and no interception. Intercepting dependency traffic would put Clyde in the path of dependency *content* and undermine the content-hash story that [D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift) depends on. This carve-out is narrow on purpose and must stay that way.

Consequences of the carve-out:
- Clyde generates a per-installation CA at first run. The private key is host-only, never in a sandbox, never in an artifact.
- The CA certificate is mounted read-only into the workspace environment and pointed at by `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, and equivalents. Build and fetch sandboxes do **not** receive it.
- The proxy becomes a plaintext-handling trusted component for model traffic. It sees prompts and completions, so it must not log bodies. Recordable metadata is host, request path, status, byte counts, and timing — never content.
- Per-mission request and byte budgets on `model-api` egress become the cost control, since the agent can spend the credential without holding it.

**Consequences**
- Allowlisting is by CONNECT target host. Except for the `model-api` carve-out above, TLS is end-to-end, so there is no payload inspection and no content filtering. This limitation must be stated in the approval UX.
- Every egress-bearing task profile names an **egress profile**, and the set of profiles is closed. See [Network egress model](network-egress-model.md).
- The in-sandbox forwarder is trusted code running in an untrusted netns. Compromising it grants nothing beyond the allowlist the proxy already enforces.

## D8: Brokered `git.push` uses the developer's existing credential, inside the broker only

**Status:** provisional — revisit when multi-user or shared-runner support is added

**Decision**
`clyde-brokerd` uses the developer's existing SSH key or `ssh-agent` connection, held only in the broker process. No credential, socket, or token is ever exposed to an agent, a workspace environment, or a build sandbox. Every push is gated by a remote allowlist, a branch-pattern allowlist, and a per-push approval bound to the exact request.

**Rationale**
This proves the "brokered, not mounted" property immediately and with no provisioning friction. The credential's own scope is unchanged from today, which is honest: Clyde's contribution here is eliminating exposure paths and adding approval and audit, not reducing the credential's inherent authority.

**Consequences**
- Push must not execute repository-controlled code. The workspace `.git/config` and `.git/hooks` are untrusted content. The broker therefore pushes from a **sanitized temporary repository**, fetching the approved commit from the workspace repo with hooks and object-transfer hooks disabled, then pushing with hooks disabled and system/global git config neutralised. See [Phase 4](phase-4-credential-broker.md#5-hostile-repository-hardening).
- Commit creation is a trusted clyded operation (`git.commit.prepare`), not an agent operation, and `.git` is read-only inside workspace environments — otherwise an agent could plant a `pre-push` hook.

## D9: The Firecracker backend lands as Phase 2b, before dependency resolution

**Status:** accepted

**Decision**
The roadmap phase order becomes: Phase 2a (bubblewrap + snapshots + safe inner loop) → **Phase 2b (Firecracker backend behind the same trait)** → Phase 3 (dependency resolution) → Phase 4 (broker). The microVM work is removed from the old Phase 6 position.

**Rationale**
Phase 3 is the first phase to give untrusted execution any network reachability at all. Running that on the weaker boundary, and then reworking the egress plumbing for Firecracker afterwards, is the worse order in both security and effort terms. Building the vsock-based egress path once, against the intended backend, is cheaper.

**Consequences**
- Phase 2b must reach parity for `rust.check`, `rust.test.unit`, and the workspace environment before Phase 3 begins.
- Task policy names a **required minimum isolation level**, and backend selection is policy-driven, not manual.
- Phase 2a's bubblewrap backend is retained after 2b for low-risk task classes and for hosts without KVM.

## D10: MCP is the primary actor-facing API

**Status:** accepted

**Decision**
clyded exposes its actor-facing operations as MCP tools on `clyded.sock` from Phase 1. The internal command model is transport-agnostic; JSON-RPC is retained for the CLI and admin surface.

Initial tool set: `mission_status`, `list_capabilities`, `run_task`, `task_status`, `task_logs`, `list_artifacts`, `request_escalation`, `request_subagent`, `request_publish`, `commit_prepare`.

**Rationale**
It makes the MVP drivable by an existing agent with no bespoke adapter, which is what makes the delegation workflow provable end-to-end in Phase 1 rather than at the end of the roadmap.

**Consequences**
- File reading and editing are **not** MCP tools. The agent uses its own filesystem tools against the mounted workspace; scope is enforced by mount topology ([D1](#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)).
- Tool descriptions are part of the security UX: each must state the policy consequences of the call (network, credentials, approval requirement).

## D11: Workspace-environment model-API egress goes through the Clyde proxy

**Status:** accepted

**Decision**
The workspace environment's only egress is the Clyde proxy under an egress profile allowlisting the configured model API host(s). All attempts are logged.

**Rationale**
One egress mechanism for the whole system, and the proxy is needed in Phase 1-2 for this reason regardless of Phase 3. The alternative — unrestricted egress for the workspace environment — leaves one environment with an unaudited exfiltration path.

### Credential handling
The agent holds **no** model API credential. The proxy terminates TLS for `model-api` hosts and injects the `Authorization` header host-side ([D7 amendment](#amendment-selective-tls-termination-for-model-api-only)).

This resolves what would otherwise be a contradiction: the workspace environment is specified as holding no credentials, yet something must authenticate. Injecting the key into the sandbox was the simpler option and was rejected in favour of the agent never possessing it, so that a compromised or prompt-injected agent cannot exfiltrate a reusable credential — only spend the channel while it holds a lease.

**Consequences**
- The proxy is a **Phase 1** component, not a Phase 3 component.
- Clyde holds a CA whose certificate the workspace environment trusts. That is a real trusted-surface increase, scoped to model hosts only.
- The proxy sees prompt and completion plaintext. It must not log bodies; metadata only.
- Model egress needs request and byte budgets, since the agent can spend the credential without holding it.
- Honest caveat to state in the docs and UX: an allowlisted API endpoint that accepts arbitrary request bodies is still an exfiltration channel for a misbehaving agent. What this buys is that it is the *only* such channel, that it is logged, and that the credential itself cannot be stolen and reused elsewhere.

## D12: Existing docs are updated in place; phase specs and this log are added

**Status:** accepted

**Decision**
No document in `docs/` may contradict a decision recorded here. New documents cover what had no home: this decision log, a schema reference, the network egress model, the agent/workspace-environment design, and one implementation spec per phase.

## D13: CLI through Phases 1-3; TUI in Phase 4

**Status:** accepted

**Decision**
Phases 1-3 ship a complete CLI with human-readable and `--json` output. The ratatui TUI is a Phase 4 deliverable, once the mission, task, and approval surfaces have stabilised.

**Consequences**
The CLI is also the integration-test harness, so `--json` output stability matters from Phase 1.

## D14: Machine-readable policy in `.clyde/`, `AGENTS.md` advisory only

**Status:** accepted

**Decision**
- `.clyde/policy.toml` in the repository holds machine-readable settings: allowed registries, subtree defaults, mission task pre-approvals, push remote/branch patterns, snapshot exclusions, synthetic service defaults.
- `AGENTS.md` is human/agent prose. It is passed into agent context and is never parsed for authority.
- Precedence: built-in defaults → host config → user config → repo config. **Repo config may only narrow.** Any repo-config value that would widen authority relative to the layer above is rejected with a diagnostic, not silently clamped.

**Rationale**
Repository content is untrusted. A repo that can widen its own authority is a self-signed permission slip. Keeping the advisory channel textual and the authority channel narrow-only keeps that boundary legible.

## D15: Three binaries from Phase 0

**Status:** accepted

**Decision**
Cargo workspace with three binaries — `clyde` (CLI), `clyded` (control plane), `clyde-brokerd` (broker) — plus library crates. `clyde-brokerd` exists from Phase 0 as a stub that answers capability queries only; it gains real operations in Phase 4.

**Rationale**
The broker boundary is the one the design cares most about and the one where a retrofit is most likely to leak — shared process state, in-process credential access, an "internal" call path that skips audit. Establishing the process boundary while it is trivially cheap avoids that.

## D16: One active mission per workspace

**Status:** accepted

**Decision**
Multiple registered workspaces are supported. Each workspace has at most one active mission at a time, which may have sub-agent leases beneath it. The schema carries the identifiers needed to relax this later.

**Rationale**
Concurrent missions editing one live mutable tree raises edit-conflict and cache-partitioning questions the design has not answered. Per-mission cache scoping ([D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)) is also materially simpler under this constraint.

## D17: `workspace.edit` helper execution *is* the workspace environment

**Status:** accepted — consequence of [D1](#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)

**Decision**
Earlier drafts described helper-driven `workspace.edit` as a separate low-authority sandbox that Clyde launches on request. Since the agent now runs inside exactly such a sandbox ([D1](#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)), that separate runtime is redundant. The workspace environment *is* the edit-helper runtime, and the agent running a codemod there requires no task request.

**What is preserved**
All the properties that made the separate runtime a requirement still hold, because they are now properties of the agent's own environment:
- separate runtime root from build/test/fetch ([D6](#d6-runtime-roots-are-nix-closures-not-oci-images))
- text and code manipulation tooling, and no project build toolchain
- writable paths limited to lease scope, enforced by mount topology
- no raw credentials, no host home, no container runtime socket
- no egress beyond the model-API allowlist

**Consequences**
- `workspace.edit` remains a task *type* in the catalog for audit and policy purposes, but in the MVP it describes a class of activity rather than a Clyde-launched sandbox.
- Edit auditing is diff-based, computed at snapshot and task boundaries. An optional exec-logging shim in the workspace runtime root can add command-level records; it is a nice-to-have, not a boundary.
- A command that needs the project build toolchain simply cannot run in the workspace environment — the tooling is absent. That is the enforcement, and it is stronger than policy text.

## D18: Build access is baselined, pinned in Clyde state, and escalated on drift

**Status:** accepted

### The threat this addresses
Malicious code arriving in a transitive dependency and executing during compilation, via `build.rs` or a proc macro. This is the primary threat the build-task design is tuned for, and it is not addressed by snapshot *breadth* decisions alone — what matters is detecting **change**.

### Decision
Build and test tasks run against a pinned **access baseline** with two components:

1. **Repo read set** — two tiers: *subtree grants* for first-party project code, and *file pins* for anything outside them. See [the two tiers](#the-two-tiers-and-why) below.
2. **Code-execution inventory** — every dependency package that has a `build.rs` or is a proc-macro crate, pinned by crate, version, **and source content hash**.

The baseline is authoritative in **Clyde's own state**, keyed by workspace, task type, and build target. It is never stored in the repository, because repository content is untrusted and an attacker who could edit the baseline could hide their own drift.

### The two tiers, and why
The threat is dependency code changing. Editing the project is not the threat — it is the work. Treating both at the same granularity would make an agent adding a source file indistinguishable from a build script reaching somewhere new, and a control that fires on ordinary editing gets clicked through.

So the repo read set has two tiers with deliberately different sensitivity:

**Subtree grants — first-party project code, not drift-sensitive.**
- the subtrees the human approved in the mission envelope: edit paths and read paths
- the subtrees the static build closure requires within that approved scope: workspace root manifests, member manifests, in-repo path dependencies

Creating, renaming, moving, or deleting files inside a granted subtree is ordinary work. It produces no prompt, no drift, and no baseline amendment. The human already approved these subtrees when they approved the mission envelope, and that is where the review happens — once, up front, over a scope rather than over a file list.

**File pins — everything outside the granted subtrees, drift-sensitive.**
- an `include_str!` target in `docs/`
- a fixture file a test reads from a shared directory
- a single sibling crate a build script reads from

These are admitted individually, each confirmed by a human, and each recorded. A read outside grants ∪ pins is drift.

**Absolute exclusions**, applied before grants and not overridable by them: `.env*` and similar secret-shaped files, key material, `target/`, `node_modules/`, and `.git` by default. A subtree grant over `backend/auth` does not admit `backend/auth/.env.local`.

This revises the earlier "file-level with directory rollup" granularity: rollup is still how pins are rendered when a whole out-of-scope directory is read, but first-party code is granted by subtree and is not file-pinned at all.

### What is not drift
Stated explicitly, because getting this wrong is how the control becomes noise:
- a new, renamed, moved, or deleted file inside a granted subtree
- a new module, test, or fixture inside a granted subtree
- a new in-repo path dependency on a crate **inside** the approved scope

### What is drift
- a read outside grants ∪ pins — including a new in-repo path dependency on a crate **outside** the approved scope, which is a genuine widening of what the mission reaches into, and rare enough to be worth a prompt
- a new code-executing crate in the dependency inventory
- a version change to a code-executing crate
- a content-hash change at the same version, which is a registry-tampering signal

### Enforcement
Repo read enforcement is **by materialization**: the snapshot contains the granted subtrees (minus exclusions) plus the pinned files, and nothing else, so a read outside that fails with `ENOENT`. No tracing, no privilege, and identical behaviour on both sandbox backends.

Because grants are subtrees, materialization picks up newly created first-party files automatically on the next snapshot — which is exactly the behaviour that keeps the inner loop free of prompts.

Inventory enforcement is **pre-execution**: the inventory is computed from the lockfile and dependency bundle before the sandbox starts, so a newly-arrived or changed build script is caught *before it runs*, not after.

### Establishing a baseline
1. **Static proposal (normal path).** Clyde computes the cargo build closure — workspace root manifest, `Cargo.lock`, cargo config, toolchain file, every workspace member's manifest, and full source for the target crate plus its transitive in-repo path dependencies — and proposes it as the initial baseline for human confirmation.
2. **Learn mode (fallback).** For reads static analysis cannot see (`include_str!` to an arbitrary path, a `build.rs` reading repo files), a human may run the task in learn mode: auto-admit within the mission's read scope, observe actual reads, and produce a baseline proposal for review.

Learn mode is itself a privilege and is constrained accordingly:
- human-invoked on the admin channel only, never reachable by an actor
- never a default, and never a fallback Clyde selects on its own
- scoped to a single run
- marked distinctly in the audit log
- produces a proposal that has no effect until a human confirms it

The static-proposal path exists specifically so that learn mode is rarely needed, since a learn run is a wide-scope execution of the code you are trying to constrain.

### Drift and escalation
In enforce mode, denial and drift are the same event: a build that wants something outside grants ∪ pins fails, and Clyde renders that as an escalation naming the path rather than surfacing a raw cargo error.

The two drift classes are presented differently, because they carry very different signal:
- **path drift** — uncommon once grants are set, usually a build script or macro reaching outside the project's approved scope
- **dependency drift** — the one this control exists for, checked before execution and reported as an inventory diff

The approval prompt shows the diff against the pinned baseline, not the whole baseline, so the human reviews what changed.

### Observation mechanism for learn mode
- namespace backend: host-side `inotify` (`IN_OPEN`/`IN_ACCESS`) on the materialised tree — unprivileged and adequate, subject to watch limits on large trees
- microVM backend: host-side inotify cannot see guest reads; observation requires serving the snapshot through a Clyde-owned virtio-fs/FUSE daemon

That FUSE path is also the route to per-open *enforcement* with exact denied-path reporting instead of `ENOENT` inference. It costs build performance, since cargo does very large numbers of `stat` calls, and is deferred ([OQ5](#oq5-per-open-enforcement-via-a-fuse-served-snapshot)).

### Consequences
- Snapshot scope is no longer a policy question with a fixed answer; it is per-target recorded state: approved subtrees plus individually confirmed pins.
- First-party development is prompt-free by construction. Churn cannot come from editing the project, only from reaching outside it or from dependency change.
- A task with no baseline for its target is refused, with the static proposal offered. Fail-closed, and no implicit wide-scope first run.
- Because the baseline lives only in Clyde state, it is invisible to code review, is not shared across a team or a fresh clone, and is lost with Clyde's state. Mitigations: the mission approval envelope summarises the baseline in force, and an export/import command for backup and machine migration is worth adding (post-MVP).
- Env-var reads and subprocess execs are not baselined in the MVP. Each is a real improvement and neither is needed for the change-detection property.

## D19: MCP over the actor socket is line-framed JSON-RPC

**Status:** accepted

**Decision**
The actor surface speaks newline-delimited JSON-RPC directly over `clyded.sock`, treating the socket as a duplex stream — which is what MCP's stdio transport already is. No HTTP stack on the actor surface.

**Rationale**
Minimal machinery, and the same framing the admin JSON-RPC surface uses, so there is one message codec in the codebase rather than two. An agent that can only spawn a stdio subprocess can be bridged with a small shim; that shim is a compatibility detail, not part of the boundary.

**Consequences**
- If remote deployment is added later, streamable HTTP becomes the transport for that case and the internal command model is already transport-agnostic.
- Message size limits and backpressure are the codec's responsibility and must be explicit, since an actor is untrusted input.

## D20: The agent command is host or user configuration, never repository configuration

**Status:** accepted

**Decision**
What binary clyded execs as the agent — command, arguments, and environment allowlist — comes from host or user configuration only. `.clyde/policy.toml` cannot set, extend, or override it.

**Rationale**
Repository content is untrusted. A repo that can choose what Clyde executes as the agent has arbitrary code execution in the workspace environment on `clyde mission create`, before any policy applies. This follows from [D14](#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only) but is worth stating separately because it is the highest-consequence instance of it.

**Consequences**
- The config loader treats `agent.*` keys in repository config as a hard rejection, not a narrowing check.
- The same rule covers runtime root selection: a repository cannot choose which runtime root a task executes against.

## D21: `.git` is never available to build tasks

**Status:** accepted — resolves the former OQ2

**Decision**
`.git` is an absolute exclusion from build snapshots. It cannot be admitted by a subtree grant, by a file pin, or by configuration. There is no flag.

Git metadata for builds that want it — commit sha, timestamp, describe output, dirty flag — is **not supported in the MVP**. If the need arises it will be added as a typed Clyde task that computes the metadata in the trusted control plane and hands it to the build as an ordinary input, never by admitting the repository history.

**Rationale**
Repository history routinely contains secrets that were removed from HEAD. Handing `.git` to a build script therefore hands hostile dependency code every credential ever committed to the repository, including ones the developer believes they have deleted. That is a materially worse exposure than the working tree itself, and it is invisible to anyone reasoning only about what the current checkout contains.

The alternatives — a pinnable full `.git`, or a shallow depth-1 clone — both put git objects in front of untrusted execution to serve a small convenience. Deferring the convenience is cheaper than getting the sanitisation right.

**Consequences**
- A build that reads `.git` fails. The failure must be diagnosed specifically: `.git` is never available to build tasks, this is not drift, and git metadata is not yet supported. A raw cargo error here would send someone hunting for a bug that does not exist.
- `vergen`-style crates and anything shelling out to `git` in a `build.rs` will not work until the metadata task exists. That is a known, accepted MVP limitation and belongs in the user-facing docs, not only here.
- The exclusion list stays a single class: absolute, with nothing pinnable. The two-tier baseline model gains no second kind of exception.

## D22: No degraded resource limits for untrusted execution

**Status:** accepted — resolves the former OQ4

**Decision**
Cgroup v2 limits are mandatory for T2 and above. On a host where cgroup v2 delegation is unavailable, build and test tasks are **refused**. There is no opt-in, no fallback, and no configuration that permits running them on rlimits and a wall-clock timeout alone.

The workspace environment (T0/T1) may still run, with rlimits and a timeout, since it hosts a semi-trusted agent rather than hostile dependency code. `clyde doctor` reports missing delegation as a hard failure for build capability, not a warning.

**Rationale**
rlimits are a genuinely weaker bound, not an equivalent one: `RLIMIT_NPROC` is per-user rather than per-sandbox, and `RLIMIT_AS` is per-process, so a hostile build script can still exhaust the host's process table or memory collectively. Permitting that behind a flag means Clyde would sometimes run hostile code with weaker limits than its own documentation claims, and a flag that weakens a stated boundary tends to end up set.

**Consequences**
- Clyde will not run build tasks on hosts without systemd user delegation — some container environments and minimal distributions. This is a real portability cost, accepted deliberately.
- The refusal is transitional. Phase 2b moots it: a microVM's memory and vCPU allocation bounds the workload by construction, with no cgroup delegation required.
- `clyde doctor` must distinguish "no cgroup v2", "cgroup v2 present but not delegated", and "delegated but missing controllers", because the remedies differ.

## Implementation refinements

These arose while implementing Phases 0-4. Each is a narrowing or a clarification
of a decision above rather than a reversal; where one changes what a decision
said, it says so.

### R1: The actor token binds a connection, and is re-resolved on every request

**Refines** [D2](#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel).

D2 says every actor-facing request carries the capability token. MCP has no
per-request header, and threading a token through every tool call's arguments
would put it in a place tool schemas and client logs can see.

So the token is presented once, in `initialize`, and binds the connection. The
property D2 exists for is preserved by re-resolving token → session → lease →
mission on **every** request rather than caching the resolution: an expired or
revoked lease stops working immediately rather than at the next connection. The
socket is bind-mounted only into the sandbox the token belongs to, so the
connection and the token identify the same actor either way.

What this gives up: a stolen connection is as good as a stolen token for as long
as it stays open. Since the connection is a Unix socket inside one sandbox, an
attacker who can hold it already has that sandbox.

### R2: `clyde-git` is a crate

**Extends** the [Phase 0 crate layout](phase-0-foundations.md#3-crate-layout).

Phase 4 requires exactly one place in the codebase that constructs a git command.
Both clyded (diff, commit preparation) and clyde-brokerd (push) need it, and they
are separate processes, so it is a crate rather than a module: `crates/clyde-git`.

Its sanitisation is applied by construction — there is no constructor that
produces an unsanitised invocation — and the one value a caller may influence is
the ssh command, which is how the broker supplies a credential.

### R3: Learn-mode observation uses the `inotify` crate

**Implements** the observation mechanism in
[D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift).

Host-side `inotify` on the materialised tree needs syscalls. The `inotify` crate
is a thin safe wrapper, which keeps the workspace free of `unsafe` (forbidden at
the workspace level) without a `pre_exec` hook or hand-rolled fd handling.

The observation reports how many directories it could not watch, so an incomplete
observation — a tree past the host's watch limit — cannot be mistaken for a
complete one.

### R4: Resource limits are applied by wrapping the command

**Implements** the limits in
[D5](#d5-bubblewrap-first-behind-a-sandboxbackend-trait) and
[D22](#d22-no-degraded-resource-limits-for-untrusted-execution).

Limits are applied by wrapping the sandbox command in `systemd-run --user
--scope` and `prlimit` rather than by setting rlimits in a `pre_exec` hook. That
keeps the whole mechanism at the argv level: it contains no `unsafe`, and the
exact command Clyde runs is a value a test can assert on and an audit record can
carry.

`RLIMIT_NPROC` is deliberately not set. It is per-user, so it would bound the
developer's whole login session rather than the sandbox, and would not bound the
sandbox at all if other processes were already running. Process count is bounded
by `TasksMax` in the cgroup, which is per-scope.

### R5: The seccomp filter is a deny list, and is passed on the child's stdin

**Implements** the seccomp filter in
[Phase 2a](phase-2-execution-and-isolation.md#2-bubblewrap-backend).

`bwrap --seccomp FD` reads a filter from an inherited descriptor. Passing it as
the child's stdin keeps this in safe Rust: `Stdio::from(File)` places the file on
descriptor 0 with no fd manipulation. A sandboxed task has no use for stdin.

The policy is a deny list over an allow-by-default base. An allow-list is the
stronger shape, but a build sandbox runs cargo, rustc, a linker, and arbitrary
`build.rs` code, whose syscall surface is wide and toolchain-dependent. An
allow-list tight enough to be worth having would break builds on the next
toolchain bump, and a filter that gets disabled to make builds work is worth
nothing. The deny list closes the escape and privilege-manipulation calls that
namespace isolation cares about; the namespace and cgroup boundaries remain the
primary control.

### R6: A test-only backend exists, and never ships

**Extends** [D5](#d5-bubblewrap-first-behind-a-sandboxbackend-trait).

Integration tests need to exercise the pipeline — admission, snapshot, execution,
classification, artifacts, audit — on hosts that cannot create unprivileged user
namespaces, which includes containers and a default Ubuntu 24.04 install.

A backend with no isolation at all lives behind the `test-backend` cargo feature,
which no shipped binary enables, and reports `BackendKind::TestOnly` so a run on
it is identifiable in the audit record. The daemon's own registry construction
never adds it; a test must inject it explicitly.

Everything asserted with it is a statement about the pipeline. The isolation
boundary is asserted separately and structurally, over every sandbox
specification the system can generate, which is a check that runs on every host.

### R7: A remote is approved by name; the broker resolves the URL

**Refines** [D8](#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only).

The push request digest covers the remote *name*, refspec, commit, and tree, but
not the resolved URL. A human approves a remote name, and the URL comes from the
broker's own configuration — never from the workspace repository, whose
configuration is attacker-controlled content in this threat model.

So a configuration change to a remote's URL is not an approval mismatch, while a
different remote is. That is the intended reading of "approved for one remote".

### R8: Replay protection is the brokered operation's state

**Refines** [Phase 4 deliverable 2](phase-4-credential-broker.md#2-clyde-brokerd).

Phase 4 says the broker refuses an operation whose approval is already consumed.
Consumption and execution cannot be transactional across two processes, and the
correct ordering is consume-then-push: a push that ran under a consumed approval
is recoverable, while one that ran under an unconsumed approval is a second push
waiting to happen. That ordering means the broker would always see the approval
as consumed.

So the broker verifies a fact it can check for itself: clyded moves the brokered
operation to `executing` immediately before calling, a terminal operation cannot
re-enter that state, and the broker refuses anything not in it. Single-use
consumption remains the control plane's bookkeeping; the operation state is the
replay protection the broker enforces independently.

### R9: `clyde doctor` works without a daemon

**Extends** [Phase 0 deliverable 11](phase-0-foundations.md#11-clyde-doctor).

Bring-up is exactly when the daemon is not running, so `clyde doctor` falls back
to probing the host directly when it cannot reach the admin socket. A diagnostic
that requires the thing it is diagnosing is no diagnostic at all.

## Cross-cutting consequences

### Snapshot scope is baselined, not merely closure-derived
Cargo cannot build a subtree in isolation: it needs the workspace root manifest, `Cargo.lock`, cargo configuration, every workspace member's manifest, and full source for in-repo path dependencies. That static closure routinely exceeds a lease's edit scope.

Read-only admission of those paths is not an increase in authority — the write set remains the lease's edit paths, so the delta is confidentiality only. But breadth is still exfiltration surface for hostile build code, so the closure is the *starting point* for a baseline rather than the answer: the pinned baseline is what a task actually receives, and it is confirmed by a human ([D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

### Audit chain
`audit_events` is append-only with a monotonic sequence number and a `prev_hash` chain, giving tamper-evidence cheaply. See [Schema reference](schema-reference.md#audit-events).

### Structured task failure classification
Phase 3's escalation flow depends on distinguishing "compile failed because the code is wrong" from "compile failed because dependencies are not present". Task results therefore carry a structured failure classification from Phase 2a, not just an exit code.

## Open Questions

These are identified and not yet decided. None block the start of Phase 0.

### OQ3: Exec-logging shim in the workspace environment
Whether to ship the optional command-level exec logger described in [D17](#d17-workspaceedit-helper-execution-is-the-workspace-environment) inside the MVP window, or defer it. It improves audit fidelity for editing work and is not a security boundary. **Proposed:** defer to post-MVP.

### OQ5: Per-open enforcement via a FUSE-served snapshot
Whether to serve build snapshots through a Clyde-owned FUSE or virtio-fs daemon, giving per-open enforcement and exact denied-path reporting instead of inferring denials from `ENOENT`, and giving learn-mode observation that works on the microVM backend. It costs build performance and is a substantial component. **Proposed:** defer past the MVP; use materialization plus inotify first, and revisit if `ENOENT`-based diagnostics prove too weak in practice.

### Resolved
- **OQ1** — superseded by [D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift): snapshot scope is a pinned baseline seeded from the build closure, not a fixed policy.
- **OQ2** — resolved by [D21](#d21-git-is-never-available-to-build-tasks): `.git` is absolutely excluded; git metadata is deferred to a future typed task.
- **OQ4** — resolved by [D22](#d22-no-degraded-resource-limits-for-untrusted-execution): no degraded limits for T2 and above; build tasks are refused on hosts without cgroup v2 delegation.
