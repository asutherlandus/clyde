![CLYDE](clyde_banner.png)

# CLYDE

Clyde is being redesigned as a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

This branch carries the clean-slate architecture, its design document set, and the
implemented MVP.

In the simplest terms: **an actor works on a mission in a workspace, asks to run a task, policy decides whether and how it may run, and the task runs in an appropriate environment.**

## Status

The MVP slice — Phases 0 through 4 — is implemented. The design and
implementation-planning document set is in `docs/`; the binding decisions are in
[docs/decisions.md](docs/decisions.md), with the refinements that came out of
implementing them recorded in the same file.

What exists:

- `clyde` — the CLI and terminal client, operator and actor surfaces
- `clyded` — the control plane: two sockets, missions, leases, sessions, the task
  pipeline, access baselines, the egress proxy, and agent hosting
- `clyde-brokerd` — the credential broker
- `clyde-forward` — the in-sandbox egress forwarder
- bubblewrap and Firecracker backends behind one `SandboxBackend` trait

### What has and has not been exercised

Stated plainly, because a green test run is easy to over-read.

**Exercised end to end:** mission proposal, approval, lease issuance, session
binding, task admission, snapshot construction, a real `cargo check`, failure
classification, artifacts, the hash-chained audit trail, mission review, and a
brokered push against a repository whose hooks and configuration are hostile.

**Asserted structurally rather than by execution:** the isolation boundary. This
development host cannot create unprivileged user namespaces (the Ubuntu 24.04
AppArmor case `clyde doctor` diagnoses), has no cgroup v2 delegation, and has no
KVM. So sandbox properties are asserted over every specification the system can
generate — no credential path reachable, no CA certificate in a build sandbox, no
`.git`, no live workspace bind, no admin or broker socket, only the mission cache
writable — and the bubblewrap argv, seccomp filter, and Firecracker VM
configuration are asserted as values. Running them needs a host that can provide
the boundary.

**Implemented but not run anywhere yet:** the Firecracker backend and
`rust.resolve-deps`. Refusing them on a host without KVM is the intended
behaviour rather than a gap (D9), but three guest-side pieces are also missing —
no image build, no way for a task's argv to reach the guest, and no vsock in the
forwarder — so dependency resolution stays refused even on a host with KVM.
[INSTALL.md step 8](INSTALL.md#step-8-firecracker) says exactly what is absent.

## Quick start

```sh
nix develop                       # the only supported development entry point
cargo nextest run --all-features  # 662 tests
nix flake check                   # includes the runtime-root assertion

cargo run -p clyde -- doctor      # what this host can and cannot do
```

`clyde doctor` works without a running daemon: bring-up is exactly when the
daemon is not running.

The design direction assumes:
- untrusted code may execute during build, test, install, and codegen
- credentials must be **brokered, not mounted**
- network should be **denied by default**
- build, test, sign, and publish must be **separate environments with different authority**
- humans and agents act as **actors** within **missions**
- work happens through **tasks** evaluated by **policy** and run in controlled **environments**
- the workspace environment supports low-authority code-manipulation scripts, and project build/test toolchains stay outside it
- Clyde hosts the coding agent inside that workspace environment, so typed tasks are the only path to project execution

## Host setup

[INSTALL.md](INSTALL.md) has the full procedure. Three prerequisites decide what a
host can run, and `clyde doctor` is the authority on whether it has them — it
works without a running daemon, distinguishes *blocked* from *unavailable*, and
picks its remedy from what encloses it, so a container boundary is reported as
one rather than as a session misconfiguration (R10).

- **User namespaces.** Ubuntu 23.10+ ships
  `kernel.apparmor_restrict_unprivileged_userns=1`, which denies unprivileged
  userns to any binary without a permitting AppArmor profile — and a nix-store
  `bwrap` has none. This is the most likely thing to block a first run.
  [Fix](INSTALL.md#step-3-user-namespaces-and-apparmor).
- **cgroup v2 delegation.** Build and test tasks are refused outright without it,
  with no degraded mode (D22). Usually already present in a systemd login
  session. [Fix](INSTALL.md#step-4-cgroup-v2-delegation).
- **KVM**, for `rust.resolve-deps` only — and supplying it is not currently
  enough, for the reasons in
  [step 8](INSTALL.md#step-8-firecracker).

## Clyde Next document set

Start here:

1. [Problem Statement and Threat Model](docs/problem-statement-threat-model.md)
2. [Terminology](docs/terminology.md)
3. [Requirements](docs/requirements.md)
4. [Competitive Alternatives](docs/competitive-alternatives.md)
5. [High-Level Design](docs/high-level-design.md)

Detailed design:

- [Mission and Capability Lease Model](docs/mission-lease-model.md)
- [Task Policy Matrix](docs/task-policy-matrix.md)
- [Sequence Flows and Interaction Scenarios](docs/sequence-flows.md)
- [Component Architecture](docs/component-architecture.md)
- [Technology and Library Choices](docs/technology-choices.md)
- [MVP Implementation Roadmap](docs/mvp-implementation-roadmap.md)

Setting it up:

- [INSTALL.md](INSTALL.md) — host setup for both sandbox backends

Implementation decisions and specs:

- [Implementation Decision Log](docs/decisions.md)
- [Schema Reference](docs/schema-reference.md)
- [Agent Integration and the Workspace Environment](docs/agent-and-workspace-environment.md)
- [Network Egress Model](docs/network-egress-model.md)
- Phase specs: [0](docs/phase-0-foundations.md) · [1](docs/phase-1-mission-lease-approval.md) · [2](docs/phase-2-execution-and-isolation.md) · [3](docs/phase-3-dependency-resolution.md) · [4](docs/phase-4-credential-broker.md)

## Design summary

Clyde Next is intended to be:

- a **trusted local or self-hosted control plane**
- a **secure execution system** for hostile build/test/install workflows
- a **developer-facing workspace** for humans and coding agents
- a **credential broker** for git, signing, and publish operations
- an **artifact and audit system** connecting all trust boundaries

Core design ideas:

- **missions** define bounded goals
- **actors** work within mission limits
- the **workspace** is mutable and used for authoring
- **tasks** are the units of work Clyde controls
- **policy** decides whether and how tasks may run
- **environments** separate editing, research, build/test execution, and privileged external actions
- **leases**, **snapshots**, and **brokers** enforce those boundaries

## The MVP slice

Phases 0-4 target one strong end-to-end workflow:

- a mission delegated to an agent hosted in a Clyde-managed workspace environment
- snapshot-based `rust.check` and `rust.test.unit` with no network and no credentials
- bubblewrap isolation first, Firecracker microVMs before any network-bearing task
- approval-gated, registry-only dependency resolution through a Clyde egress proxy
- brokered `git.push` with no credential reachable from any agent or build sandbox

Full-stack, browser testing, signing, and publishing follow the MVP.

## Working on Clyde

[AGENTS.md](AGENTS.md) carries the coding standards, error-handling rules, and
security requirements, and the rule that the Nix flake is the source of truth for
tooling. [docs/decisions.md](docs/decisions.md) records what has been decided and
why; cite the identifiers (`D7`, `R2`) rather than restating the rationale, and
if a decision looks wrong, say so rather than quietly implementing something
else.
