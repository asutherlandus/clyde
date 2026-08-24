# Clyde Next: Requirements

## Purpose

This document defines requirements for a clean-sheet version of Clyde designed to provide a least-privilege development environment for Rust projects and modern full-stack applications while preserving full support for agentic coding workflows.

It assumes the threat model described in [problem-statement-threat-model.md](problem-statement-threat-model.md).

## Product Goal

Clyde must provide a development platform where untrusted project code, dependencies, build scripts, tests, and agent-requested execution can be run with fine-grained isolation over:
- filesystem access
- network access
- credential access
- artifact flow
- publish and signing authority

The default experience should make the secure path the easiest path.

## Design Principles

The new Clyde should be designed around these principles:

1. **Least privilege by default**
2. **Explicit trust boundaries**
3. **Ephemeral execution for untrusted code**
4. **Credentials are brokered, not mounted**
5. **Network is denied unless specifically required**
6. **Build and publish are separate security domains**
7. **Agent capabilities are policy-controlled**
8. **All privilege escalations are explicit and auditable**

## System Model

Clyde shall separate the system into a trusted control plane and a small number of distinct environments.

### 1. Control plane
Trusted local supervisor responsible for:
- policy evaluation
- task scheduling
- snapshot creation
- sandbox lifecycle
- audit logging
- artifact collection
- credential-broker coordination

### 2. Workspace environment
Environment used for:
- reading code
- editing files
- searching the repository
- viewing logs and artifacts
- running low-authority helper scripts for ad hoc code manipulation
- requesting task execution

This environment must not directly expose publish, signing, raw credential capabilities, or the full project build/test toolchain.

Repo-local agent guidance such as `AGENTS.md` should be readable from this environment and should be available as an input to agent behavior, mission defaults, and workspace-edit guidance.

### 3. Build environment
Sandboxed environment used for:
- dependency installation and fetch
- Rust compilation
- procedural macro execution
- `build.rs`
- frontend builds
- tests
- browser automation
- project-defined code generation

This environment must be assumed hostile.

### 4. Broker environment
Brokered capability layer used for:
- git fetch and push
- SSH-backed operations
- signing
- cloud/API token issuance
- registry publish operations

### 5. Artifact layer
Storage and transfer layer for:
- source snapshots
- dependency bundles
- build outputs
- logs
- SBOMs
- provenance and attestation metadata

## Functional Requirements

### A. Task model

#### A0. Repo-local agent guidance
Clyde should support a repo-local `AGENTS.md` file as a source of development and coding guidance for humans and coding agents.

`AGENTS.md` may define guidance such as:
- expected development workflow
- flake-based tooling expectations
- coding style and review standards
- security-sensitive implementation constraints
- testing expectations

`AGENTS.md` should guide how work is performed, but it should not silently override core security policy or widen authority.


#### A1. Typed task execution
Clyde shall expose common workflows as typed tasks rather than relying exclusively on unrestricted shell execution.

Examples include:
- `workspace.edit`
- `rust.resolve-deps`
- `rust.check`
- `rust.build`
- `rust.test.unit`
- `rust.test.integration`
- `node.resolve-deps`
- `web.build`
- `browser.test`
- `git.fetch`
- `git.push`
- `artifact.sign`
- `artifact.publish`

Each typed task shall have a predefined policy covering:
- filesystem scope
- network scope
- credential access
- writable paths
- execution runtime
- time and resource limits
- output locations

#### A2. Arbitrary command support
Clyde may support arbitrary commands, but it shall classify them into explicit risk classes such as:
- low-authority workspace utility commands
- trusted maintenance commands
- untrusted project commands
- privileged brokered operations

Arbitrary commands shall not bypass policy enforcement.

#### A3. Helper-driven workspace editing
Clyde shall support a low-authority helper-execution mode as part of `workspace.edit` for agent-authored ephemeral scripts that manipulate the live workspace.

This mode shall be treated as distinct from build/test/fetch execution and shall enforce all of the following:
- live workspace access limited to the lease-scoped repository paths
- writable outputs limited to allowed repo paths and isolated scratch space
- no raw credentials
- no host home directory, unrelated projects, browser state, or container runtime sockets
- no external network by default
- a separate image/runtime from build/test/fetch task environments
- tooling for text and code manipulation
- no full project build toolchain available inside the edit-helper runtime

If a command requires the project build toolchain, executes repo-defined build/test/install code, or needs broader authority, it shall run as a different typed task under the appropriate policy rather than as workspace editing.

### B. Filesystem isolation

#### B1. Snapshot-based inputs
Untrusted tasks shall run against immutable snapshots of required inputs rather than a shared mutable workspace mount.

#### B2. Scoped repository access
Clyde shall support limiting a task to a repository subtree or explicit path set when possible.

#### B3. Isolated scratch space
Each untrusted task shall receive isolated writable scratch space and isolated output directories.

#### B4. Host filesystem protection
The following shall not be mounted into untrusted build and test sandboxes by default:
- host home directory
- `~/.ssh`
- `~/.gnupg`
- cloud credential directories
- browser profiles
- editor IPC sockets
- Docker or container runtime sockets
- unrelated projects

#### B5. Cache isolation
Clyde should support isolating caches by project, trust level, and task type to reduce cross-contamination and persistence risks.

### C. Network isolation

#### C1. Deny-by-default execution
Compile, build, codegen, and most test tasks shall run with no external network access by default.

#### C2. Separate fetch stage
Dependency download and update operations shall run in a distinct task or stage from compilation.

#### C3. Destination-scoped egress
When network is allowed, policy shall be able to restrict access by destination, protocol, and purpose.

#### C4. Synthetic test networks
Clyde should support isolated internal test networks for integration and browser tests using fake or local-only services.

#### C5. Audited exceptions
Any task profile that enables real external network access shall be explicit, narrowly scoped, and logged.

### D. Credential security

#### D1. No raw credential mounting
Untrusted tasks shall not receive raw SSH keys, GPG private keys, long-lived API tokens, or general-purpose agent sockets.

#### D2. Brokered git operations
Git operations that require credentials shall be mediated by a broker or equivalent control-plane service.

#### D3. Brokered signing
Signing operations shall accept explicit data, digests, or manifests to sign. Private key material shall remain outside untrusted execution environments.

#### D4. Scoped token issuance
When external service access is required, Clyde should issue short-lived, purpose-scoped credentials bound to a task or policy.

#### D5. Approval support
High-risk operations such as push, sign, publish, or access to production-facing credentials should support explicit approval workflows.

### E. Rust-specific requirements

#### E1. Treat compile as code execution
Clyde shall treat Rust build actions as untrusted code execution, including:
- `cargo check`
- `cargo build`
- `cargo test`
- `build.rs`
- procedural macros
- doctests
- custom cargo workflows

#### E2. Networkless Rust compilation
Rust compile and test tasks shall run without network by default.

#### E3. Pre-fetched dependencies
Rust dependency retrieval should occur in a separate fetch stage, preferably from a controlled mirror, proxy, vendor bundle, or content-addressed cache.

#### E4. Dependency policy
Clyde should support policy over Rust dependency sources, including restrictions on git dependencies, lockfile drift, and unexpected dependency changes.

#### E5. Build observability
Clyde should capture enough build execution metadata to identify suspicious subprocesses, file access patterns, and blocked network attempts when feasible.

### F. Full-stack and frontend requirements

#### F1. Treat package installation as untrusted
JavaScript and frontend dependency installation shall be treated as untrusted execution due to lifecycle scripts and plugin hooks.

#### F2. Build/test phase separation
Frontend dependency installation, frontend build, and browser/e2e execution shall be separable into different task policies.

#### F3. Browser isolation
Browser-based test tasks shall use isolated browser state and shall not reuse the developer's real browser session or profile.

#### F4. Synthetic identities for tests
Where possible, browser and integration tests should use synthetic or narrowly scoped test identities instead of real developer credentials.

### G. Agentic coding support

#### G1. Policy-aware task API
Clyde shall provide an API that allows agents to request task execution, retrieve logs, read artifacts, and request capability elevation through explicit interfaces.

#### G2. Separation of powers
The platform should logically separate agent behaviors for planning, editing, execution, and publishing even if implemented within one product surface.

#### G3. No implicit privilege inheritance
The fact that an agent can edit code shall not imply permission to execute untrusted code with credentials, push commits, or publish artifacts.

#### G4. Capability requests
Agents should be able to request additional capabilities with a stated reason, desired scope, and time limit, subject to policy and approval.

### H. Artifact flow and provenance

#### H1. Controlled outputs
Artifacts produced by untrusted tasks shall move through explicit output channels rather than direct access to privileged environments.

#### H2. Immutable input identity
Each task should record stable identifiers for the source snapshot, dependency inputs, and toolchain used.

#### H3. Audit trail
Clyde shall record an audit trail for each task including:
- task type
- policy profile
- input identifiers
- network policy
- credential policy
- execution result
- produced outputs

#### H4. Provenance support
Clyde should support generation of provenance or attestation metadata for build outputs and release inputs.

## Runtime Requirements

### I. Sandbox runtime

#### I0. Separate low-authority edit runtime
Clyde shall provide a separate runtime for helper-driven `workspace.edit` and similar workspace-environment editing support.

This runtime shall be a strict design requirement, not an implementation preference. It shall:
- be separate from build/test/fetch runtimes
- provide tooling for text and code manipulation
- omit the full project build toolchain
- deny credentials and external network by default
- mount only lease-scoped live workspace paths plus isolated scratch space

#### I1. Strong isolation for untrusted execution
Clyde shall support a stronger isolation boundary than a general shared development shell for untrusted tasks. Ephemeral microVMs or similarly hardened sandboxes are preferred for higher-risk execution.

#### I2. Ephemerality
Untrusted task environments shall be short-lived and destroyed after completion unless retained explicitly for debugging.

#### I3. Read-only base images
Clyde should use pinned, reproducible, read-only base images or equivalent runtime roots where practical.

#### I4. Resource controls
Each task shall support memory, CPU, time, and process-count limits.

### J. Policy engine

#### J1. Declarative policies
Clyde should represent task policies declaratively so they can be reviewed, tested, and versioned.

#### J2. Policy visibility
Before or during execution, Clyde should be able to show what a task can access, including mounts, network, outputs, and credentials.

#### J3. Safe defaults
If a policy cannot be determined, the system shall fail closed or use a clearly restricted default rather than silently granting broad access.

## UX Requirements

### K. Developer experience

#### K1. Secure path by default
The normal workflow for checking, testing, building, and publishing shall route through policy-enforced task execution.

#### K2. Clear capability display
The CLI and any agent-facing UI should clearly show when a task has:
- no network
- limited network
- brokered credentials
- approval requirements
- elevated risk

#### K3. Debuggable failures
When a task fails due to isolation policy, Clyde should provide clear diagnostics indicating whether the failure came from filesystem restrictions, blocked network, missing capability, or sandbox runtime issues.

#### K4. Escape hatches
Clyde may provide an explicit unsafe or compatibility mode, but it shall be clearly labeled, auditable, and disabled by default.

## Non-Functional Requirements

### L. Security
- Default operation must not expose raw credentials to untrusted execution.
- Compromised build steps must be unable to directly sign or publish outputs.
- The system must minimize cross-project contamination through caches and mounts.

### M. Performance
- Task startup should be optimized through caching, snapshots, or VM reuse mechanisms that do not collapse security boundaries.
- Secure execution should be fast enough to support iterative development and agent loops.

### N. Portability
- The initial target may be Linux-first, but the architecture should avoid unnecessary coupling to a single host deployment model.

### O. Testability
- Policy resolution, capability decisions, and broker interfaces should be testable independently of a full end-to-end runtime.

## Out of Scope for the First Version

The first version does not need to include all of the following:
- perfect reproducibility across every language ecosystem
- support for every package manager and framework
- zero-cost compatibility with arbitrary developer machine state
- production-grade remote execution or distributed builds

However, the architecture should leave room for those additions later.

## Summary

The new Clyde must replace the idea of a single all-powerful development container with a policy-driven system that separates:
- editing from execution
- dependency fetch from compilation
- build from publish
- task execution from credential use

The essential requirement is that untrusted project code can be built, tested, and iterated on effectively without granting it broad access to the network, host filesystem, or developer credentials.
