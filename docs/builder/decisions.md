# Builder: Implementation Decision Log

The binding implementation decisions for the builder, with rationale and consequences. Where a decision here contradicts another design document, this document wins and the other has been updated to match.

Each entry has a stable identifier (`D1`, `D2`, …). Implementation specs and code comments reference these identifiers rather than restating rationale.

**Read [D23](#d23-the-mvp-splits-into-the-builder-and-the-warden) first.** It changes what several earlier decisions are decisions *about*.

Four decisions belong to the warden and live in [its log](../warden/decisions.md): **D1** (the agent runs inside a Clyde-managed workspace environment), **D11** (model-API egress through the Clyde proxy), **D17** (`workspace.edit` helper execution *is* the workspace environment), and **D20** (the agent command is host or user configuration). Everything else is here.

**Status legend** — **accepted**: decided, in force. **provisional**: decided for now, expected to be revisited at a named point. **superseded**: replaced by a later decision, which is named; kept because the reasoning still explains the shape of the code. **open**: identified but not yet decided.

## D2: Per-actor capability tokens, with a separate human approval channel

**Status:** accepted

**Decision**
- Every actor session is issued an opaque capability token (256-bit random) when clyded binds it to a lease. Every actor-facing request carries that token. Tokens are stored hashed, expire with the lease, and are revoked immediately on lease revocation.
- clyded listens on **two** sockets: `clyded.sock`, the actor API (MCP + JSON-RPC), which may be bind-mounted into workspace-environment sandboxes and requires a valid actor token; and `clyded-admin.sock`, the human API — mission creation, approval, denial, revocation, renewal — never mounted into any sandbox, host-only, mode `0600`, callers checked with `SO_PEERCRED`.
- Approvals are only accepted on the admin socket. No actor-facing operation can approve anything.

**Rationale**
"The human approves boundary crossings" is only meaningful if an agent cannot impersonate the human. A single socket with a self-declared actor identity means any process that can reach it — including the agent whose escalation is being reviewed — can approve it. Splitting the channel makes self-approval structurally impossible rather than policy-prohibited.

**Consequences**
- Token delivery to a hosted actor is by a file at `/run/clyde/session-token` (mode `0400`) inside the sandbox, never in `argv`.
- The CLI has two modes: actor commands (token, `clyded.sock`) and operator commands (admin socket). `clyde approve` is an operator command and fails if run inside a sandbox.
- Sub-agent tokens are issued by clyded on `request_subagent`, bound to the derived lease.

### Amendment: confirmation semantics when the operator is the driver
The two-socket split exists to stop an actor approving its own escalation. When the driver *is* the human — the operator surface in [D25](#d25-task-execution-has-an-operator-surface-on-the-admin-socket) — there is no second party, and the operator both requests and confirms.

That does not make the prompt worthless, and it must not be quietly removed. The value in the fetch prompt was never that a second party signed off; it was that a human saw the code-execution inventory diff **before the build script ran**. So the prompt stays, the record stays, and it is called a *confirmation* rather than an approval, because describing a self-confirmation as an approval would overstate what happened.

The socket split itself is retained in every posture. It is not load-bearing for a human-only builder deployment, but it is for the warden and for external drivers.

## D3: Build caches are per-mission and writable; dependency caches are read-only

**Status:** accepted

**Decision**
The dependency bundle store is content-addressed and mounted **read-only**. Each mission gets a writable cache directory created at mission activation and destroyed at closeout, holding `CARGO_TARGET_DIR` and a per-mission `CARGO_HOME` seeded by hardlink from the read-only bundle. No cache is shared across missions, across projects, or with the host's own `~/.cargo`.

**Rationale**
A cold `target/` per task run makes the inner loop unusable for anything but toy crates ([Risk 1](roadmap.md#risk-1-inner-loop-latency-is-too-high)). A long-lived per-project cache is exactly the cache-poisoning persistence vector in the threat model. Per-mission scoping keeps the loop warm within the unit of work a human actually approved, and makes the blast radius of a hostile `build.rs` the mission it ran in.

**Consequences**
- One cold build per mission is accepted and should be surfaced in the UX as such.
- That cold build is the cost this decision trades for, and it is deferred rather than settled: whether dependency build output should move to a shared, input-addressed store is [D28](#d28-dependency-build-output-stays-in-the-per-mission-cache-for-the-mvp), revisited against Part 1b's measurements.
- Mission closeout must delete the cache; cache size counts against mission budget.
- `CARGO_HOME` is per-mission and writable because cargo writes lockfiles into it even when offline; it is seeded from the read-only bundle rather than mounted from it.
- A warm cache is only useful if the build tool agrees the sources are unchanged, which is a property of how snapshots are materialised rather than of the cache ([D27](#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)).

## D4: Phase 0 delivers flake, CI, crate layout, and typed schemas

**Status:** accepted

**Decision**
Phase 0 commits the Nix flake (Rust toolchain, `nextest`, `clippy`, `rustfmt`, `cargo-deny`, auxiliary CLIs), CI wired to that flake, the cargo workspace crate layout, and Rust type definitions for every Phase 0 entity with `serde`, validation, state machines, and unit tests — but no runtime behaviour.

**Rationale**
The schemas are the interface contract for the later phases. Prose schemas are not checkable; typed ones are. Landing the flake first also satisfies the standing rule that the flake is the source of truth for tooling, which cannot be honoured retroactively.

**Consequences** — see [Phase 0](roadmap.md#phase-0-foundations).

## D5: Bubblewrap first, behind a `SandboxBackend` trait

**Status:** superseded as the end state by [D24](#d24-firecracker-is-the-default-backend-for-build-execution); the trait and the namespace backend stand

**Decision**
Part 1a implements a bubblewrap-based sandbox backend behind a `SandboxBackend` trait: user/pid/ipc/uts/cgroup namespace isolation, read-only binds for runtime roots and snapshots, tmpfs scratch, `--unshare-net` for offline tasks, seccomp filter, `--die-with-parent`, `--new-session`, cleared environment, and cgroup v2 resource limits applied by clyded. Rootless Podman is **not** implemented.

**Rationale**
The stated goal is to reach Firecracker quickly. Bubblewrap gets the snapshot/task/artifact pipeline exercised end-to-end at very low startup cost and with no image-build pipeline, and it consumes the same nix-closure runtime roots the Firecracker rootfs is built from ([D6](#d6-runtime-roots-are-nix-closures-not-oci-images)). Building a Podman backend in between would be work thrown away — Podman's value here was the OCI image model, and with runtime roots defined as nix closures most of that value disappears.

**Consequences**
- Bubblewrap is explicitly a weaker boundary than the design calls for on untrusted project execution. It was acceptable as a scaffold; [D24](#d24-firecracker-is-the-default-backend-for-build-execution) makes the microVM the default for every build task, and retains this backend for hosts without KVM and for the warden's workspace environment.
- Host prerequisites are non-trivial on Ubuntu 24.04 (`kernel.apparmor_restrict_unprivileged_userns=1` blocks unprivileged user namespaces for unconfined binaries, which includes nix-store binaries). `clyde doctor` must detect this and explain the fix. See [host prerequisites](roadmap.md#host-prerequisites).
- Resource limits come from cgroup v2 delegation (`systemd-run --user --scope`) and are mandatory for T2 and above: without delegation, build tasks are refused rather than degraded ([D22](#d22-no-degraded-resource-limits-for-untrusted-execution)).

## D6: Runtime roots are nix closures, not OCI images

**Status:** accepted

**Decision**
Each task family's execution root is a nix derivation in the project flake, identified by its store path. Sandboxes bind the closure read-only. There is no OCI image build, registry, or digest-pinning pipeline. Named roots for the MVP: `runtimeRoots.workspace` (text and code manipulation tooling; **no** rustc/cargo/node/browser/signing tooling), `runtimeRoots.rust` (Rust toolchain for check/test/build), `runtimeRoots.fetch` (cargo plus network client tooling).

**Rationale**
The flake is already the source of truth for tooling. Reusing it for runtime roots gives reproducibility and content-pinning for free, keeps one definition of "the Rust toolchain we use", and removes upstream base-image trust from the supply chain. It also composes with Firecracker, where the guest rootfs is built from the same closure.

**Consequences**
- nix is required on any host that executes tasks, not just on developer machines. A deliberate narrowing of portability for the MVP.
- The Firecracker rootfs in Part 1b is an erofs image built from the same closure ([R12](#r12-the-guest-root-image-is-uncompressed-erofs)), so runtime-root identity is stable across backends.
- The hard requirement that the workspace runtime root must not contain the project build toolchain is now enforceable by inspecting a derivation, and is asserted in a test.

## D7: `registry-only` egress is enforced by a Clyde-managed proxy

**Status:** accepted

**Decision**
Network-bearing tasks are given a loopback-only network namespace plus a bind-mounted Unix socket. A trusted in-sandbox forwarder exposes `127.0.0.1:<port>` and bridges to the host-side Clyde egress proxy over that socket. The proxy terminates nothing: it accepts HTTP `CONNECT`, allowlists by destination host, and logs every attempt — allowed and denied — into the task's fetch manifest.

**Rationale**
Rootless network namespaces cannot create veth pairs, so host-side firewalling of a sandbox netns is unavailable without privilege. Socket-bridging a CONNECT proxy needs no privilege, works identically under bubblewrap and (over vsock) under Firecracker, and makes the allowlist decision host-side and trusted. It is what makes the approval prompt's "network: registry-only" claim true rather than aspirational.

**Consequences**
- Allowlisting is by CONNECT target host. Except for the `model-api` carve-out below, TLS is end-to-end, so there is no payload inspection and no content filtering. This limitation must be stated in the approval UX.
- Every egress-bearing task profile names an **egress profile**, and the set is closed. See [the egress model](tasks-and-policy.md#the-egress-model).
- The in-sandbox forwarder is trusted code running in an untrusted netns. Compromising it grants nothing beyond the allowlist the proxy already enforces.

### Amendment: selective TLS termination for `model-api` only
The proxy terminates TLS for hosts in the `model-api` profile, and **only** those hosts, so it can inject the model API credential host-side ([D11](../warden/decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)). The agent then never holds that credential.

Every other profile — `rust-registry` included — remains pure `CONNECT` pass-through with end-to-end TLS and no interception. Intercepting dependency traffic would put Clyde in the path of dependency *content* and undermine the content-hash story [D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift) depends on. This carve-out is narrow on purpose and must stay that way. Its mechanics and costs are in the [sandbox design](../warden/design.md#the-model-api-channel).

### Amendment: the guest channel is a single vsock, multiplexed by port
This decision, and the egress model, stated that under profile `none` a microVM guest has **no channel of any kind** to the host. [D24](#d24-firecracker-is-the-default-backend-for-build-execution) makes that unachievable, and the claim is weakened deliberately rather than silently.

With the microVM backend as the inner-loop default, every build task needs streamed logs and structured cargo diagnostics coming out of the guest. Firecracker's only console is an 8250 serial UART, slow enough that bulk output can stall the guest, and there is no virtio-console. The one remaining transport is vsock, and Firecracker permits **one** vsock device per VM. So every VM gets one, multiplexed by port: job control (always), log stream (always), and the egress bridge — for which the host binds a listener **only** when the profile permits egress.

The structural claim becomes: **the guest has no network device in any configuration, and no host listener exists on the egress port unless the profile permits egress.** That is weaker than "no channel at all" and it is still enforceable and testable. The `VmConfig` type keeps its property that no code path can express a guest network interface.

Not permitted: reusing one profile's VM for another profile's task. A vsock device cannot be removed after boot, so a VM booted with an egress listener must never serve a `none`-profile task. Backend pools and any future warm-start path partition by egress profile as well as by runtime root and size class.

## D8: Brokered `git.push` uses the developer's existing credential, inside the broker only

**Status:** provisional — revisit when multi-user or shared-runner support is added

**Decision**
`clyde-brokerd` uses the developer's existing SSH key or `ssh-agent` connection, held only in the broker process. No credential, socket, or token is ever exposed to an agent, a workspace environment, or a build sandbox. Every push is gated by a remote allowlist, a branch-pattern allowlist, and a per-push approval bound to the exact request.

**Rationale**
This proves the "brokered, not mounted" property immediately and with no provisioning friction. The credential's own scope is unchanged from today, which is honest: Clyde's contribution is eliminating exposure paths and adding approval and audit, not reducing the credential's inherent authority.

**Consequences**
- Push must not execute repository-controlled code. The workspace `.git/config` and `.git/hooks` are untrusted content, so the broker pushes from a **sanitized temporary repository**; see [`git.push`](tasks-and-policy.md#gitpush).
- Commit creation is a trusted clyded operation (`git.commit.prepare`), not a driver operation.

### Amendment: the broker's value is posture-dependent
The broker does three separable things, and [D23](#d23-the-mvp-splits-into-the-builder-and-the-warden) pulls them apart:

1. **Keeping credentials out of the build sandbox.** A property of the sandbox specification, true in every posture and for every driver.
2. **Binding a push to a reviewed diff and an approval digest.** Provenance and review discipline. True in every posture, including a human operator who owns the credential anyway.
3. **Withholding the credential from an untrusted driver.** Meaningless for a human operator with their own SSH key; fully live for CI or an external agent, and for a warden-hosted agent.

| | human operator | CI or external agent | hosted agent |
|---|---|---|---|
| credentials absent from the build sandbox | yes | yes | yes |
| push bound to a reviewed diff and approval digest | yes | yes | yes |
| credential withheld from the driver | no — they own it | yes | yes |

Since external drivers are a first-class builder posture, the broker earns its place on (1) and (2) alone, and gains (3) whenever the driver is not the credential's owner. Documentation and approval UX should say which of the three is doing the work in a given deployment rather than implying all three always are. All three cases are supported and none is a degraded mode.

## D9: The Firecracker backend lands as Part 1b, before dependency resolution

**Status:** accepted

**Decision**
The phase order is Part 1a (bubblewrap + snapshots + safe inner loop) → **Part 1b (Firecracker behind the same trait)** → Phase 3 (dependency resolution) → Phase 4 (broker). The microVM work is removed from its old late position.

**Rationale**
Phase 3 is the first phase to give untrusted execution any network reachability at all. Running that on the weaker boundary, and then reworking the egress plumbing for Firecracker afterwards, is the worse order in both security and effort terms. Building the vsock-based egress path once, against the intended backend, is cheaper.

**Consequences**
- Part 1b must reach parity for `rust.check`, `rust.test.unit`, and the workspace environment before Phase 3 begins.
- Task policy names a **required minimum isolation level**, and backend selection is policy-driven, not manual.
- Part 1a's bubblewrap backend is retained after 1b for low-risk task classes and for hosts without KVM.

**Superseding note.** [D24](#d24-firecracker-is-the-default-backend-for-build-execution) keeps this ordering and broadens the reason. The microVM backend is no longer the boundary for one network-bearing task; it is the default for every build task. The argument for building the egress plumbing once against the intended backend is unchanged and now applies to the whole inner loop.

## D10: MCP is the primary actor-facing API

**Status:** accepted — spans both products; the API is the builder's, two of its tools are the warden's

**Decision**
clyded exposes its actor-facing operations as MCP tools on `clyded.sock` from Phase 1. The internal command model is transport-agnostic; JSON-RPC is retained for the CLI and admin surface.

Tool set: `mission_status`, `list_capabilities`, `run_task`, `task_status`, `task_logs`, `list_artifacts`, `request_publish`, `commit_prepare` (builder); `request_escalation`, `request_subagent` (sandbox).

**Rationale**
It makes the pipeline drivable by an existing agent with no bespoke adapter, which is what makes the delegation workflow provable in Phase 1 rather than at the end of the roadmap. That is worth more after the split than before it: the builder ships without hosting anything, so this API is what a developer's existing agent or a CI job connects to — "point what you already run at this socket" rather than "run your agent inside our sandbox". The API is also not the only way in: a human operator drives the same tasks over the admin socket ([D25](#d25-task-execution-has-an-operator-surface-on-the-admin-socket)), which makes the pipeline testable with no client at all.

**Consequences**
- File reading and editing are **not** MCP tools. The actor uses its own filesystem tools against the paths its lease permits; where Clyde hosts it that scope is enforced by mount topology ([D1](../warden/decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)), and otherwise it is recorded in the closing diff rather than enforced.
- Tool descriptions are part of the security UX: each must state the policy consequences of the call — network, credentials, approval requirement. Vagueness there produces a driver that asks for the wrong things.
- The tool set splits across the two products. Everything but the two escalation tools serves any machine driver, which is what makes the builder usable without asking anyone to change how they work.

## D12: Documents are updated in place; specs and this log are added

**Status:** accepted

**Decision**
No document in `docs/` may contradict a decision recorded in a decision log. New documents cover what had no home: the decision logs, the schema reference, the egress model, the warden design, and the implementation specs.

## D13: CLI through Phases 1-3; TUI in Phase 4

**Status:** accepted

**Decision**
Phases 1-3 ship a complete CLI with human-readable and `--json` output. The ratatui TUI is a Phase 4 deliverable, once the mission, task, and approval surfaces have stabilised.

**Consequences** — the CLI is also the integration-test harness, so `--json` output stability matters from Phase 1.

## D14: Machine-readable policy in `.clyde/`, `AGENTS.md` advisory only

**Status:** accepted

**Decision**
`.clyde/policy.toml` holds machine-readable settings: allowed registries, subtree defaults, mission task pre-approvals, push remote and branch patterns, snapshot exclusions, synthetic service defaults. `AGENTS.md` is human/agent prose, passed into agent context and never parsed for authority. Precedence: built-in defaults → host config → user config → repo config. **Repo config may only narrow.** Any repo value that would widen authority relative to the layer above is rejected with a diagnostic, not silently clamped.

**Rationale**
Repository content is untrusted. A repo that can widen its own authority is a self-signed permission slip. Keeping the advisory channel textual and the authority channel narrow-only keeps that boundary legible. A silent clamp is worse than a rejection because it leaves the user believing something is configured that is not.

## D15: Three binaries from Phase 0

**Status:** accepted

**Decision**
A cargo workspace with three binaries — `clyde` (CLI), `clyded` (control plane), `clyde-brokerd` (broker) — plus library crates. `clyde-brokerd` exists from Phase 0 as a stub answering capability queries only; it gains real operations in Phase 4.

**Rationale**
The broker boundary is the one the design cares most about and the one where a retrofit is most likely to leak — shared process state, in-process credential access, an "internal" call path that skips audit. Establishing the process boundary while it is trivially cheap avoids that.

## D16: One active mission per workspace

**Status:** accepted

**Decision**
Multiple registered workspaces are supported. Each workspace has at most one active mission at a time, which may have sub-agent leases beneath it. The schema carries the identifiers needed to relax this later.

**Rationale**
Concurrent missions editing one live mutable tree raises edit-conflict and cache-partitioning questions the design has not answered. Per-mission cache scoping ([D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)) is also materially simpler under this constraint.

## D18: Build access is baselined, pinned in Clyde state, and escalated on drift

**Status:** accepted

**The threat this addresses.** Malicious code arriving in a transitive dependency and executing during compilation, via `build.rs` or a proc macro. This is the primary threat the build-task design is tuned for, and it is not addressed by snapshot *breadth* decisions alone — what matters is detecting **change**.

**Decision**
Build and test tasks run against a pinned **access baseline** with two components: a **repo read set** in two tiers (subtree grants for first-party project code, file pins for anything outside them), and a **code-execution inventory** — every dependency package with a `build.rs` or a proc-macro crate type, pinned by crate, version, **and source content hash**.

The baseline is authoritative in **Clyde's own state**, keyed by workspace, task type, and build target. It is never stored in the repository, because repository content is untrusted and an attacker who could edit the baseline could hide their own drift.

### The two tiers, and why
The threat is dependency code changing. Editing the project is not the threat — it is the work. Treating both at the same granularity would make an agent adding a source file indistinguishable from a build script reaching somewhere new, and a control that fires on ordinary editing gets clicked through.

**Subtree grants** are first-party project code and are not drift-sensitive: the subtrees the human approved in the mission envelope, plus the subtrees the static build closure requires within that approved scope. Creating, renaming, moving, or deleting files inside a granted subtree is ordinary work — no prompt, no drift, no amendment. The review happened once, up front, over a scope rather than a file list.

**File pins** are everything outside the granted subtrees and are drift-sensitive: an `include_str!` target in `docs/`, a fixture in a shared directory, a single sibling crate a build script reads. Each is admitted individually, confirmed by a human, and recorded. A read outside grants ∪ pins is drift.

**Absolute exclusions** are applied before grants and are not overridable by them: `.env*` and similar secret-shaped files, key material, `target/`, `node_modules/`, and `.git` by default. The authoritative list is in [tasks-and-policy.md](tasks-and-policy.md#repo-read-set-two-tiers).

This revises the earlier "file-level with directory rollup" granularity: rollup is still how pins are rendered when a whole out-of-scope directory is read, but first-party code is granted by subtree and is not file-pinned at all.

### Enforcement
Repo read enforcement is **by materialization**: the snapshot contains the granted subtrees minus exclusions plus the pinned files, and nothing else, so a read outside fails with `ENOENT`. No tracing, no privilege, identical behaviour on both backends. Because grants are subtrees, materialization picks up newly created first-party files automatically on the next snapshot — which is exactly what keeps the inner loop free of prompts.

Inventory enforcement is **pre-execution**: the inventory is computed from the lockfile and dependency bundle before the sandbox starts, so a newly-arrived or changed build script is caught *before it runs*.

### Establishing a baseline, drift, and escalation
The mechanics — static proposal, privileged learn mode, the drift classes, the observation mechanism, and the prompt shape — are in [tasks-and-policy.md](tasks-and-policy.md#access-baselines). In enforce mode, denial and drift are the same event, and Clyde renders it as an escalation naming the path rather than a raw cargo error. Per-open enforcement with exact denied-path reporting instead of `ENOENT` inference is deferred ([OQ5](#oq5-per-open-enforcement-of-the-build-snapshot)).

### Consequences
- Snapshot scope is no longer a policy question with a fixed answer; it is per-target recorded state.
- First-party development is prompt-free by construction. Churn cannot come from editing the project, only from reaching outside it or from dependency change.
- A task with no baseline for its target is refused, with the static proposal offered. Fail-closed, and no implicit wide-scope first run.
- Because the baseline lives only in Clyde state, it is invisible to code review, is not shared across a team or a fresh clone, and is lost with Clyde's state. Mitigations: the mission approval envelope summarises the baseline in force, and an export/import command is worth adding post-MVP.
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

## D21: `.git` is never available to build tasks

**Status:** accepted — resolves the former OQ2

**Decision**
`.git` is an absolute exclusion from build snapshots. It cannot be admitted by a subtree grant, by a file pin, or by configuration. There is no flag. Git metadata for builds that want it — commit sha, timestamp, describe output, dirty flag — is **not supported in the MVP**. If the need arises it will be added as a typed Clyde task computing the metadata in the trusted control plane and handing it to the build as an ordinary input, never by admitting the repository history.

**Rationale**
Repository history routinely contains secrets that were removed from HEAD. Handing `.git` to a build script therefore hands hostile dependency code every credential ever committed, including ones the developer believes they have deleted. That is materially worse than the working tree itself, and it is invisible to anyone reasoning only about what the current checkout contains. The alternatives — a pinnable full `.git`, or a shallow clone — both put git objects in front of untrusted execution to serve a small convenience. Deferring the convenience is cheaper than getting the sanitisation right.

**Consequences**
- A build that reads `.git` fails, and the failure must be diagnosed specifically as `GitMetadataUnavailable`: this is not drift, and git metadata is not yet supported. A raw cargo error here would send someone hunting for a bug that does not exist.
- `vergen`-style crates and anything shelling out to `git` in a `build.rs` will not work until the metadata task exists. A known, accepted MVP limitation that belongs in the user-facing docs.
- The exclusion list stays a single class: absolute, with nothing pinnable.

## D22: No degraded resource limits for untrusted execution

**Status:** accepted — resolves the former OQ4

**Decision**
Cgroup v2 limits are mandatory for T2 and above. On a host where cgroup v2 delegation is unavailable, build and test tasks are **refused**. There is no opt-in, no fallback, and no configuration permitting them to run on rlimits and a wall-clock timeout alone. The workspace environment (T0/T1) may still run with rlimits and a timeout, since it hosts a semi-trusted agent rather than hostile dependency code. `clyde doctor` reports missing delegation as a hard failure for build capability, not a warning.

**Rationale**
rlimits are a genuinely weaker bound, not an equivalent one: `RLIMIT_NPROC` is per-user rather than per-sandbox, and `RLIMIT_AS` is per-process, so a hostile build script can still exhaust the host's process table or memory collectively. Permitting that behind a flag means Clyde would sometimes run hostile code with weaker limits than its own documentation claims, and a flag that weakens a stated boundary tends to end up set.

**Consequences**
- Clyde will not run build tasks on hosts without systemd user delegation — some container environments and minimal distributions. A real portability cost, accepted deliberately.
- The refusal **narrows to hosts without KVM** once [D24](#d24-firecracker-is-the-default-backend-for-build-execution) lands: a microVM's memory and vCPU allocation bounds the workload by construction, with no cgroup delegation required. Where the microVM backend runs, this refusal does not apply. Where it cannot, the namespace backend is the only option and the refusal stands unchanged. Both statements hold simultaneously and `clyde doctor` must report whichever applies to the host in front of it.
- `clyde doctor` must distinguish "no cgroup v2", "cgroup v2 present but not delegated", and "delegated but missing controllers", because the remedies differ.

## D23: The MVP splits into the builder and the warden

**Status:** accepted

**Decision**
The MVP ships as two products with separate scopes and separate exit criteria. They were called **Part 1** and **Part 2** in earlier drafts and in commit history.

**The builder — the sandboxed build pipeline.** Snapshots, access baselines, the code-execution inventory, typed build tasks, artifacts, the egress proxy, the credential broker, and the audit chain. Driven by a human operator on the admin socket ([D25](#d25-task-execution-has-an-operator-surface-on-the-admin-socket)), or by any external machine driver — CI, or a coding agent the developer already runs — over the actor socket. It contains no notion of an agent process.

**The warden — the agent harness.** Hosting a coding agent inside a Clyde-managed workspace environment: the workspace runtime root, the `model-api` egress path, sub-agent derivation, and the mount-topology enforcement of edit scope. It closes the bypass the builder can only report ([D26](#d26-enforcement-posture-is-explicit-reported-and-recorded)).

Where each artefact belongs:

| | Builder | Warden |
|---|---|---|
| Tasks | `rust.check`, `rust.test.unit`, `rust.resolve-deps`, `git.commit.prepare`, `git.push` | `workspace.read`, `workspace.edit`, `repo.search` |
| Decisions | D2–D10, D12–D16, D18–D19, D21–D26 | D1, D11, D17, D20 |
| MCP tools | `run_task`, `task_status`, `task_logs`, `list_artifacts`, `mission_status`, `list_capabilities`, `commit_prepare`, `request_publish` | `request_subagent`, `request_escalation` |
| Specs | [roadmap.md](roadmap.md) — Phases 0, 1, 1a, 1b, 3, 4 | [spec.md](../warden/spec.md) |

[D10](#d10-mcp-is-the-primary-actor-facing-api) spans both: the actor API is the builder's and serves any driver, while two of its tools are the warden's. D23 itself is about the split and belongs to neither.

The mission and lease model, the two-socket split, the approval manager, and the audit chain are the builder's and remain driver-agnostic: they are an authority model over requests, not over a hosted process.

The three workspace task types stay in the closed catalog rather than being deleted — the catalog's exhaustiveness is a tested property and churning it buys nothing. In a builder-only deployment they are inert, with no policy path reaching them.

**Rationale**
The two halves defend against different attackers, and that is what makes them separable.

The build pipeline's attacker is the **dependency**. It is actively hostile, it is inside the workload, and the workload is a pure function of its inputs. That needs an *execution* boundary: strong, one-shot, no network, no credentials.

The warden's attacker is the **agent**. It is usually not hostile but careless, and occasionally prompt-injected into hostility. It is the driver rather than the workload. That needs an *authority* boundary: what it may ask for, what requires a human, what it holds.

Bundling them cost real design coherence. The workspace environment became the one sandbox needing a writable live tree and outbound network, which is exactly why it cannot move to the microVM backend and why [Risk 7](roadmap.md#risk-7-the-workspace-environments-model-api-egress-is-an-exfiltration-path) exists. It also made the most valuable part of the system untestable without first configuring the least essential part, because tasks were reachable only by a token holder.

Crucially, containing the agent is not load-bearing against the primary threat. A hostile `build.rs` does not care who requested the build. If it ran through the pipeline it ran in a microVM, against a read-only snapshot, with no network, no credentials, and a pre-execution inventory diff — whether or not the requester was contained.

**What the builder does not provide**
[D1](../warden/decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)'s rationale still holds and is not waived: a driver that can invoke `cargo` directly makes the typed pipeline advisory. The builder does not solve that; it *reports* it. The enforcement D1 described is the absence of the build toolchain from the driver's environment, which is a property of that environment and not necessarily one Clyde provides — a host with no Rust toolchain has it too. So the builder verifies and states the property, and the warden provides it ([D26](#d26-enforcement-posture-is-explicit-reported-and-recorded)).

The honest limit: a driver with network access can fetch a toolchain, so toolchain-absence is only durable when egress is also controlled, which it is not for an uncontained driver. **The builder alone is therefore advisory against a hostile driver and fully effective against a hostile dependency.** Both halves of that sentence belong in the user-facing documentation.

**Consequences**
- The builder keeps the MCP surface. It stops being "the agent's API" and becomes the machine-driver API, serving CI or an agent the developer already runs elsewhere. That makes the builder useful without asking anyone to change how they work.
- Brokered `git.push` carries different weight per posture; see the [D8 amendment](#amendment-the-brokers-value-is-posture-dependent).
- Edit-scope enforcement by mount topology is a sandbox property. Under the builder alone, out-of-scope edits are *detected* in the closing diff rather than prevented.
- Approvals in a human-driven builder deployment are self-confirmations; see the [D2 amendment](#amendment-confirmation-semantics-when-the-operator-is-the-driver).
- The library crates already match this split — no crate under `crates/` has any notion of an agent, and the sandbox spec builders for the workspace environment and for build tasks are already independent. The coupling is confined to the daemon.
- The warden is config-gated inside `clyded` rather than a fourth binary for now. The daemon already fails mission activation cleanly when no agent command is configured, so "builder only" is the absence of an `[agent]` section. Extraction into its own binary stays available and should be a deliberate decision when the warden gains its own roadmap, not a side effect.

## D24: Firecracker is the default backend for build execution

**Status:** accepted — supersedes [D5](#d5-bubblewrap-first-behind-a-sandboxbackend-trait) as the end state

**Decision**
Every build task runs on the microVM backend by default, not only `rust.resolve-deps`. Bubblewrap is retained for two cases: hosts without KVM, and the workspace environment.

The builder is therefore sequenced in two sub-parts: **Part 1a**, the operator-driven pipeline on the namespace backend, everything except `rust.resolve-deps`; and **Part 1b**, the same pipeline with the microVM backend as the default, plus `rust.resolve-deps`.

**Rationale**
With the builder standing alone and its driver possibly uncontained ([D23](#d23-the-mvp-splits-into-the-builder-and-the-warden)), the build boundary carries all of the weight. Bubblewrap was always described as the scaffold rather than the intended boundary, and D5 was provisional for exactly this revisit. Reserving the microVM for the one network-bearing task leaves the inner loop — where hostile dependency code actually executes, many times per mission — on the weaker boundary indefinitely.

Sequencing 1a before 1b is not a hedge. It puts the microVM work in front of a pipeline whose snapshot construction, closure computation, baselines, inventory diffing, classification, and artifact handling have already been debugged, instead of debugging both at once through a VM boundary.

**Consequences**
- Inner-loop latency becomes a Part 1b exit criterion rather than a curiosity, since every iteration now pays VM cost.
- **The file surface is block devices only.** Firecracker's device model has no virtio-fs, no 9p, and no filesystem passthrough of any kind; the only path for host bytes into the guest is a block device. This is a deliberate property of Firecracker, not an oversight, and it is not configurable.
- That works because the expensive surface and the per-run surface differ. The mission cache becomes one image created at mission activation and attached read-write to every run, so `max_cache_bytes` is enforced by the filesystem's size rather than by accounting. Only the source snapshot is rebuilt per run, and `mke2fs -d` builds an image from a directory unprivileged, at a cost proportional to source size rather than to `target/`.
- **Artifact extraction needs an explicit mechanism.** Outputs written by the guest live inside an image the host cannot safely read while the guest runs. The options were `debugfs` extraction or a read-only `fuse2fs` mount after the guest stops, or shipping artifacts over vsock. Resolved by [R11](#r11-all-guest-output-leaves-over-vsock): everything leaves over vsock, and no host code reads a guest-written image.
- A job contract is required: the guest must learn its argv, environment, and working directory, and must return an exit status distinguishable from the VM's own. Drive identity must travel as a filesystem label or UUID, because guest device names are positional and `drive_id` is invisible to the guest.
- Log streaming forces a vsock device on every VM; see the [D7 amendment](#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port).
- [D22](#d22-no-degraded-resource-limits-for-untrusted-execution)'s refusal narrows to hosts without KVM.
- Learn-mode observation moves **into** the guest, using `fanotify`. That removes the awkward arrangement where the widest-scope run of the code being constrained happened on the weaker backend.
- `clyde doctor` reports KVM as a hard requirement for build capability in the default configuration, not as a `rust.resolve-deps` footnote.
- A warm pool via Firecracker snapshot/restore is the correct optimisation if measured latency demands one, and it is deliberately *not* part of 1b. It constrains the job contract, though: the pooled VM must be snapshotted with guest init already blocked on the job channel and with placeholder drives to be repointed per run. Design the contract so a pool remains possible; do not build the pool before the numbers justify it. Persistent VMs *reused across task runs* are refused outright — a VM that has executed a hostile build script and then serves the next run is the in-memory form of the cache-persistence vector [D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only) exists to close.

## D25: Task execution has an operator surface on the admin socket

**Status:** accepted

**Decision**
`task run`, `task status`, `task logs`, and `task list` gain an operator surface on the admin socket, bound to an approved mission and lease, with the human recorded as the acting principal. The actor surface is unchanged and still requires a session token. Audit records therefore carry the **kind** of principal that acted — human operator, external actor holding a token, or a hosted agent — rather than assuming a token-bearing actor.

**Rationale**
Tasks were reachable only by a token holder, so exercising the entire build pipeline required first configuring and hosting an agent. That made the most important part of the system depend on the least essential part of it. It is also what CI needs, and what a developer driving the pipeline by hand needs.

**Consequences**
- The human operator is identified by `SO_PEERCRED` on the admin socket, which is already how that socket authenticates.
- `clyde task run` no longer refuses when invoked by a human on the host. The refusal that matters — `clyde approve` inside a sandbox — is unaffected, because it protects a different property ([D2](#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).
- A mission still gates the work. An operator-driven task is admitted against the same lease, policy, budget, and baseline as an actor-driven one, and produces the same records. The surface differs; the admission path does not.
- Both sockets are retained regardless of posture. The split is not load-bearing for a human-only builder deployment, but it is for the warden and for external drivers, and retrofitting a channel split into a running protocol is the kind of change that goes wrong.

## D26: Enforcement posture is explicit, reported, and recorded

**Status:** accepted

**Decision**
clyded knows and reports an enforcement **posture** for each workspace:

- **`enforcing`** — the driver cannot reach a project build toolchain outside Clyde. Either the warden hosts the agent in a workspace environment whose runtime root has no toolchain, or the host itself has none. Typed tasks are the only path to project execution.
- **`advisory`** — a driver can bypass the pipeline. Clyde states specifically how: a toolchain on `PATH`, an uncontained driver, or both.

`clyde doctor` reports the posture. Every task run records the posture it ran under. Mission review states the posture the mission's work happened under.

**Rationale**
Splitting a security product into the strong half and the half that closes the bypass creates a standing temptation to ship the first and describe it as though the second existed. The builder with an uncontained driver is a large, genuine improvement — hostile dependency code in a microVM with no network and no credentials, rather than running as the developer's user with their SSH keys — and it is *not* what D1 promised. The gap is only safe if it is impossible to be in the weaker mode without being told.

This is the same discipline as [D22](#d22-no-degraded-resource-limits-for-untrusted-execution)'s refusal, applied to a boundary deliberately made optional rather than mandatory.

**Consequences**
- `clyde doctor` gains a check for a reachable project build toolchain on the host, reported as the bypass it is rather than as a warning about tooling hygiene.
- Posture is a field on the task run and appears in mission review. It is derived state, never configuration: there is no setting that makes an advisory deployment report as enforcing.
- The user-facing documentation states the bypass in the README rather than in a footnote.
- Posture affects **reporting only**, never admission. Nothing about detecting an advisory posture may widen or narrow what a host is permitted to run — the same constraint [R10](#r10-a-remedy-that-cannot-work-is-a-defect-not-a-nicety) places on enclosure detection, and worth a test for the same reason.

## D27: Snapshot materialisation preserves change ordering in mtime

**Status:** accepted

**Decision**
Materialising a snapshot must guarantee, for every path, that the materialised file's mtime is newer than the previous run's materialisation of that path **if and only if** the path's content changed since that run. Concretely:

- a path whose content digest is unchanged from the mission's previous snapshot is materialised by hardlink, sharing the content store blob's inode and therefore its mtime
- a build with **no** previous snapshot hardlinks everything: there are no outputs for these mtimes to be compared against, so nothing has to look newer than anything, and stamping them now would let an untouched file's mtime move *backwards* on the next run when the hardlink path takes over
- a path whose digest **differs in either direction** — edited, or reverted to content the store already holds — is materialised by copy, with mtime set explicitly to the materialisation time
- the copy fallback taken when hardlinking is unavailable sets mtime explicitly, and never leaves a materialised file carrying a blob's ingest time by accident
- reflink materialisation, if it is added, carries the same obligation: `FICLONE` does not copy timestamps either

`MaterialisationKind` already records which strategy a snapshot used. A strategy that cannot honour this contract is a preflight failure, not a slower path.

**Rationale**
Cargo decides freshness for local sources by comparing source mtimes against build output mtimes, and has no content-hash freshness mode on the stable toolchain. Content-addressed materialisation breaks the assumption cargo relies on, in both directions, and both failures are silent.

**Correctness.** Reverting a file — `git checkout -- <path>`, or returning to a branch built earlier in the same mission — re-links a blob ingested earlier, carrying an mtime *older* than the existing build output. Cargo considers the crate fresh, skips work it needed to do, and the task result describes a tree that was not built. A stale pass is the worst outcome this pipeline can produce, because everything downstream treats a pass as evidence about a specific tree — the task evidence attached to a push approval most of all.

**Performance.** `std::fs::copy` copies permission bits and not timestamps, so the copy fallback stamps every file with the materialisation time and cargo rebuilds everything on every run. That is not "hardlinks, but slower"; it is the difference between an incremental inner loop and a cold one on every iteration, and it would present as an unexplained constant factor rather than as a fault.

Both follow from the same missing property, so both are closed by stating it once.

**Consequences**
- Materialisation needs the previous snapshot manifest for the same mission and target, which the snapshot store already holds. The comparison is per-path digest equality; nothing new is computed and nothing new is stored.
- The copy path costs one copy per *changed* file, so it is proportional to what was actually edited and is negligible against the rest of a build.
- Two tests, both correctness rather than performance assertions: a fixture that edits a file, runs, reverts it, and runs again must **rebuild**, and must never report a pass for the reverted tree; and a run forced onto the copy strategy must produce the same rebuild decisions as the hardlink strategy for the same edits.
- `clyde doctor` already probes for hardlink support on the state directory, which is the filesystem that matters: materialisation links from the blob store to the snapshot tree, and both live there. What changes is the remedy's claim. "Slower but correct" was false before this decision, because the copy path re-stamped every file and cargo rebuilt everything; it is true once the copy carries the blob's mtime.
- This is the first decision that owns snapshot materialisation semantics. [D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only) governs what the cache holds and how long it lives; this governs what the *inputs* look like to the tool reading them, which is the other half of whether an incremental build is correct.
- Cargo's unstable `-Z checksum-freshness` addresses the same problem upstream by hashing local sources rather than stat-ing them. It is not a substitute: it is nightly-only, and the flake-pinned stable toolchain is the supported version ([Phase 0](roadmap.md#1-nix-flake)). Worth revisiting if it stabilises, at which point this decision becomes defence in depth rather than the mechanism.

## D28: Dependency build output stays in the per-mission cache for the MVP

**Status:** provisional — revisit when [Part 1b](roadmap.md#part-1b-the-microvm-backend-as-the-default) reports measured latency

**Decision**
[D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only) is unchanged for the MVP. One writable cache per mission holds both first-party and dependency compilation output and is destroyed at closeout. Dependency build output is **not** promoted to a shared, input-addressed artifact store, and no MVP work is sequenced on the assumption that it will be.

The revisit point is named rather than open-ended: Part 1b measures latency as an exit criterion, and a shared dependency cache is weighed then, against numbers, if cold-build cost turns out to be a real problem rather than an anticipated one.

**Rationale**
The cost D3 accepts is one cold build per mission. For a workspace with a few hundred dependencies that is minutes, and it recurs at every mission boundary — so it is plausibly the dominant term in a day's work rather than the footnote D3 treats it as. Dependency compilation is also the overwhelming majority of that cost, and it is a pure function of inputs Clyde has already pinned: crate source hash from the content-addressed bundle, rustc identity from the runtime root's store path, target, features, and flags. An input-addressed dependency artifact store is a natural fit rather than a stretch.

It is deferred regardless, for two reasons.

**It is an optimisation with no measurement behind it.** Nothing in the MVP has yet run against a real repository on the intended backend. Building a second cache tier now would size a solution to an estimate, and it would land in the same window as the microVM backend, whose own latency profile is unmeasured. Characterise first, optimise if the characterisation demands it.

**Its security argument needs care, and care is cheaper once the pipeline works.** D3's per-mission scoping closes the cache-persistence vector bluntly, and a shared tier reopens part of it. The analysis is recorded here so that it does not have to be re-derived at the revisit:

- A hostile `build.rs` in crate X, keyed by X's own source hash, can only poison X's own entry — which it could do in any mission regardless, since it *is* X. Reusing that entry recompiles to the same hostile output either way, so cross-mission persistence adds little on its own.
- The vector that genuinely matters is a build script escaping its `OUT_DIR` to clobber a *different* crate's artifacts in a shared target directory. That is real, and it is why a shared `target/` cannot simply be carried across missions.
- It is avoidable rather than inherent. If Clyde extracts entries per crate from a per-run scratch target directory and re-materialises them read-only, rather than letting cargo write into the shared store directly, then the store is written by trusted code and each entry is addressed by the inputs that produced it. That is roughly what `sccache` does, and it is real machinery rather than a configuration change.

So the shape of the future decision is already known. What is missing is evidence that it is worth building.

**Consequences**
- Part 1b's latency measurement must report **cold-build cost at a mission boundary** as well as warm inner-loop cost. Only the second is currently named as an exit criterion, and it is the first that decides this question.
- The mission cache stays one writable surface, so nothing in Parts 1a or 1b needs to distinguish first-party from dependency build output. A future split would need that distinction, so the cache layout should not actively prevent it — separate directories under the mission cache are sufficient and cost nothing now.
- The UX obligation D3 already carries — surfacing the cold build as such — matters more while this stays unresolved. A user who understands why the first build in a mission is slow will tolerate it; one who does not reads it as the system being slow.
- If measurement shows cold-build cost is acceptable, this decision becomes accepted as it stands, and the analysis above is the record of why the alternative was not built.

## Implementation refinements

These arose while implementing Phases 0-4. Each is a narrowing or a clarification of a decision above rather than a reversal; where one changes what a decision said, it says so.

### R1: The actor token binds a connection, and is re-resolved on every request

**Refines** [D2](#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel).

D2 says every actor-facing request carries the capability token. MCP has no per-request header, and threading a token through every tool call's arguments would put it where tool schemas and client logs can see it.

So the token is presented once, in `initialize`, and binds the connection. The property D2 exists for is preserved by re-resolving token → session → lease → mission on **every** request rather than caching the resolution: an expired or revoked lease stops working immediately rather than at the next connection. The socket is bind-mounted only into the sandbox the token belongs to, so the connection and the token identify the same actor either way.

What this gives up: a stolen connection is as good as a stolen token for as long as it stays open. Since the connection is a Unix socket inside one sandbox, an attacker who can hold it already has that sandbox.

### R2: `clyde-git` is a crate

**Extends** the [crate layout](roadmap.md#3-crate-layout).

Phase 4 requires exactly one place in the codebase that constructs a git command. Both clyded (diff, commit preparation) and clyde-brokerd (push) need it, and they are separate processes, so it is a crate rather than a module: `crates/clyde-git`.

Its sanitisation is applied by construction — there is no constructor producing an unsanitised invocation — and the one value a caller may influence is the ssh command, which is how the broker supplies a credential.

### R3: Learn-mode observation uses the `inotify` crate

**Implements** the observation mechanism in [D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift).

Host-side `inotify` on the materialised tree needs syscalls. The `inotify` crate is a thin safe wrapper, which keeps the workspace free of `unsafe` (forbidden at the workspace level) without a `pre_exec` hook or hand-rolled fd handling.

The observation reports how many directories it could not watch, so an incomplete observation — a tree past the host's watch limit — cannot be mistaken for a complete one.

### R4: Resource limits are applied by wrapping the command

**Implements** the limits in [D5](#d5-bubblewrap-first-behind-a-sandboxbackend-trait) and [D22](#d22-no-degraded-resource-limits-for-untrusted-execution).

Limits are applied by wrapping the sandbox command in `systemd-run --user --scope` and `prlimit` rather than by setting rlimits in a `pre_exec` hook. That keeps the whole mechanism at the argv level: it contains no `unsafe`, and the exact command Clyde runs is a value a test can assert on and an audit record can carry.

`RLIMIT_NPROC` is deliberately not set. It is per-user, so it would bound the developer's whole login session rather than the sandbox, and would not bound the sandbox at all if other processes were already running. Process count is bounded by `TasksMax` in the cgroup, which is per-scope.

### R5: The seccomp filter is a deny list, and is passed on the child's stdin

**Implements** the seccomp filter in [Part 1a](roadmap.md#part-1a-snapshots-and-the-namespace-backend).

`bwrap --seccomp FD` reads a filter from an inherited descriptor. Passing it as the child's stdin keeps this in safe Rust: `Stdio::from(File)` places the file on descriptor 0 with no fd manipulation. A sandboxed task has no use for stdin.

The policy is a deny list over an allow-by-default base. An allow-list is the stronger shape, but a build sandbox runs cargo, rustc, a linker, and arbitrary `build.rs` code, whose syscall surface is wide and toolchain-dependent. An allow-list tight enough to be worth having would break builds on the next toolchain bump, and a filter that gets disabled to make builds work is worth nothing. The deny list closes the escape and privilege-manipulation calls that namespace isolation cares about; the namespace and cgroup boundaries remain the primary control.

### R6: A test-only backend exists, and never ships

**Extends** [D5](#d5-bubblewrap-first-behind-a-sandboxbackend-trait).

Integration tests need to exercise the pipeline — admission, snapshot, execution, classification, artifacts, audit — on hosts that cannot create unprivileged user namespaces, which includes containers and a default Ubuntu 24.04 install.

A backend with no isolation at all lives behind the `test-backend` cargo feature, which no shipped binary enables, and reports `BackendKind::TestOnly` so a run on it is identifiable in the audit record. The daemon's own registry construction never adds it; a test must inject it explicitly.

Everything asserted with it is a statement about the pipeline. The isolation boundary is asserted separately and structurally, over every sandbox specification the system can generate, which is a check that runs on every host.

### R7: A remote is approved by name; the broker resolves the URL

**Refines** [D8](#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only).

The push request digest covers the remote *name*, refspec, commit, and tree, but not the resolved URL. A human approves a remote name, and the URL comes from the broker's own configuration — never from the workspace repository, whose configuration is attacker-controlled content in this threat model.

So a configuration change to a remote's URL is not an approval mismatch, while a different remote is. That is the intended reading of "approved for one remote".

### R8: Replay protection is the brokered operation's state

**Refines** [`git.push`](tasks-and-policy.md#gitpush) validation.

The broker must refuse an operation whose approval is already consumed. Consumption and execution cannot be transactional across two processes, and the correct ordering is consume-then-push: a push that ran under a consumed approval is recoverable, while one that ran under an unconsumed approval is a second push waiting to happen. That ordering means the broker would always see the approval as consumed.

So the broker verifies a fact it can check for itself: clyded moves the brokered operation to `executing` immediately before calling, a terminal operation cannot re-enter that state, and the broker refuses anything not in it. Single-use consumption remains the control plane's bookkeeping; the operation state is the replay protection the broker enforces independently.

### R9: `clyde doctor` works without a daemon

**Extends** [`clyde doctor`](roadmap.md#11-clyde-doctor).

Bring-up is exactly when the daemon is not running, so `clyde doctor` falls back to probing the host directly when it cannot reach the admin socket. A diagnostic that requires the thing it is diagnosing is no diagnostic at all.

**The fallback loads configuration itself**, from the same host and user files `clyded` would read. It did not at first, and the result was a probe that reported every configured path as absent — telling an operator whose runtime roots were correctly configured to configure them, which is [R10](#r10-a-remedy-that-cannot-work-is-a-defect-not-a-nicety)'s failure exactly. Two consequences: the host-file discovery and layering live in `clyde-policy` rather than in the daemon, since two binaries need the same answer; and the report names its own source, because several rows depend on configuration and "not configured" and "not visible from here" are different claims that looked identical.

### R10: A remedy that cannot work is a defect, not a nicety

**Extends** [R9](#r9-clyde-doctor-works-without-a-daemon).

`clyde doctor` reported two prerequisites correctly and diagnosed both causes wrongly when run inside a container: an unwritable `cgroup.subtree_control` was attributed to a missing `Delegate=yes` when the real cause was a read-only `/sys/fs/cgroup` and no service manager at all, and an absent `/dev/kvm` was attributed to hardware that in fact reported `vmx`. Both remedies were actionable-looking and impossible, which is worse than silence — they get followed.

So the probes observe the enclosure (container, no service manager, or host) and the CPU's virtualisation extensions, and select the remedy from them. Two constraints:

- **The enclosure changes the remedy, never the verdict.** `can_run_build` and `strongest_isolation` read capability variants only, so nothing about detecting a container can widen what a host is permitted to run — D22's refusal in particular. A test asserts the two remedies differ while the detail and the admission decision stay identical.
- **Observation is separated from classification.** The cases worth diagnosing are the ones the test machine cannot reproduce, so `CgroupObservation` and `KvmObservation` are plain values and the classifiers are pure functions over them.

### R11: All guest output leaves over vsock

**Implements** the extraction mechanism [D24](#d24-firecracker-is-the-default-backend-for-build-execution) requires to be chosen before the guest init is written.

Logs, structured cargo diagnostics, the learn-mode read set, and task artifacts all leave a microVM guest over the vsock multiplex. There is no `debugfs` or `fuse2fs` path in the pipeline, and no host code reads a guest-written image.

The argument for `debugfs` was that it invents no protocol. That argument is spent: the [D7 amendment](#amendment-the-guest-channel-is-a-single-vsock-multiplexed-by-port) already puts a job-control port and a log port on every VM, so the protocol is written either way, and extraction would be a second output path with a different shape and its own failure modes. Three things follow from having only one.

- **A crashed guest still yields its output.** Everything streamed before the failure is already on the host. Extraction would have to recover it from an ext4 image whose last writes never landed, at exactly the moment an operator most needs the log.
- **No per-run writable output image has to exist.** Artifacts here are task stdout and stderr, cargo's JSON diagnostics, the observed read set, and the failure classification — small, typed, and already bounded by `max_artifact_bytes`. Compilation output stays in the mission cache image, which persists across runs within a mission and is never extracted ([D3](#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)). Creating a writable image per run *so that* the host can read it back is machinery in service of the weaker option.
- **The read-write drive rule keeps no exceptions.** A drive attached read-write to a running guest is never touched from the host, and there is now no post-stop case to reason about either.

The cost is that the host end of the stream is a trust boundary. A hostile guest can flood the channel, so the host enforces the artifact budget as it reads and truncates exactly as the namespace backend already does, rather than trusting a length the guest declares.

`e2fsprogs` remains a Part 1b prerequisite: `mke2fs -d` builds the source and mission-cache images. `debugfs` stays useful to a human inspecting a mission cache after a run. Neither is on the task path.

### R12: The guest root image is uncompressed erofs

**Refines** the guest images in [Part 1b](roadmap.md#1-guest-images).

The read-only root image for each runtime root is erofs, uncompressed. The guest kernel comes from the same flake, so `CONFIG_EROFS_FS` is a line of kernel configuration rather than a host dependency.

**Erofs rather than squashfs**, because the access pattern decides it: the root is a nix closure read randomly at every `exec` and every dynamic link, and block-compressed squashfs answers a page fault by decompressing a whole block. Erofs is built for random reads of an immutable tree, which is what a closure is.

**Uncompressed rather than compressed**, because every run in a mission reads the same image, so it belongs in the host page cache as mapped pages rather than as blocks decompressed per miss. The cost is disk — the rust root is the large one — paid against a latency budget that is a [Part 1b exit criterion](roadmap.md#part-1b-exit-criteria). Compression is an option to measure later, not a default to start from.

The image derivation is a pure function of the closure, so image identity follows closure identity and [D6](#d6-runtime-roots-are-nix-closures-not-oci-images)'s identity claim holds across both backends.

What would reverse this: a guest kernel or an `mkfs.erofs` that cannot produce a byte-reproducible image for a fixed closure. Squashfs is then the fallback at the same interface — one derivation and one kernel configuration line — and the reversal belongs here rather than in a commit message.

## Cross-cutting consequences

**Snapshot scope is baselined, not merely closure-derived.** Cargo cannot build a subtree in isolation, and its static closure routinely exceeds a lease's edit scope. Read-only admission of those paths is not an increase in authority — the write set remains the lease's edit paths, so the delta is confidentiality only. But breadth is still exfiltration surface for hostile build code, so the closure is the *starting point* for a baseline rather than the answer: the pinned baseline is what a task actually receives, and it is confirmed by a human ([D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

**Audit chain.** `audit_events` is append-only with a monotonic sequence number and a `prev_hash` chain, giving tamper-evidence cheaply. See [schema.md](schema.md#audit-events).

**Structured task failure classification.** The fetch escalation flow depends on distinguishing "compile failed because the code is wrong" from "compile failed because dependencies are not present". Task results therefore carry a structured failure classification from Part 1a, not just an exit code.

## Open questions

Neither blocks work. Each records a proposed resolution, which is a starting point for discussion, not a default.

### OQ5: Per-open enforcement of the build snapshot

Whether to serve build snapshots through a Clyde-owned file-serving daemon, giving per-open enforcement and exact denied-path reporting instead of inferring denials from `ENOENT`. It costs build performance and is a substantial component. **Proposed:** defer past the MVP; use materialisation first, and revisit if `ENOENT`-based diagnostics prove too weak in practice.

**Reframed by [D24](#d24-firecracker-is-the-default-backend-for-build-execution).** The virtio-fs route this question originally assumed does not exist: Firecracker has no filesystem passthrough, so on the default backend the snapshot arrives as a block device. Two consequences:

- The learn-mode half of this question is **resolved without it**. Guest-side `fanotify` sees every read, so observation on the microVM backend does not need a file-serving daemon.
- The enforcement half becomes harder, not easier. Per-open enforcement under Firecracker would need either a `vhost-user-blk` backend serving a synthesised filesystem image from the content store — which gains block-level rather than file-level observation — or a bespoke file protocol over vsock. Both are substantially more work than a FUSE mount, which strengthens the case for deferring. Enforcement by materialisation remains the mechanism, and it is backend-independent.

[OQ3](../warden/decisions.md#oq3-exec-logging-shim-in-the-workspace-environment) belongs to the warden.

### Resolved
- **OQ1** — superseded by [D18](#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift): snapshot scope is a pinned baseline seeded from the build closure, not a fixed policy.
- **OQ2** — resolved by [D21](#d21-git-is-never-available-to-build-tasks): `.git` is absolutely excluded; git metadata is deferred to a future typed task.
- **OQ4** — resolved by [D22](#d22-no-degraded-resource-limits-for-untrusted-execution): no degraded limits for T2 and above; build tasks are refused on hosts without cgroup v2 delegation.
