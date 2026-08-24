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
behaviour rather than a gap (D9) — but the backend also expects a guest kernel
and rootfs images that nothing in the flake builds, so dependency resolution
stays refused even on a host that has KVM. See [Host setup](#host-setup).

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

Clyde needs three things from the host, and `clyde doctor` is the source of truth
for whether it has them. Run it first; it works without a running daemon and it
distinguishes *unavailable* from *blocked*, because the remedies are different:

```sh
nix develop
cargo run -p clyde -- doctor
```

Each blocked or missing prerequisite prints a remedy. What follows is the
background for the three that actually come up.

### User namespaces, and the Ubuntu 24.04 AppArmor restriction

Bubblewrap needs to create an unprivileged user namespace. Ubuntu 23.10 and
later ship `kernel.apparmor_restrict_unprivileged_userns=1`, which denies that to
any binary without a permitting AppArmor profile — and a nix-store `bwrap` has
none. This is the single most likely thing to block a first run on a Ubuntu
desktop.

The narrow fix is a profile for the store path. Store paths are content-addressed
and change whenever the toolchain moves, so the profile needs a glob:

```
# /etc/apparmor.d/nix-bwrap
abi <abi/4.0>,
include <tunables/global>

profile nix-bwrap /nix/store/*/bin/bwrap flags=(unconfined) {
  userns,
  include if exists <local/nix-bwrap>
}
```

```sh
sudo apparmor_parser -r /etc/apparmor.d/nix-bwrap
```

It lives in `/etc`, so it survives a reboot. Check `ls /etc/apparmor.d/ | grep -iE
'bwrap|userns'` first — if your Ubuntu already ships a profile for the
distribution's `bwrap`, adapting that one for the nix path is lower-risk than the
sketch above.

The blunt alternative is `sudo sysctl -w
kernel.apparmor_restrict_unprivileged_userns=0`, persisted under `/etc/sysctl.d/`.
It re-enables unprivileged user namespaces for every binary on the machine, which
is precisely the attack surface the restriction exists to close. For a project
whose thesis is least privilege, prefer the profile — and note that even the
glob covers any `bwrap`-shaped path in the store.

Pin the binary the profile names, so a `PATH` change cannot silently select a
different one:

```toml
# host configuration
[sandbox]
bwrap = "/nix/store/…/bin/bwrap"
```

### cgroup v2 delegation

Build and test tasks (T2 and above) are refused outright without a delegated
cgroup v2 subtree. There is no degraded mode and no opt-in flag (D22), so this is
a hard prerequisite rather than a performance note.

On a systemd machine it usually works already: `user@$UID.service` is delegated by
default, which is what `systemd-run --user --scope` needs. Confirm with:

```sh
systemctl --user show user@$(id -u).service -p Delegate
test -w /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.subtree_control \
  && echo delegated
```

It breaks when `clyded` runs outside a login session — a system service, or `ssh`
without a session. `loginctl enable-linger $USER` is the fix.

### KVM and Firecracker

Only `rust.resolve-deps` needs these: it is the one task that executes with
network reachable, so it requires microVM isolation and is refused rather than
downgraded on a host that cannot provide it (D9). Everything else in the MVP runs
under bubblewrap.

```sh
ls -l /dev/kvm                      # present?
lsmod | grep kvm                    # kvm_intel / kvm_amd loaded?
sudo usermod -aG kvm $USER          # then re-login
```

If `/dev/kvm` is missing on a CPU that reports `vmx` or `svm`, `doctor` says so
explicitly — that is a device that was not exposed, not hardware that cannot
virtualise. Inside a VM, nested virtualisation has to be enabled on the
hypervisor.

**Not runnable yet, even with KVM.** The Firecracker backend expects a guest
kernel at `<state-dir>/vm/vmlinux` and rootfs images under `<state-dir>/vm/rootfs/`
built from the runtime-root closures, and nothing in the flake builds them.
`firecracker` is also not in the devShell. So dependency resolution stays refused
on any host until those images exist; the backend, its configuration, and the
policy that gates it are implemented and tested, but the images are the missing
piece. Adding them is the next real deliverable, not a configuration step.

### Containers

`clyded` is meant to run on the host. Inside a typical container it will report
three failures at once — no writable cgroupfs, no `/dev/kvm`, and an AppArmor
sysctl that belongs to the host kernel — and `doctor` will say so rather than
suggesting session changes that cannot take effect from in there.

### Verifying

```sh
cargo run -p clyde -- doctor
```

On a correctly configured host the summary lines flip to `YES` for the workspace
environment and for build and test tasks. That is the meaningful signal: it means
the daemon will admit a bubblewrap-backed task rather than refuse it on host
capability.

Running a task end to end additionally needs an agent, and the agent binary is
resolved from inside the workspace runtime root rather than from `PATH` (D6), so
it has to be added to `runtimeRoots.workspace` in `nix/runtime-roots.nix` and
named by `agent.command` in host configuration. Until then the operator surface —
register, propose, approve, review, audit, close — is what runs on a real host,
and the task pipeline is exercised by the test suite.


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
