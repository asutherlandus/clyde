# Clyde Next: Requirements

## Purpose

This document defines requirements for a clean-sheet version of Clyde designed to provide a least-privilege development environment for Rust projects and modern full-stack applications while preserving full support for agentic coding workflows.

It assumes the threat model described in [problem-statement-threat-model.md](problem-statement-threat-model.md).

> **Decision status.** Requirements here are unchanged in intent, but several have been made mechanical by the decisions in [decisions.md](decisions.md) — notably agent hosting (A4), actor authentication (P), egress enforcement (C), and the runtime requirements in section I.

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
- **hosting the coding agent process itself** ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment))

This environment must not directly expose publish, signing, raw credential capabilities, or the full project build/test toolchain. Because the agent runs inside it, the absence of the build toolchain is what makes typed tasks the only path to project execution.

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
Clyde shall support low-authority execution of agent-authored ephemeral scripts that manipulate the live workspace, as part of `workspace.edit`.

In the MVP this is satisfied by the workspace environment itself rather than by a separately launched sandbox, because the agent already runs in exactly such an environment ([D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)). The requirement is unchanged: this execution shall be distinct from build/test/fetch execution and shall enforce all of the following:
- live workspace access limited to the lease-scoped repository paths
- writable outputs limited to allowed repo paths and isolated scratch space
- no raw credentials
- no host home directory, unrelated projects, browser state, or container runtime sockets
- no external network except the configured model API allowlist, proxied and logged
- a separate runtime root from build/test/fetch task environments
- tooling for text and code manipulation
- no project build toolchain available inside that runtime

If a command requires the project build toolchain, executes repo-defined build/test/install code, or needs broader authority, it shall run as a different typed task under the appropriate policy rather than as workspace editing. This shall be enforced by the absence of that tooling from the workspace runtime root, not only by policy text, and that absence shall be verified by an automated test.

#### A4. Agent hosting
The coding agent process and every sub-agent process shall run inside a Clyde-managed workspace-environment sandbox, not on the host ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)).

Each such sandbox shall:
- mount the lease's edit paths read-write, and other permitted paths read-only
- mount `.git` read-only, so repository hooks and configuration cannot be planted by an actor
- mount the actor socket, but never the admin or broker socket
- receive a session capability token as a file with mode `0400`
- have no host home directory, no credentials, and no egress except the model API allowlist

Editing authority shall be enforced by mount topology, so that an out-of-scope write fails at the kernel rather than at a policy check.

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
Clyde shall isolate caches to prevent cross-contamination and persistence.

Specifically ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)):
- dependency caches shall be content-addressed and mounted read-only
- writable build caches shall be scoped to a single mission and destroyed at mission closeout
- no cache shall be shared across missions, across projects, or with the host's own package-manager caches

### C. Network isolation

#### C1. Deny-by-default execution
Compile, build, codegen, and most test tasks shall run with no external network access by default.

Deny-by-default shall be structural: a task with egress profile `none` shall receive a loopback-only network namespace with no proxy socket bound, so that "no network" is the absence of a channel rather than a configuration flag ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)).

#### C2. Separate fetch stage
Dependency download and update operations shall run in a distinct task or stage from compilation.

#### C3. Destination-scoped egress
When network is allowed, policy shall restrict access by destination, and shall do so from outside the sandbox.

Egress shall be expressed as one of a closed set of named **egress profiles**, enforced by a host-side proxy that the sandbox reaches over a bound socket, with every attempt recorded whether allowed or refused. No code inside a sandbox shall be able to widen its own reachability. See [network-egress-model.md](network-egress-model.md).

The limits of this mechanism shall be stated in approval prompts: allowlisting is by destination, not by content.

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

#### E6. Pinned build access baseline
Build and test tasks shall run against a confirmed access baseline recorded in Clyde's own state, covering:
- the repository content the task may read, as **subtree grants** for first-party project code within the mission-approved scope, plus individually confirmed **file pins** for anything outside it
- the inventory of dependency packages that execute code at build time — `build.rs` and proc-macro crates — by crate, version, and source content hash

Path enforcement shall be by materialisation, so the task receives the granted subtrees minus absolute exclusions, plus the pins, and nothing else. A task with no confirmed baseline shall be refused, not run with wider access.

Absolute exclusions — secret-shaped files, key material, build output directories, and `.git` by default — shall be applied before grants and shall not be admissible by a grant.

Ordinary development within a granted subtree — creating, renaming, moving, or deleting files — shall not constitute drift and shall not require approval. The authority for those paths was granted when the human approved the mission envelope.

The baseline shall not be stored in the repository, because repository content is untrusted and an attacker able to edit the baseline could conceal drift ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

#### E7. Drift detection and pre-execution ordering
Clyde shall escalate when observed build access diverges from the baseline:
- a read outside the granted subtrees and confirmed pins, including a new in-repo path dependency on a crate outside the approved scope
- a new code-executing dependency, a version change to one, or a content change at the same version

Path drift and dependency drift shall be presented distinctly, since they carry different signal and warrant different scrutiny.

The code-execution inventory check shall run **before** the task's sandbox starts, so that newly arrived dependency code is surfaced before it executes rather than after.

#### E8. Learn mode is privileged
Any mode that runs a build with auto-admitted access for the purpose of recording a baseline shall be:
- invocable only by a human on the admin channel
- unavailable to any actor, and never selected by Clyde as a fallback
- limited to a single run
- recorded distinctly in the audit log
- without effect until a human confirms the resulting proposal

Clyde shall propose a baseline from static analysis of the build closure so that learn mode is the exception rather than the normal path.

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

Denials shall be structured and actionable: what was denied, which constraint denied it, and what narrower or escalated alternative exists. An agent given a bare denial will either loop or try to route around the boundary, and both outcomes are worse than a clear next step.

## P. Actor identity and authority separation

### P1. Session-bound authority
Every actor session shall be bound to a lease by a capability token issued by Clyde. Every side-effecting request shall carry that token, and shall be rejected if the token is unknown, expired, or revoked ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).

Tokens shall be stored hashed, delivered only into the actor's sandbox as a file with mode `0400`, never passed in `argv` or environment, never logged, and never returned by any query.

### P2. Separate human approval channel
Approvals shall be accepted only on a human-only channel that is not reachable from any sandbox. No actor-facing operation shall grant, consume, or modify an approval.

This shall be structural rather than policy-based: the approval socket shall not exist in any sandbox mount table, and the approval command shall refuse to run inside a sandbox.

### P3. Independent verification at authority boundaries
A component that performs a privileged effect shall verify the approval record itself rather than trusting a caller's assertion that approval exists.

### P4. Approval binding
An approval shall be bound to a digest of the exact normalised request it authorises. A request differing in any authority-relevant field shall not match, and shall require a new approval. Single-use approvals shall be consumed transactionally with the action they authorise.

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

#### I0. Separate low-authority workspace runtime
Clyde shall provide a workspace runtime that is separate from build/test/fetch runtimes.

This shall be a strict design requirement, not an implementation preference. It shall:
- be separate from build/test/fetch runtimes
- provide tooling for text and code manipulation
- omit the project build toolchain, verified by an automated test over the runtime root's contents
- deny credentials, and deny egress other than the configured model API allowlist
- mount only lease-scoped live workspace paths plus isolated scratch space

#### I1. Strong isolation for untrusted execution
Clyde shall support a stronger isolation boundary than a general shared development shell for untrusted tasks. Ephemeral microVMs are the intended boundary for higher-risk execution.

Each task policy shall declare a minimum isolation level. The sandbox manager may select a stronger backend, and shall never select a weaker one. Any configured downgrade shall be audited, and shall be unavailable for T3 tasks. No task with an egress profile other than `none` shall run below microVM isolation ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)).

#### I2. Ephemerality
Untrusted task environments shall be short-lived and destroyed after completion unless retained explicitly for debugging.

#### I3. Read-only, content-pinned runtime roots
Clyde shall execute tasks against read-only, content-pinned runtime roots.

In the MVP these are nix closures identified by store path rather than OCI images ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)). The requirement is the property — reproducible, read-only, content-identified — not the packaging format.

#### I4. Resource controls
Each task shall support memory, CPU, time, and process-count limits.

For trust class T2 and above these limits shall be enforced by cgroup v2. Where cgroup v2 delegation is unavailable, such tasks shall be refused; there shall be no configuration that permits untrusted project execution under process-level rlimits alone ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)).

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
- Policy resolution, capability decisions, and broker interfaces shall be testable independently of a full end-to-end runtime.
- Policy resolution, lease derivation, egress profile ordering, and budget charging shall be pure functions with no I/O dependency.
- The system's security claims shall be expressed as automated tests, including: no credential path in any generated sandbox mount table; egress refusal from a `none`-profile sandbox; cache non-persistence across missions; no code execution during a brokered push against a repository containing hostile hooks and configuration.

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
