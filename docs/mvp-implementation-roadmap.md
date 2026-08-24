# Clyde Next: MVP Implementation Roadmap

## Purpose

This document is the phased build order for Clyde Next. It translates the architecture, mission/lease model, task policy matrix, and sequence flows into a sequence of shippable slices.

Per-phase implementation detail lives in dedicated specs; this document is the map and the rationale for the ordering.

This document builds on:
- [component-architecture.md](component-architecture.md)
- [mission-lease-model.md](mission-lease-model.md)
- [task-policy-matrix.md](task-policy-matrix.md)
- [sequence-flows.md](sequence-flows.md)
- [decisions.md](decisions.md)

## Phase specs

| Phase | Spec | Summary |
|---|---|---|
| 0 | [phase-0-foundations.md](phase-0-foundations.md) | flake, CI, crate layout, typed schemas, pure policy functions, `clyde doctor` |
| 1 | [phase-1-mission-lease-approval.md](phase-1-mission-lease-approval.md) | missions, leases, tokens, approvals, agent hosting, egress proxy, CLI |
| 2a | [phase-2-execution-and-isolation.md](phase-2-execution-and-isolation.md#phase-2a-snapshots-and-the-namespace-sandbox) | snapshots, namespace sandbox, `rust.check`, `rust.test.unit`, per-mission cache |
| 2b | [phase-2-execution-and-isolation.md](phase-2-execution-and-isolation.md#phase-2b-microvm-backend) | Firecracker backend behind the same trait, policy-driven selection |
| 3 | [phase-3-dependency-resolution.md](phase-3-dependency-resolution.md) | `rust.resolve-deps`, dependency bundles, registry-only egress, escalation flow |
| 4 | [phase-4-credential-broker.md](phase-4-credential-broker.md) | broker gateway, brokered `git.push`, hostile-repo hardening, TUI, mission review |

Phases 5 and beyond are sketched at the end of this document and have no specs yet.

## Roadmap Principles

- preserve the core trust boundaries from the beginning
- make the safe inner loop usable as early as possible
- separate execution from authority before adding advanced features
- start with a narrow task catalog and deepen it incrementally
- optimize for one strong end-to-end workflow before broad platform coverage
- prefer explicit policy even when initial policy is simple
- never ship a network-bearing task on a boundary weaker than intended

## What the MVP Must Prove

1. A human can delegate a bounded mission to an agent.
2. The agent can autonomously perform edit / check / test loops within a lease.
3. Untrusted build and test execution happen in isolated sandboxes against immutable snapshots.
4. Dependency fetch is separated from compile/test.
5. Credentials are not mounted into agent or build environments.
6. Git push happens through a brokered path.
7. Boundary crossings trigger approvals instead of silently inheriting privilege.
8. The task and audit history remain visible enough for a human to trust the system.

## Proposed MVP Scope

### Primary workflow
- Rust project setup
- `AGENTS.md` repo guidance support (advisory prose, [D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only))
- one human
- one primary coding agent, hosted inside a Clyde-managed workspace environment ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment))
- mission creation and one active lease per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace))
- repeated editing, `rust.check`, and `rust.test.unit`
- separate dependency resolution
- brokered `git.push`
- visible approval prompts on a channel no agent can reach ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel))

### Out-of-scope for MVP
- release publishing and signing
- multi-agent orchestration beyond one level of sub-agent
- broad language ecosystem coverage
- enterprise policy federation
- remote runner fleet
- browser and synthetic-service testing

## Phase Summaries

## Phase 0: Foundations

**Goal** — establish tooling, structure, and the typed data model before any runtime behaviour.

**Key deliverables** — the Nix flake including runtime root derivations; CI wired to the flake; the three-binary crate layout; typed entities and state machines for every schema in [schema-reference.md](schema-reference.md); policy resolution, lease derivation, egress ordering, and budget charging as pure tested functions; the closed MVP task catalog; layered configuration with narrow-only repository config; the append-only hash-chained audit store; `clyde doctor`.

**Exit criteria** — see [Phase 0 exit criteria](phase-0-foundations.md#exit-criteria).

## Phase 1: Mission, lease, and approval core

**Goal** — bounded delegation, with the agent hosted inside Clyde and no project code execution at all.

**Key deliverables** — clyded with separate actor and admin sockets; mission lifecycle; lease issuance, derivation, renewal, revocation; session capability tokens; the workspace-environment sandbox; the egress proxy under the `model-api` profile; MCP server and agent hosting; approval manager with digest-bound approvals; the CLI; the full audit event set; diff-based edit auditing.

**Security value** — establishes the human-to-agent delegation boundary, and removes any reliance on ambient authority. Because the agent's environment contains no project build toolchain, no untrusted project execution is possible in this phase by construction.

**Exit criteria** — see [Phase 1 exit criteria](phase-1-mission-lease-approval.md#exit-criteria).

## Phase 2a: Snapshot execution and the safe inner loop

**Goal** — make autonomous edit / check / test loops practical and isolated.

**Key deliverables** — the `SandboxBackend` trait; the bubblewrap backend with cgroup v2 limits; the snapshot manager with build-closure computation; the **access baseline** — static proposal, human confirmation, enforcement by materialisation, privileged learn mode, drift escalation ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)); the code-execution inventory with pre-execution checking; dependency bundle import; the per-mission cache; `rust.check` and `rust.test.unit`; structured failure classification; the task execution pipeline with log and artifact capture; task CLI and MCP surface.

**Security value** — this is where Clyde replaces "the agent runs project code in its own environment" with snapshot-based, network-free, credential-free execution.

**Exit criteria** — see [Phase 2a exit criteria](phase-2-execution-and-isolation.md#2a).

## Phase 2b: MicroVM backend

**Goal** — upgrade untrusted execution to the intended isolation boundary before any task gains network reachability.

**Key deliverables** — the Firecracker backend against the same `SandboxSpec`; guest rootfs built from the same nix closure; vsock-bridged egress with no guest network device; policy-driven backend selection with no silent downgrade; parity and measured inner-loop latency.

**Why here and not later** — Phase 3 is the first phase to give untrusted execution any reachability at all. Shipping that on the namespace backend and then reworking the egress plumbing for Firecracker afterwards is worse on both security and effort ([D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)).

**Exit criteria** — see [Phase 2b exit criteria](phase-2-execution-and-isolation.md#2b).

## Phase 3: Separate dependency resolution

**Goal** — enforce the fetch-versus-compile split.

**Key deliverables** — `rust.resolve-deps` with a manifests-and-lockfile-only input snapshot; the dependency bundle artifact; lockfile change classification and the **code-execution inventory diff** in the approval prompt; the `rust-registry` egress profile with full attempt logging; the escalation flow driven by the `MissingDependencies` classification; the offline rerun path, gated on inventory confirmation.

**Security value** — removes any legitimate reason for a compile step to have network access, which is the core supply-chain property for Rust.

**Exit criteria** — see [Phase 3 exit criteria](phase-3-dependency-resolution.md#exit-criteria).

## Phase 4: Credential broker and brokered git push

**Goal** — separate code execution from authority, and complete the MVP.

**Key deliverables** — the broker gateway; `clyde-brokerd` with real operations and independent approval verification; `git.commit.prepare` as a trusted operation; `git.push` with remote and branch allowlists; hostile-repository hardening via a sanitised temporary repository; the push approval prompt; the TUI; the mission review surface.

**Security value** — removes the largest remaining ambient-power risk in typical agentic workflows, and closes the loop where a compromised repository could otherwise pivot through a credentialed git invocation.

**Exit criteria** — see [Phase 4 exit criteria](phase-4-credential-broker.md#exit-criteria).

## MVP Detailed Slice

The true MVP stops after Phase 4:

- one human developer
- one agent, hosted in a Clyde-managed workspace environment
- Rust project setup, with `AGENTS.md` guidance support
- mission creation and one primary lease
- scoped editing enforced by mount topology
- immutable, build-closure-aware snapshots for build/test tasks
- `rust.check` and `rust.test.unit` on microVM isolation
- `rust.resolve-deps` with approval-gated registry-only egress
- brokered `git.push`
- task logs, egress records, and a mission review surface

## Post-MVP Phases

These follow the MVP and have no specs yet.

### Phase 5: Frontend and full-stack extension
`node.resolve-deps`, `web.build`, synthetic service support, `browser.test.synthetic`, isolated browser profiles.

### Phase 6: Sub-agent and derived lease depth
Multi-level derivation, parent-child lease graph views, derived budget accounting, revocation fan-out at depth. One level of sub-agent derivation already exists from Phase 1.

### Phase 7: Publishing, signing, and provenance
`artifact.sign`, `artifact.publish`, release plan review, artifact lineage, in-toto/SLSA-style provenance.

### Phase 8: Broader deployment
Postgres-backed shared control plane, remote runners, editor integrations, org policy overlays.

## Sequencing Rationale

### Why mission/lease first?
Without it, agent autonomy falls back to implicit session authority and later hardening becomes messy.

### Why host the agent in Phase 1 rather than later?
Because the boundary is a property of the agent's environment. An agent on the host can bypass Clyde entirely, which would make Phase 2a's isolation work unverifiable in practice ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)).

### Why snapshots and check/test next?
Because the inner loop must be usable or the system will be rejected by developers.

### Why the microVM backend before dependency fetch?
Because dependency fetch is the first task with any network reachability, and the egress plumbing should be built once against the intended backend.

### Why dependency fetch before publish features?
Because separating fetch from compile is the core security property for Rust and full-stack ecosystems.

### Why git push broker before release publishing?
Because push is a frequent developer workflow and a common privilege exposure path.

## Engineering Workstreams

### Workstream A: Control plane and schemas
mission manager, lease manager, task request model, audit schema, session tokens

### Workstream B: Workspace and UX
CLI, mission and task views, approval UX, mission review, TUI in Phase 4

### Workstream C: Execution
snapshot manager, sandbox backends, cache management, log streaming, egress proxy

### Workstream D: Broker
broker gateway, git push broker, sanitised git invocation helper, approval linkage

### Workstream E: Policy and testing
task policy resolver, lease validation, mission defaults, threat-model tests

## Testing Strategy

### Unit tests
mission state transitions; lease derivation rules including every denial case; budget exhaustion; approval decision logic and digest binding; policy resolution; egress profile ordering including incomparable pairs.

### Integration tests
snapshot correctness and build-closure computation across project layouts; no-network enforcement; dependency fetch escalation and offline rerun; brokered push; revocation while tasks are pending or running; mission closeout and cache teardown.

### Security tests
These are assertions about the system's claims, and each should fail loudly if the claim stops holding:

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

## Risks and Mitigations

### Risk 1: inner-loop latency is too high
Mitigation: per-mission warm cache ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)); hardlink and reflink snapshot materialisation; measured latency as a Phase 2b exit criterion rather than an afterthought; small initial task catalog.

### Risk 2: UX becomes too approval-heavy
Mitigation: mission-level pre-approval for safe inner-loop tasks; approvals only at boundary crossings; narrow prompts that state what changed and why the safer profile failed.

### Risk 3: escape hatch becomes the default
Mitigation: `shell.untrusted` is deliberately absent from the MVP catalog; the workspace environment simply lacks the build toolchain, so ad hoc project execution is not available to route around typed tasks; escape-hatch usage, if added later, is logged and reviewed.

### Risk 4: complexity overwhelms implementation
Mitigation: ship one strong Rust workflow first; keep interface contracts separate from backend sophistication; defer multi-agent, publishing, and enterprise features.

### Risk 5: host prerequisites block adoption
Mitigation: `clyde doctor` ships in Phase 0 with per-item remediation, including the Ubuntu 24.04 user namespace restriction; host requirements are documented in the Phase 0 spec rather than discovered during bring-up.

### Risk 6: baseline churn makes drift prompts routine
If baselines churn on ordinary development, drift prompts become noise, the human starts confirming reflexively, and the control loses its value for the thing it exists to catch — new dependency code.

Mitigation is structural rather than procedural: first-party project code is granted **by subtree**, so creating, renaming, or deleting files inside the mission's approved scope is not drift and produces no prompt at all ([D18](decisions.md#the-two-tiers-and-why)). Only two things prompt: reaching outside the approved scope, and dependency change. Path drift and dependency drift are also presented distinctly, since the first is low-signal and the second is the point.

Prompt frequency should still be measured against a real repository during Phase 2a rather than after. The target is that a full feature's worth of editing produces zero baseline prompts.

### Risk 7: the workspace environment's model-API egress is an exfiltration path
Mitigation: it is the only egress from that environment, it is allowlisted and logged, and untrusted dependency code never runs there. The residual risk — a misbehaving or prompt-injected agent — is stated plainly rather than papered over, and is not claimed to be solved in the MVP ([D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy)).

## Suggested Initial Milestones

1. Flake, CI, crate layout, typed schemas, pure policy functions, `clyde doctor`
2. Mission and lease creation, agent hosted in the workspace environment, approvals on a separate channel
3. Snapshot-based `rust.check` and `rust.test.unit` with a warm per-mission cache
4. The same loop on the microVM backend, with measured latency
5. `rust.resolve-deps` with approval-gated registry-only egress and offline rerun
6. Brokered `git.push`, TUI, and mission review

## Summary

The roadmap builds from **bounded autonomy** outward: first missions, leases, and a hosted agent; then isolated snapshot-based execution; then the intended isolation boundary; then dependency fetch separation; then credential brokerage. That order gives usable agentic coding early while protecting the trust boundaries that motivated the redesign.
