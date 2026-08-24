# Phase 0: Foundations

## Goal

Establish the toolchain, repository structure, and typed data model that Phases 1-4 build against — with no runtime behaviour.

Phase 0 is complete when the schemas are executable and checkable, CI enforces the project's standards, and every interface contract Phases 1-4 depend on has been written down.

Decisions in force: [D4](decisions.md#d4-phase-0-delivers-flake-ci-crate-layout-and-typed-schemas), [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), [D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only), [D15](decisions.md#d15-three-binaries-from-phase-0), [D16](decisions.md#d16-one-active-mission-per-workspace).

## Deliverables

### 1. Nix flake

The flake is the source of truth for all tooling ([AGENTS.md](../AGENTS.md#tooling-source-of-truth)). It must provide:

- a pinned Rust toolchain from a pinned `nixpkgs`, with `clippy` and `rustfmt` components. Use nixpkgs' Rust unless a specific version is needed that nixpkgs does not carry, in which case add `fenix`. Edition 2024. There is no separate MSRV policy: the flake-pinned toolchain *is* the supported version
- `cargo-nextest`, `cargo-deny`, `cargo-insta`
- `bubblewrap` (Phase 2a), `sqlite`, `git`, `jq`
- a `devShell` that is the only supported development entry point
- `checks` outputs mirroring each CI job, so `nix flake check` reproduces CI locally

It must also define the runtime roots ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)), even though nothing executes them until Phase 2a:

- `runtimeRoots.workspace` — text/code manipulation tooling, and demonstrably no project build toolchain
- `runtimeRoots.rust` — Rust toolchain for check/test
- `runtimeRoots.fetch` — cargo plus network client tooling (Phase 3)

Each runtime root exposes its store path and closure manifest so a task policy can reference it by content identity.

### 2. Runtime root assertion test

A test that inspects the `runtimeRoots.workspace` closure and fails if it contains `cargo`, `rustc`, `rustup`, `node`, `npm`, `pnpm`, `yarn`, a browser, `gpg`, `ssh`, `docker`, or `podman`.

This is the mechanical form of what the design documents state as a hard architectural requirement. Until it is a test, it is a hope.

### 3. Crate layout

```text
crates/
  clyde-core/        ids, entities, state machines, validation, error types
  clyde-policy/      built-in task policies, policy resolution, lease derivation,
                     egress profile ordering, approval requirements
  clyde-store/       repository traits + SQLite impl + in-memory impl, migrations
  clyde-snapshot/    content store, snapshot manifest, materialisation strategies
  clyde-sandbox/     SandboxBackend trait, cgroup/rlimit helpers  (backends: Phase 2)
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

Dependency direction is strictly one way: `core` ← `policy` ← everything else. `clyde-policy` must not depend on `clyde-store`, `clyde-sandbox`, or `tokio`, so policy decisions are pure and testable without I/O ([AGENTS.md](../AGENTS.md#api-and-module-design)).

`clyde-brokerd` exists from Phase 0 as a stub answering capability queries only ([D15](decisions.md#d15-three-binaries-from-phase-0)).

### 4. Entity types

Every entity in the [schema reference](schema-reference.md) — including `AccessBaseline`, `CodeExecInventory`, and `AccessDrift` ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)) — as Rust types with:

- typed newtype identifiers, never bare `String` at API boundaries
- `serde` with `deny_unknown_fields` on all config and request types
- validation on construction, returning typed errors
- state machines as explicit transition functions returning `Result`, not as mutable field assignment
- `Display` implementations that are redaction-safe

Credential-bearing types (once they exist in Phase 4) must not implement `Serialize` or `Debug` in a way that reveals content. Phase 0 establishes the pattern with a `Redacted<T>` wrapper.

### 5. Policy resolution and lease derivation

Pure functions, fully unit-tested in Phase 0 even though nothing calls them yet:

- `resolve_task_policy(task_type, repo_path, mission, lease, config) -> Result<ResolvedPolicy, PolicyReason>`
- `derive_lease(parent, request) -> Result<Lease, PolicyReason>` implementing all eight [derivation rules](schema-reference.md#derivation-rules-normative)
- `egress_profile_order(a, b) -> Option<Ordering>` implementing the [profile ordering](network-egress-model.md#profile-ordering), including the incomparable pairs
- `charge_budget(budget, usage, cost) -> Result<BudgetUsage, PolicyReason>`

These four functions are where the security model actually lives. They are the highest-value test targets in the project and should be tested against denial cases first.

### 6. MVP task catalog

The catalog is closed for the MVP and encoded as a Rust enum with a built-in policy per variant:

| Task | Trust | Environment | Min isolation | Input | Egress | Cache | Approval |
|---|---|---|---|---|---|---|---|
| `workspace.read` | T0 | workspace | in-process / workspace sandbox | live, lease-scoped | `none` | none | none |
| `workspace.edit` | T0/T1 | workspace | workspace sandbox | live, lease-scoped | `none` | none | none |
| `repo.search` | T0 | workspace | workspace sandbox | live, lease-scoped | `none` | none | none |
| `rust.check` | T2 | build | namespace sandbox → microVM | snapshot, baselined | `none` | mission-scoped | none, but baseline required |
| `rust.test.unit` | T2 | build | namespace sandbox → microVM | snapshot, baselined | `none` | mission-scoped | none, but baseline required |
| `rust.resolve-deps` | T3 | build | microVM | snapshot: manifests + lockfile | `rust-registry` | writes dep bundle | human |
| `git.commit.prepare` | T1 | control plane | in-process, sanitised git | live working tree | `none` | none | none |
| `git.push` | T4 | broker | broker | commit id + refspec | `broker` | none | human |

"Min isolation" is the *minimum* the policy demands; the sandbox manager may select something stronger. `rust.resolve-deps` requires microVM, which is why [Phase 2b precedes Phase 3](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution).

### 7. Configuration layering

Implement the loader and precedence rules ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)): built-in defaults → host config → user config → `.clyde/policy.toml`.

Repo config may only narrow. A repo value that would widen authority relative to the layer above is a **rejection with a diagnostic**, not a silent clamp — a silently clamped config leaves the user believing something is configured that is not. Each load records a `config_loads` audit event with the file digest and any rejected keys.

`AGENTS.md` is read as prose and passed to agent context. There is no code path that parses it for authority.

#### Keys repository config may never set
Some keys are rejected outright rather than narrowed, because narrowing is not a meaningful operation on them ([D20](decisions.md#d20-the-agent-command-is-host-or-user-configuration-never-repository-configuration)):

- `agent.command`, `agent.args`, `agent.env` — a repository that can choose what Clyde execs has arbitrary code execution in the workspace environment before any policy applies
- runtime root selection for any task type
- egress profile host lists (a repo may narrow the *set of allowed registries*, but cannot add a host)
- anything under `broker.*`

#### Configuration surface for Phase 0
The loader should cover, at minimum: `agent.*` (host/user only), `registries.*`, `egress.model_api_hosts` (host/user only), `snapshot.exclusions`, `mission.defaults.*`, `push.remotes`, `push.branch_patterns`, and `limits.*`.

### 8. Storage layer

SQLite schema, forward-only migrations, and the repository traits from the [persistence layout](schema-reference.md#persistence-layout), with an in-memory implementation used by the policy and mission tests.

The audit chain (`prev_hash`, monotonic `seq`, append-only API with no update or delete) is established here, because retrofitting tamper-evidence onto an existing log is meaningless.

### 9. CI

GitHub Actions, wired to the flake so CI and local development cannot diverge. Every job runs inside `nix develop` rather than using setup actions, so a green CI run means the flake is correct:

- `nix flake check`
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo nextest run`
- `cargo deny check` (advisories, licences, bans)
- a lint that fails on `unwrap()` / `expect()` outside `#[cfg(test)]` in the `crates/` tree, per [AGENTS.md](../AGENTS.md#production-code-must-not-panic)

### 10. Test fixture inventory

The security properties in Phases 1-4 are asserted against fixtures, and those fixtures are a Phase 0 deliverable because several phases share them. Under `tests/fixtures/`:

**Project layouts** — for build-closure computation:
- `single-crate` — no workspace
- `virtual-workspace` — root with members, no root package
- `inherited-deps` — members using `workspace = true` inheritance
- `nested-path-deps` — member depending on a sibling by path

**Access baseline behaviour:**
- `include-str-outside` — `include_str!` reaching outside the crate directory
- `buildrs-reads-repo` — `build.rs` reading a repo file outside its crate
- `secret-shaped-files` — `.env.local` inside a granted subtree, which must not be materialised
- `churn` — a fixture whose modules, tests, and files change between runs, asserting **zero** drift prompts

**Hostile execution:**
- `buildrs-hostile-write` — `build.rs` attempting writes outside scratch and cache
- `buildrs-egress` — `build.rs` attempting a network connection
- `buildrs-credential-hunt` — `build.rs` looking for `~/.ssh`, `~/.gnupg`, and cloud config

**Dependency drift:**
- `dep-gains-buildscript` — two lockfile states, the second adding a build script
- `dep-same-version-tampered` — identical version, different source content
- `missing-dep` — lockfile requiring an absent crate, for `MissingDependencies` classification

**Broker hardening:**
- `hostile-git` — `.git/hooks/pre-push` and `.git/config` with `url.*.insteadOf`, both of which must execute nothing

Each fixture asserts a named property, and the property is stated in the fixture's own README so a failing test explains itself.

### 11. `clyde doctor`

A host-prerequisite checker, shipped in Phase 0 because it is what makes Phase 2a's bring-up diagnosable rather than mysterious. See [host prerequisites](#host-prerequisites).

## Host prerequisites

Clyde is Linux-first and, per [D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images), requires nix on any host that executes tasks. `clyde doctor` checks each item below and prints the remediation rather than a bare failure.

| Requirement | Needed for | Notes |
|---|---|---|
| nix with flakes | everything | runtime roots are nix closures |
| unprivileged user namespaces | Phase 2a sandbox | see the Ubuntu note below |
| cgroup v2 with systemd user delegation | resource limits | **mandatory for build tasks**; no fallback ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)) |
| KVM (`/dev/kvm` accessible) | Phase 2b | microVM backend only |
| a filesystem supporting hardlinks | snapshots | reflink (btrfs/XFS) is used when available |

### The Ubuntu 24.04 user namespace restriction

Ubuntu 24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1`, which blocks unprivileged user-namespace creation for binaries without a permitting AppArmor profile. Distribution-packaged `bwrap` has such a profile; a nix-store `bwrap` does not, so the Phase 2a sandbox will fail on a default Ubuntu 24.04 host.

Three remediations, in preference order:

1. install an AppArmor profile for the nix-store `bwrap` path that grants `userns create` (narrowest, survives reboot, and the profile can be shipped in the repository)
2. `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` (broad — it re-enables unprivileged userns for everything on the host)
3. use the distribution's `bwrap` instead of the flake's (contradicts the flake-as-source-of-truth rule and should be a last resort)

`clyde doctor` must distinguish "userns unavailable" from "userns blocked by AppArmor", because the fixes are entirely different and the raw kernel error does not say which it is.

It must draw the same distinction for cgroups — "no cgroup v2", "cgroup v2 present but not delegated", "delegated but missing controllers" — and report missing delegation as a hard failure for build capability rather than a warning, since build tasks will be refused without it ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)).

### Known MVP limitations to report
`clyde doctor` should also state plainly what Clyde will not do, so it is discovered before a confusing build failure:
- git metadata is unavailable to build tasks, so `vergen`-style crates and `build.rs` scripts shelling out to `git` will fail ([D21](decisions.md#d21-git-is-never-available-to-build-tasks))
- build tasks require cgroup v2 delegation

## Exit criteria

- `nix develop` provides every tool the project uses; nothing depends on a host-global tool
- `nix flake check` and all CI jobs pass on a clean checkout
- every entity in the [schema reference](schema-reference.md) exists as a validated Rust type with state transitions as functions
- policy resolution, lease derivation, egress ordering, and budget charging are implemented as pure functions with denial-case tests
- the runtime root assertion test passes and would fail if a build toolchain were added to `runtimeRoots.workspace`
- the config loader rejects (not clamps) authority-widening repo config, with a test
- the audit store is append-only and hash-chained, with a test that detects truncation
- `clyde doctor` correctly reports the state of every host prerequisite, distinguishing AppArmor-blocked from unavailable user namespaces
- the fixture inventory exists, and each fixture states the property it asserts
- repository config setting `agent.command` is rejected with a diagnostic, with a test
- the MVP task catalog exists with a built-in policy per task type
- `clyde-brokerd` starts, answers a capability query, and holds no credentials

## Explicitly not in Phase 0

No daemon behaviour, no sandbox execution, no snapshots taken, no MCP server, no agent hosting, no network. Phase 0 produces types, pure functions, storage, tooling, and diagnostics.
