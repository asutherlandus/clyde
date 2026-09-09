![CLYDE](clyde_banner.png)

# CLYDE

Clyde is being redesigned as a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

This branch carries the clean-slate architecture, its design document set, and the
implemented MVP.

In the simplest terms: **an actor works on a mission in a workspace, asks to run a task, policy decides whether and how it may run, and the task runs in an appropriate environment.**

## The two products

The MVP is planned as two products ([D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)):

**The builder — the sandboxed build pipeline.** Immutable snapshots, access baselines, the
dependency code-execution inventory, typed build tasks, the fetch/compile split,
brokered `git.push`, and a hash-chained audit trail. Driven by a human at a
terminal, by CI, or by a coding agent the developer already runs. It has no notion
of an agent process.

**The warden — the agent harness.** Hosting a coding agent inside a Clyde-managed
workspace environment, with the model-API egress path and sub-agent derivation.

They separate because they defend against different attackers. The builder's is the
**dependency**: actively hostile, inside the workload, and needing an *execution*
boundary. The warden's is the **agent**: usually careless rather than hostile, the
driver rather than the workload, and needing an *authority* boundary. Containing
the agent is not what protects you from a hostile `build.rs` — that code does not
care who requested the build.

**The builder alone cannot stop a driver from running `cargo` itself**, outside Clyde
entirely. That is real, and Clyde reports it rather than leaving it implied:
`clyde doctor` names the posture, every task run records it, and mission review
states it ([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).
What the builder does guarantee is that whatever *is* run through it executes in a
microVM against a read-only snapshot, with no network and no credentials.

## Status

The MVP slice — Phases 0 through 4 — is implemented, and predates the
builder / warden split. The document set in `docs/` has been reorganised around the split;
the code has not yet. The binding decisions are in
[docs/builder/decisions.md](docs/builder/decisions.md), with the refinements that came out of
implementing them recorded in the same file.

Built since that slice, and recorded as decisions: the **operator task surface**
([D25](docs/builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)),
**posture** reporting ([D26](docs/builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)),
the materialisation mtime contract ([D27](docs/builder/decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)),
and the **microVM backend's guest side** — a flake-built kernel and erofs root
images, a versioned job contract, the vsock guest channel, and block-image
construction ([D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution),
[R11](docs/builder/decisions.md#r11-all-guest-output-leaves-over-vsock),
[R12](docs/builder/decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)).
The microVM is opt-in per run rather than the default: an operator raises a task
into a guest with `--isolation microvm`, and the vsock egress bridge, guest-side
learn mode, and the raised policy floor are still to come.

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

**Run once inside a real boundary:** `rust.check` against a dependency-free
project, on a host with unprivileged user namespaces, cgroup v2 delegation, and a
user session bus — `cargo check` under bubblewrap, against a read-only snapshot
at `/work`, offline, exiting clean. That first run found four bugs in the spawn
path, every one invisible to the test suite because nothing in it had ever
executed a sandbox: the wrapper's environment was cleared past what
`systemd-run --user` needs, `argv[0]` was a leftover placeholder, the runtime
root's closure was never bound, and the path handed to `execvp` was a GC-root
symlink no sandbox binds. See the [changelog](CHANGELOG.md).

**Asserted structurally rather than by execution:** everything else about the
boundary. Sandbox properties are asserted over every specification the system can
generate — no credential path reachable, no CA certificate in a build sandbox, no
`.git`, no live workspace bind, no admin or broker socket, only the mission cache
writable — and the bubblewrap argv, seccomp filter, and Firecracker VM
configuration are asserted as values. One passing run is not the same as those
properties holding, and the gap that hid four bugs is still open: there is no
integration test that spawns a sandbox. That is the next thing worth building.

**Implemented, and never booted:** the microVM backend's guest side. The guest
kernel and root images build from the flake, images are built by real `mke2fs`
invocations in the test suite, and the whole guest channel — hello, job, log
streaming, report — runs end to end against a fake guest over the same sockets
Firecracker uses. What no test reaches is Firecracker itself and the init running
under a real kernel, which is exactly the gap that hid four bugs on the namespace
backend. [The bring-up guide](docs/builder/microvm-bring-up.md) is the walkthrough
and says what to read when a guest does not boot.

**Refused rather than degraded:** `rust.resolve-deps`, because the vsock egress
bridge its profile needs is unbuilt. A task that would run without the egress it
was promised is refused at preflight.

## Quick start

```sh
nix develop                       # the only supported development entry point
cargo nextest run --all-features  # 734 tests
nix flake check                   # includes the runtime-root assertion

cargo run -p clyde -- doctor      # what this host can and cannot do
```

`clyde doctor` works without a running daemon: bring-up is exactly when the
daemon is not running.

For the full path from here to a sandboxed `cargo check` under an approved
mission — configuration, runtime roots, a dependency bundle, an access baseline,
the first task, review — see
[docs/builder/quick-start.md](docs/builder/quick-start.md).

The design direction assumes:
- untrusted code may execute during build, test, install, and codegen
- credentials must be **brokered, not mounted**
- network should be **denied by default**
- build, test, sign, and publish must be **separate environments with different authority**
- untrusted project execution belongs in a **microVM by default**, on every inner-loop iteration and not only when a task touches the network
- humans and agents act as **actors** within **missions**
- work happens through **tasks** evaluated by **policy** and run in controlled **environments**
- the workspace environment supports low-authority code-manipulation scripts, and project build/test toolchains stay outside it
- where Clyde hosts the coding agent inside that workspace environment, typed tasks are the only path to project execution — and where it does not, Clyde says so rather than implying otherwise

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
- **cgroup v2 delegation.** On the namespace backend, build and test tasks are
  refused outright without it, with no degraded mode (D22). Usually already
  present in a systemd login session.
  [Fix](INSTALL.md#step-4-cgroup-v2-delegation).
- **KVM**, today for `rust.resolve-deps` and, once
  [D24](docs/builder/decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)
  lands, for every build task by default. Supplying it is not currently enough,
  for the reasons in [step 8](INSTALL.md#step-8-firecracker).

## Clyde Next document set

[docs/README.md](docs/README.md) is the index. The set is partitioned by product.

Shared:

1. [Threat Model](docs/threat-model.md) — the problem, the attacker, and which product answers which vector
2. [Terminology](docs/terminology.md) — the shared vocabulary
3. [Competitive Alternatives](docs/competitive-alternatives.md) — and the gap they leave

[docs/builder/](docs/builder/) — the build pipeline:

- [design.md](docs/builder/design.md) — goal, requirements, architecture, components, technology stack, interaction flows
- [mission-and-lease.md](docs/builder/mission-and-lease.md) — the authority model
- [tasks-and-policy.md](docs/builder/tasks-and-policy.md) — task catalog, trust and runtime classes, access baselines, the egress model
- [schema.md](docs/builder/schema.md) — entities, state machines, invariants, persistence
- [decisions.md](docs/builder/decisions.md) — the binding decisions; read [D23](docs/builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) first
- [roadmap.md](docs/builder/roadmap.md) — phase order and the specs for [0](docs/builder/roadmap.md#phase-0-foundations) · [1](docs/builder/roadmap.md#phase-1-mission-lease-and-approval-core) · [1a](docs/builder/roadmap.md#part-1a-snapshots-and-the-namespace-backend) · [1b](docs/builder/roadmap.md#part-1b-the-microvm-backend-as-the-default) · [3](docs/builder/roadmap.md#phase-3-separate-dependency-resolution) · [4](docs/builder/roadmap.md#phase-4-credential-broker-and-brokered-git-push)

[docs/warden/](docs/warden/) — the agent harness:

- [design.md](docs/warden/design.md) — the workspace environment, editing authority, the actor API, the model-API channel, sub-agents
- [decisions.md](docs/warden/decisions.md) — D1, D11, D17, D20
- [spec.md](docs/warden/spec.md) — deliverables, security properties, exit criteria

Setting it up:

- [INSTALL.md](INSTALL.md) — host setup for both sandbox backends

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

### The builder — the build pipeline
- a mission a human approves, and a lease usable from the CLI or by any driver holding a token
- snapshot-based `rust.check` and `rust.test.unit` with no network and no credentials
- access baselines, so first-party editing is prompt-free and dependency change is not
- bubblewrap isolation during bring-up, then Firecracker microVMs as the default for every build task
- confirmation-gated, registry-only dependency resolution through a Clyde egress proxy
- brokered `git.push` with no credential reachable from any build sandbox
- task logs, egress records, posture, and a mission review surface

### The warden — the agent harness
- that mission delegated to an agent hosted in a Clyde-managed workspace environment
- edit scope enforced by mount topology rather than detected in a diff
- the model API reachable without the agent ever holding the credential
- one level of sub-agent derivation
- `enforcing` posture

Full-stack, browser testing, signing, and publishing follow the MVP.
[docs/builder/roadmap.md](docs/builder/roadmap.md) is the map.

## Working on Clyde

[AGENTS.md](AGENTS.md) carries the coding standards, error-handling rules, and
security requirements, and the rule that the Nix flake is the source of truth for
tooling. [docs/builder/decisions.md](docs/builder/decisions.md) records what has been decided and
why; cite the identifiers (`D7`, `R2`) rather than restating the rationale, and
if a decision looks wrong, say so rather than quietly implementing something
else.
