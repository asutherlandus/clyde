# Clyde Next: Technology and Library Choices

## Purpose

This document proposes concrete technology and library choices for implementing Clyde Next.

It turns the architecture and security model into an opinionated implementation stack for an MVP, while also identifying where the design should evolve later as stronger isolation, broader platform coverage, and release-management features are added.

This document builds on:
- [terminology.md](terminology.md)
- [component-architecture.md](component-architecture.md)
- [mvp-implementation-roadmap.md](mvp-implementation-roadmap.md)
- [task-policy-matrix.md](task-policy-matrix.md)
- [mission-lease-model.md](mission-lease-model.md)

> **Decision status.** The choices in this document have been narrowed to binding decisions in [decisions.md](decisions.md). Where earlier drafts recommended rootless Podman and pinned OCI images, those are superseded by a bubblewrap-then-Firecracker backend path and nix-closure runtime roots. The sections below reflect the current decisions and cite the relevant identifiers.

## Decision Drivers

The technology choices should optimize for these priorities, in order:

1. **Strong security boundaries**
2. **Linux-first practicality**
3. **Low-latency inner-loop execution**
4. **Simple, testable local architecture**
5. **Clear typed APIs between subsystems**
6. **Room to upgrade isolation backends later**
7. **Good observability and debuggability**

Secondary goals:
- portability to remote/self-hosted deployments later
- support for richer IDE integrations later
- ability to add enterprise policy and audit features later

## High-Level Recommended Stack

Using the terminology from [terminology.md](terminology.md), this document is mostly about how Clyde implements:
- the **control plane** that evaluates policy and coordinates work
- the **workspace environment** for low-authority editing support
- the **build environment** for project execution
- the **broker environment** for privileged external actions

| Subsystem | Recommended MVP choice | Later evolution |
|---|---|---|
| Control plane daemon | **Rust** | Rust remains primary |
| CLI/TUI | **Rust** | add editor/web frontends later |
| Agent integration API | **MCP over Unix domain socket** (JSON-RPC retained for CLI/admin) ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api)) | add gRPC for remote deployment later |
| Config / policy format | **TOML for static config + Rust enums/structs for built-in task policy** | maybe Cedar/OPA-like policy layer later |
| Mission / lease / audit metadata store | **SQLite** | SQLite first, optional Postgres later |
| Snapshot storage | **content-addressed filesystem store + tar/zstd bundles** | add overlayfs/reflink optimizations later |
| Sandbox backend | **bubblewrap namespace sandbox (Phase 2a), Firecracker microVM (Phase 2b)** ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)) | Kata or gVisor only if Firecracker proves impractical |
| Runtime roots | **nix closures per task family, no OCI images** ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)) | same closures build the Firecracker rootfs |
| Network egress control | **Clyde CONNECT proxy, socket-bridged into a loopback-only netns** ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)) | vsock bridge under Firecracker |
| Actor authentication | **per-session capability tokens, separate admin socket** ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)) | OS-user separation for multi-user hosts later |
| Task process orchestration | **Tokio-based async task supervisor** | same foundation |
| Artifact store | **local filesystem blob store + SQLite metadata** | optional S3/OCI/CAS later |
| Structured logs | **JSON logs + tracing crate** | OpenTelemetry exporter later |
| Credential broker | **separate Rust local daemon over Unix socket** | split brokers, HSM/Vault integration later |
| Git operations | **git CLI initially, libgit2 optional later** | brokered higher-level git service |
| Signing | **broker wrapper around gpg/ssh-key/sigstore tooling** | native sigstore/HSM integrations later |
| Browser isolation | **Playwright in isolated sandbox** | dedicated browser runner |
| Synthetic services | **docker/podman containers or sandbox-side service launcher** | richer synthetic environment controller later |

## Recommended Core Implementation Language: Rust

## Choice
Use **Rust** as the primary implementation language for Clyde Next.

## Why Rust
Rust is the best fit because Clyde is primarily a:
- local control plane
- security-sensitive orchestrator
- policy engine
- concurrent task supervisor
- typed API surface
- artifact and audit manager

Rust is a strong fit for:
- long-running daemons
- precise data modeling
- async orchestration
- CLI/TUI applications
- strong compile-time guarantees around policy and state transitions
- packaging a single static-ish binary for Linux-first deployment

## Why not Bash
Bash is a poor fit for:
- mission/lease state machines
- structured policy enforcement
- robust audit trails
- concurrent sandbox orchestration
- Unix socket APIs
- long-term maintainability of a control plane

Bash may still be used for:
- helper scripts
- environment probing
- transitional wrapper commands

## Why not TypeScript/Node as primary
TypeScript would be attractive for rapid prototyping and UI integration, but it is weaker for:
- secure low-level process orchestration
- durable local daemon behavior
- system integration around namespaces, cgroups, mount setup, and broker boundaries

It remains a reasonable choice for future editor/UI adapters, but not the best control-plane core.

## Why not Go as primary
Go is a reasonable alternative and would also be a strong candidate. Rust is preferred because:
- richer type modeling for policy/state invariants
- stronger memory-safety without GC pauses
- stronger fit if future work includes more local binary tooling and security-sensitive parsing

If team familiarity strongly favors Go, Go could still be a viable fallback, but the recommendation is Rust.

## Recommended Rust Crates and Libraries

These are suggested, not final lock-ins.

### Core application
- `tokio` for async runtime
- `anyhow` and `thiserror` for error handling
- `serde`, `serde_json`, `toml` for config and API serialization
- `clap` for CLI parsing
- `tracing`, `tracing-subscriber` for structured logs
- `uuid` for mission/task/lease identifiers
- `time` or `chrono` for timestamps and lease expiry handling

### Data and persistence
- `rusqlite` or `sqlx` with SQLite backend
- `sha2` / `blake3` for content-addressing and artifact hashing
- `zstd` for snapshot/artifact compression
- `walkdir` and `ignore` for scoped filesystem traversal

### IPC and local APIs
- Unix domain sockets
- `jsonrpsee` or a lightweight custom JSON-RPC layer
- alternatively a simple HTTP-over-UDS admin API for early iterations

### TUI
- `ratatui` and `crossterm` for terminal UI

### Process/sandbox orchestration
- `tokio::process`
- direct invocation of `bwrap` (Phase 2a) and the Firecracker API (Phase 2b), behind one `SandboxBackend` trait
- avoid overcommitting to a heavy orchestration framework before the execution model is proven

## Control Plane Architecture Choice

## Choice
Implement Clyde as a **local daemon + CLI/TUI client** rather than as a purely one-shot CLI.

## Why
A daemon model is a better fit for:
- active missions and leases
- long-running task supervision
- background log streaming
- revocation and renewal
- local broker coordination
- UI clients reconnecting to the same mission state

## Recommended structure
- `clyded`: local daemon and control plane
- `clyde`: CLI/TUI client
- optional future editor extension speaking to `clyded`

## IPC recommendation
Use **Unix domain sockets** for local IPC.

Why:
- natural local trust boundary
- easy permission control
- low overhead
- simple integration for local clients and agent adapters

## Policy Representation

## MVP choice
Use a hybrid model:
- **built-in task policies in Rust code**
- **repo/user/org config in TOML**

### Example split
Built into code:
- meaning of `rust.check`
- meaning of `git.push`
- default runtime and trust class
- invariant rules like no credentials in T2 tasks

Configured in TOML:
- allowed registries
- repo subtree defaults
- which tasks are pre-approved for missions
- branch naming rules for push
- synthetic service defaults

### Configuration layering
Precedence is built-in defaults → host config → user config → `.clyde/policy.toml` in the repository ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)).

**Repository configuration may only narrow.** A repo value that would widen authority relative to the layer above is rejected with a diagnostic rather than silently clamped, because a silent clamp leaves the user believing something is configured that is not. Repository content is untrusted, and a repo that can widen its own authority is a self-signed permission slip.

`AGENTS.md` is prose, passed into agent context, and never parsed for authority.

## Why this hybrid
A fully dynamic policy engine is not needed for MVP and would add complexity early.

The built-in policy layer guarantees strong semantics for core tasks. TOML overlays allow project and user customization without letting repos redefine the security meaning of tasks.

## Why TOML
TOML fits well because:
- readable for humans
- already familiar in Rust-heavy ecosystems
- good for static local configuration
- easier to validate than ad hoc YAML in many cases

## Not recommended for MVP
### OPA / Rego
Powerful, but too much machinery for early local-first implementation.

### Cedar
Interesting for authorization logic, but likely overkill before the task and mission model is stable.

### YAML-first policy DSL
Possible, but more error-prone and less aligned with a Rust-first configuration story.

## Metadata Store Choice

## Choice
Use **SQLite** as the primary metadata store for:
- missions
- leases
- task runs
- approvals
- audit events
- artifact metadata

## Why SQLite
SQLite is the right MVP choice because it is:
- local-first
- embeddable
- reliable
- easy to inspect and back up
- sufficient for a single-user or small local daemon model
- easy to test with fixtures

## Suggested schema areas
- `missions`
- `leases`
- `actors`
- `task_requests`
- `task_runs`
- `approvals`
- `artifacts`
- `broker_ops`
- `audit_events`

## Later evolution
If Clyde becomes a shared team service or remote control plane, add a Postgres-backed storage mode later.

## Snapshot Mechanism

## MVP choice
Use a **content-addressed snapshot store on the local filesystem** with:
- scoped file collection
- path filtering
- hashing
- compressed tarball or unpacked snapshot directories

## Recommended implementation approach
For each task:
1. collect allowed paths from workspace
2. apply exclusions from policy
3. compute content hashes
4. store snapshot manifest in SQLite
5. materialize snapshot for runtime as:
   - a read-only directory tree, or
   - a tar/zstd bundle unpacked into sandbox input

## Why this choice
It is:
- simple
- explicit
- testable
- portable across runtime backends

## Not recommended for MVP as the only approach
### overlayfs-only design
Fast, but makes later portability and debugging harder, and can complicate unprivileged operation.

### btrfs/zfs dependency
Attractive for performance in some setups, but too opinionated for Linux-first MVP portability.

## Later optimizations
- content deduplication
- reflink-aware copies
- overlay materialization for hot loops
- OCI-style layer export for remote execution

## Sandbox Backend Choice

## Decision
Implement a `SandboxBackend` trait with two backends, in this order ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)):

1. **bubblewrap** (Phase 2a) — namespace isolation, read-only binds, tmpfs scratch, seccomp, cgroup v2 limits applied by clyded
2. **Firecracker** (Phase 2b) — microVM isolation for untrusted project execution and for everything network-bearing

Rootless Podman is **not** implemented.

## Why bubblewrap first
The stated goal is to reach microVM isolation quickly. Bubblewrap exercises the whole snapshot / task / artifact / cache pipeline at very low startup cost, needs no image build pipeline, and consumes the same nix-closure runtime roots that the Firecracker rootfs is built from. A Podman backend in between would be work discarded on the way to Firecracker.

## Why not Podman
Podman's value here was the OCI image model and its ecosystem. With runtime roots defined as nix closures ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)) most of that value disappears, and rootless containers were never the intended long-term boundary for high-risk tasks. Skipping it removes an image-build pipeline and a daemon-adjacent dependency from the MVP.

## What bubblewrap is and is not good enough for
Acceptable on the namespace backend:
- the workspace environment, which hosts a semi-trusted agent and has no project build toolchain
- `rust.check` and `rust.test.unit` during Phase 2a bring-up only

Not acceptable on the namespace backend:
- any task with an egress profile other than `none` — which is precisely why Phase 2b precedes Phase 3
- T3 tasks generally

## Isolation is policy-driven
Each task policy declares a `min_isolation`. The sandbox manager may select a stronger backend but never a weaker one, and there is no manual override. A configured downgrade is recorded in the audit log and is unavailable for T3 tasks.

## Host prerequisites
Both backends need real host capabilities, and the failure modes are unfriendly without diagnostics. See [Phase 0 host prerequisites](phase-0-foundations.md#host-prerequisites) — in particular Ubuntu 24.04's `kernel.apparmor_restrict_unprivileged_userns=1`, which blocks unprivileged user namespaces for nix-store binaries. `clyde doctor` must distinguish that case from user namespaces being unavailable altogether, because the remedies are entirely different.

## Runtime Root Strategy

## Decision
Each task family executes against a **nix closure** defined in the project flake and identified by its store path ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)). There is no OCI image build, no registry, and no digest-pinning pipeline.

### Runtime roots for the MVP
- `runtimeRoots.workspace` — text and code manipulation tooling for the workspace environment
- `runtimeRoots.rust` — Rust toolchain for check/test/build
- `runtimeRoots.fetch` — cargo plus network client tooling for dependency resolution

Under bubblewrap the closure is bind-mounted read-only. Under Firecracker the guest rootfs is built from the same closure, so runtime-root identity is stable across backends.

## Why
The flake is already the source of truth for tooling ([AGENTS.md](../AGENTS.md#tooling-source-of-truth)). Reusing it for runtime roots gives content-pinned reproducibility for free, keeps one definition of "the Rust toolchain we use", and removes upstream base-image trust from the supply chain.

## Cost accepted
nix becomes a requirement on any host that executes tasks, not only on developer machines. That is a deliberate narrowing of portability for the MVP.

## Strict requirement for the workspace runtime root
`runtimeRoots.workspace` must be separate from the build/test/fetch roots. This is a hard architectural requirement, not a preference.

It should include:
- shell utilities
- search and filter tools
- structured text and code transformation helpers
- an interpreter suitable for one-off codemods

It must not include the project build toolchain: no `cargo`, `rustc`, `rustup`, `node`, `npm`/`pnpm`/`yarn`, browser, `gpg`, `ssh`, `docker`, or `podman`.

Because a runtime root is a derivation, this is checkable rather than merely reviewable: [Phase 0](phase-0-foundations.md#2-runtime-root-assertion-test) requires a test that inspects the closure and fails if any of those appear.

## Not recommended
- one mutable runtime root shared across task families
- installing packages inside a sandbox at task time
- reusing the build runtime root as the workspace runtime root

## Artifact Store Choice

## MVP choice
Use:
- **filesystem blob store** for content
- **SQLite metadata tables** for indexing and lineage

### Suggested layout
```text
~/.local/share/clyde/
  db.sqlite
  blobs/
  snapshots/
  logs/
  artifacts/
```

## Why
This gives a clean split between:
- large opaque payloads
- queryable metadata

## Hashing recommendation
Use **BLAKE3** for fast local content addressing.

Use SHA-256 as needed for compatibility with external signing/provenance workflows later.

## Logging, Audit, and Provenance

## MVP choice
Use:
- `tracing` for structured in-process events
- JSON log output for task and daemon events
- SQLite-backed audit event records for durable indexing

## Why
This keeps observability simple and local-first while still structured enough for later export.

## Later evolution
Add:
- OpenTelemetry export
- in-toto / SLSA-style provenance records
- Sigstore-compatible attestations

## Recommendation
Do not block MVP on a fully standardized provenance format. Capture the right linkage first:
- mission id
- lease id
- actor id
- task id
- snapshot id
- artifact ids
- approval ids

## Credential Broker Choice

## MVP choice
Implement a **separate local Rust broker daemon** behind a Unix socket API.

### Why separate daemon
It creates a clean process boundary between:
- general task orchestration
- privileged external authority

That makes it easier to:
- harden later
- audit separately
- freeze privileged actions independently
- evolve multiple broker backends

## Broker API style
The broker should expose typed actions, not raw secret access.

Good:
- `git_push(branch, commit)`
- `sign_digest(digest, key_profile)`
- `publish_artifact(artifact_id, destination)`

Bad:
- `get_ssh_key()`
- `read_gpg_secret()`
- `return_github_token()`

## Git implementation choice
For MVP, use the **git CLI** in the broker rather than jumping immediately to `libgit2`.

Why:
- simpler to reason about operationally
- easier to match normal user git behavior
- fewer early surprises than abstracting git too aggressively

Later, evaluate whether select operations should move to native libraries.

## Signing choice
For MVP, the broker may wrap:
- `gpg` for GPG signing where required
- SSH signing if desired
- later Sigstore/Cosign integration for artifact signing

The key design rule is more important than the exact tool:
- the signer stays in the broker environment
- signing acts on explicit inputs
- untrusted code never gets key material

## Browser Test Technology

> Post-MVP: browser and synthetic-service work belongs to Phase 5, outside the Phase 0-4 boundary. The choices below remain the intended direction.

## MVP choice
Use **Playwright** in a dedicated sandboxed task profile.

## Why Playwright
- mature automation stack
- good tracing and screenshot support
- practical for modern full-stack workflows
- easier to script than building a custom browser harness

## Key security rule
Playwright must run with:
- isolated browser profile
- synthetic or scoped test identity only
- no reuse of developer browser cookies or host profile

## Synthetic Services Technology

## MVP choice
Use simple isolated service containers or sandbox-side processes for:
- Postgres
- Redis
- fake SMTP
- fake OAuth/OIDC
- optional S3-compatible service

Potential implementation options:
- dedicated synthetic service images
- Compose-like internal orchestration by Clyde
- sidecar task launch pattern

## Why simple is acceptable first
The important thing is the security property:
- synthetic services are easy to spin up
- credentials are fake or ephemeral
- network remains private

The orchestration layer can get more sophisticated later.

## CLI and TUI Choice

## MVP choice
Implement both CLI and lightweight TUI in Rust.

### CLI should handle
- mission creation
- task execution
- approvals
- status inspection
- artifact lookup

### TUI should emphasize
- conversation view
- mission/lease status
- running task list
- approval pane
- log view

## Why not IDE-first
IDE integration is valuable, but a CLI/TUI-first core:
- keeps the architecture honest
- reduces early integration complexity
- makes testing and debugging easier
- supports agent experimentation without editor-specific constraints

## Agent Integration API Choice

## Decision
Expose the actor-facing API as **MCP over a Unix domain socket** from Phase 1, with JSON-RPC retained for the CLI and admin surface ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api)). One internal command model, two transports.

## Why MCP first
It makes the MVP drivable by an existing MCP-capable agent with no bespoke adapter, so the delegation workflow is provable in Phase 1 rather than at the end of the roadmap.

## What is deliberately not in the API
File reading and editing are not tools. The agent runs inside a workspace-environment sandbox and uses its own filesystem tools; edit scope is enforced by mount topology ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)). That is both a stronger boundary than an API check and the reason an off-the-shelf agent works unmodified.

## Tool descriptions are security UX
Each tool description states the policy consequences of calling it: whether it can cause network access, whether it needs approval, and what it will refuse. The agent's understanding of its constraints comes from these descriptions, so vagueness there produces an agent that asks for the wrong things.

## Later evolution
Editor plugins, and gRPC if remote deployment is added.

## Actor Authentication and Socket Topology

## Decision
Three Unix sockets with distinct trust properties ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)):

| Socket | Callers | Mounted into sandboxes | Authentication |
|---|---|---|---|
| `clyded.sock` | actors (agents, sub-agents) | yes, workspace environments only | session capability token |
| `clyded-admin.sock` | the human operator | never | `0600` plus `SO_PEERCRED` |
| `brokerd.sock` | clyded only | never | `SO_PEERCRED`, plus independent approval verification |

## Why the split
"The human approves boundary crossings" is only enforceable if an agent cannot impersonate the human. With one socket and a self-declared actor identity, the agent whose escalation is under review can approve it. Splitting the channel makes self-approval structurally impossible rather than policy-prohibited, and `clyde approve` refusing to run inside a sandbox is the visible form of that.

## Packaging and Distribution

## MVP choice
Ship Clyde as:
- one primary Rust binary for CLI/TUI
- one local daemon binary
- optional one broker binary if separated at process level
- a set of nix-built runtime root closures

## Why
This keeps installation and versioning manageable.

## Development and Testing Tooling

## Recommended tooling
- `cargo test` for unit tests
- `nextest` for faster Rust test execution
- `insta` for snapshot testing of mission/policy outputs where helpful
- `assert_cmd` for CLI integration tests
- `tempfile` for filesystem-heavy tests
- containerized integration tests for sandbox/backend behavior

## Suggested test layers
### Unit tests
- mission manager
- lease derivation
- policy resolution
- approval logic

### Integration tests
- snapshot generation
- no-network enforcement
- dependency fetch then offline compile
- brokered push flow

### Security assertions
- no mount of `~/.ssh`, `~/.gnupg`, browser profiles
- forbidden path access denied
- forbidden task requests denied

## Explicit MVP Technology Choices

For clarity, the MVP implementation stack is:

- **Language:** Rust
- **Process model:** three binaries — `clyde` (CLI/TUI), `clyded` (control plane), `clyde-brokerd` (broker) ([D15](decisions.md#d15-three-binaries-from-phase-0))
- **IPC:** Unix domain sockets; MCP for actors, JSON-RPC for CLI and admin
- **Socket topology:** separate actor, admin, and broker sockets ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel))
- **Config format:** TOML, layered, repository config narrow-only
- **Policy representation:** built-in typed policies in Rust plus TOML overlays
- **Metadata DB:** SQLite
- **Artifact store:** local filesystem blobs plus SQLite metadata
- **Hashing:** BLAKE3 locally, SHA-256 where external compatibility matters
- **Logging:** tracing plus JSON logs, with hash-chained audit events in SQLite
- **Sandbox backends:** bubblewrap (Phase 2a) then Firecracker (Phase 2b), behind one trait
- **Runtime roots:** nix closures per task family, no OCI images
- **Caches:** read-only dependency bundles, per-mission writable build cache ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only))
- **Egress control:** Clyde CONNECT proxy, socket-bridged into a loopback-only network namespace
- **Client:** CLI through Phase 3, TUI in Phase 4 ([D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4))
- **Credential broker:** separate local Rust daemon over a Unix socket, developer credential held in-process only ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only))
- **Git implementation:** git CLI, invoked through a single sanitised-invocation helper that disables hooks and neutralises system, global, and repository configuration
- **Browser and synthetic services:** post-MVP (Phase 5)

## Known Future Upgrades

These are not MVP requirements, but the architecture should keep room for them:
- Firecracker or Kata backend for stronger isolation
- OpenTelemetry export
- in-toto/SLSA provenance
- Sigstore/Cosign artifact signing
- Postgres-backed shared control plane mode
- editor plugins and remote deployment support
- richer synthetic environment orchestration

## Summary

The implementation strategy for Clyde Next is intentionally conservative:
- Rust for the trusted core
- SQLite and filesystem storage for local-first durability
- TOML plus built-in typed policies for understandable configuration
- nix closures as runtime roots, with bubblewrap giving way quickly to Firecracker
- a separate broker daemon for privileged authority
- Unix sockets for clean local interfaces, split by trust level

This stack is not the final endpoint, but it is a practical path to proving Clyde's core thesis:

> agentic coding can be fast and useful without giving build code, dependencies, or coding agents ambient access to credentials, unrestricted network, or publish authority.
