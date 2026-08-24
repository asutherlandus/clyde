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
| Agent integration API | **JSON-RPC over Unix domain socket** | add gRPC / MCP-facing adapters later |
| Config / policy format | **TOML for static config + Rust enums/structs for built-in task policy** | maybe Cedar/OPA-like policy layer later |
| Mission / lease / audit metadata store | **SQLite** | SQLite first, optional Postgres later |
| Snapshot storage | **content-addressed filesystem store + tar/zstd bundles** | add overlayfs/reflink optimizations later |
| Sandbox backend (MVP) | **rootless Podman or rootless Docker-compatible OCI runtime** | add Firecracker / Kata / gVisor class later |
| Workspace-environment image | **pinned OCI image dedicated to helper-driven `workspace.edit`** | keep separate from build/fetch/browser images |
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
- direct invocation of `podman`, `docker`, `bwrap`, `firecracker`, etc. initially
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

## MVP choice
Use **rootless Podman** as the preferred local sandbox backend.

Fallback:
- rootless Docker-compatible backend if necessary

## Why Podman
Podman is preferred because:
- rootless mode is a better fit for least-privilege local execution
- daemonless architecture reduces one category of ambient privileged service dependency
- OCI compatibility is good enough for task images and isolated runs
- integrates well with cgroups and namespace isolation on Linux

## What Podman is good enough for in MVP
- helper-driven `workspace.edit`
- `rust.check`
- `rust.test.unit`
- `rust.resolve-deps`
- synthetic test service containers

## Important caveat
Rootless containers are **not the long-term ideal boundary** for the highest-risk tasks. They are the practical MVP stepping stone.

## Preferred later evolution
Add a stronger backend class for T2/T3 tasks:
- **Firecracker** if fast local microVM management is practical
- **Kata Containers** if integration cost is lower in your environment
- possibly **gVisor** as an intermediate stronger isolation option

## Recommendation by phase
- MVP: rootless Podman
- Phase 2+: add microVM-capable backend abstraction
- Phase 3+: migrate high-risk tasks to stronger backend by policy

## Runtime Image Strategy

## MVP choice
Use a small number of pinned OCI images for task families.

### Suggested image families
- `clyde-edit-utility-base`
- `clyde-rust-base`
- `clyde-node-base`
- `clyde-browser-base`
- `clyde-fetch-base`

Each should be:
- pinned by digest
- minimal
- reproducible where practical
- separate from user workspace state

## Why
This reduces runtime drift and lets task policy choose a known base.

## Strict requirement for the workspace environment image
`clyde-edit-utility-base` must be a separate image/runtime from build/test/fetch images.
This is a hard architectural requirement, not a nice-to-have.

It should include tooling for text and code manipulation such as:
- shell utilities
- search/filter tools
- structured text/code transformation helpers
- interpreters suitable for one-off codemods

It must not include the full project build toolchain.
In particular, the workspace environment should not become a disguised dev container for:
- project compilation or test toolchains
- package-manager install/fetch workflows
- browser/e2e stacks
- signing/publishing tools
- container runtime access

## Not recommended
- one giant mutable dev image for everything
- on-the-fly package installation during offline compile/test tasks
- reusing build/test images as the workspace-environment image

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

## MVP choice
Expose a **local JSON-RPC API over Unix socket** for agent clients.

## Why
This is sufficient for:
- local agent processes
- TUI/CLI integration
- sub-agent creation flows
- mission-aware tool invocation

## Later evolution
Add adapters for:
- MCP-like tool interfaces
- editor plugins
- gRPC for remote deployment scenarios

## Packaging and Distribution

## MVP choice
Ship Clyde as:
- one primary Rust binary for CLI/TUI
- one local daemon binary
- optional one broker binary if separated at process level
- a small set of pinned OCI images

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

For clarity, the recommended MVP implementation stack is:

- **Language:** Rust
- **Daemon model:** local daemon + CLI/TUI client
- **IPC:** Unix domain socket + JSON-RPC
- **Config format:** TOML
- **Policy representation:** built-in typed policies in Rust + TOML overlays
- **Metadata DB:** SQLite
- **Artifact store:** local filesystem blobs + SQLite metadata
- **Hashing:** BLAKE3 locally, SHA-256 where external compatibility matters
- **Logging:** tracing + JSON logs
- **Sandbox backend:** rootless Podman
- **Runtime images:** pinned OCI images by environment and task type, including a separate helper-driven `workspace.edit` image
- **Browser runner:** Playwright in isolated sandbox
- **Credential broker:** separate local Rust daemon over Unix socket
- **Git broker implementation:** git CLI in broker
- **Signing broker implementation:** broker wraps gpg/ssh signing tools initially

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

The recommended implementation strategy for Clyde Next is intentionally conservative:
- Rust for the trusted core
- SQLite and filesystem storage for local-first durability
- TOML plus built-in typed policies for understandable configuration
- rootless Podman for MVP execution isolation
- a separate broker daemon for privileged authority
- JSON-RPC over Unix sockets for clean local interfaces

This stack is not the final endpoint, but it is a practical path to proving Clyde's core thesis:

> agentic coding can be fast and useful without giving build code, dependencies, or coding agents ambient access to credentials, unrestricted network, or publish authority.
