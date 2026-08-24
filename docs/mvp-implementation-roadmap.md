# Clean-Sheet Clyde: MVP Implementation Roadmap

## Purpose

This document proposes a phased implementation roadmap for Clyde Next.

It is designed to translate the architecture, mission/lease model, task policy matrix, and sequence flows into a practical build order. The emphasis is on delivering meaningful least-privilege improvements early while preserving a usable agentic coding experience.

This document builds on:
- [component-architecture.md](component-architecture.md)
- [mission-lease-model.md](mission-lease-model.md)
- [task-policy-matrix.md](task-policy-matrix.md)
- [sequence-flows.md](sequence-flows.md)

## Roadmap Principles

The roadmap should follow these principles:
- preserve the core trust boundaries from the beginning
- make the safe inner loop usable as early as possible
- separate execution from authority before adding advanced features
- start with a narrow task catalog and deepen it incrementally
- optimize for one strong end-to-end workflow before broad platform coverage
- prefer explicit policy even when initial policy is simple

## What the MVP Must Prove

A successful MVP should prove all of the following:

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
The MVP should optimize for this workflow:
- Rust backend project
- optional frontend build support
- one human
- one primary coding agent
- mission creation and one active lease
- repeated `rust.check` and `rust.test.unit`
- separate dependency resolution
- brokered `git.push`
- visible approval prompts

### Out-of-scope for MVP
The MVP should not require:
- release publishing
- signing pipeline
- multi-agent orchestration beyond optional single-level sub-agent experimentation
- broad language ecosystem coverage
- enterprise policy federation
- remote runner fleet
- fully general browser testing

## Recommended Phases

## Phase 0: Foundations and framing

### Goal
Create the shared data model, task taxonomy, and policy skeleton before building runtime behavior.

### Deliverables
- canonical task type list for MVP
- mission schema
- lease schema
- task policy schema
- audit event schema
- component boundaries and interface definitions
- initial repo policy format

### MVP task set for this phase
- `workspace.read`
- `workspace.edit`
- `repo.search`
- `rust.resolve-deps`
- `rust.check`
- `rust.test.unit`
- `git.push`

### Exit criteria
- all major entities and state transitions documented
- basic policy resolution rules defined
- implementation interfaces agreed

## Phase 1: Mission, lease, and approval core

### Goal
Implement bounded autonomy without yet requiring full sandbox sophistication.

### Deliverables
- mission creation flow
- lease issuance and validation
- mission status and review UI/CLI
- approval prompt system
- revocation and expiry handling
- audit event capture for missions, leases, approvals, and edits

### User-visible outcome
A human can:
- define a mission
- approve a mission envelope
- see active lease scope and budget
- revoke or renew a lease

An agent can:
- read and edit within scoped paths
- request allowed tasks
- receive clear denials when exceeding scope

### Security value
This phase establishes the human-to-agent delegation boundary and prevents the system from relying on ambient authority.

### Exit criteria
- every side-effecting agent action requires an active lease
- lease scope enforcement works for file edits and task requests
- approvals and denials are visible and logged

## Phase 2: Snapshot-based execution and safe inner loop

### Goal
Make autonomous edit / check / test loops practical and secure.

### Deliverables
- snapshot manager
- task execution pipeline
- sandbox manager with initial isolated runtime backend
- support for `rust.check`
- support for `rust.test.unit`
- task logs and output artifact capture
- task status UI/CLI

### User-visible outcome
An agent can:
- edit files
- run `rust.check`
- run `rust.test.unit`
- inspect failure logs
- retry without repeated approval

A human can:
- watch task history
- review which policy ran
- inspect produced logs/artifacts

### Security value
This phase is where Clyde first replaces the unsafe "agent runs code in its own environment" model.

### Implementation notes
- a hardened container backend may be acceptable for earliest bring-up
- the abstraction should still preserve room for microVM backends
- snapshot vs live workspace separation should be preserved even if implementation is simple

### Exit criteria
- build/test tasks use immutable snapshots
- no-network default is enforced for compile/test
- no credentials are visible inside build/test runtimes
- autonomous inner loop feels acceptably fast

## Phase 3: Separate dependency resolution

### Goal
Enforce the critical fetch-vs-compile split.

### Deliverables
- support for `rust.resolve-deps`
- dependency bundle/cache artifact model
- registry-only network profile
- lockfile-aware fetch policy
- approval flow for dependency-fetch escalation
- rerun flow from fetch success back to no-network compile

### User-visible outcome
When a dependency is missing, the agent:
- receives a task failure
- requests escalation for dependency resolution
- gets approval for narrow registry-only fetch
- reruns compile without network after fetch succeeds

### Security value
This phase directly addresses the Rust and supply-chain threat model by removing normal justification for networked compilation.

### Exit criteria
- compile/test no longer fetch dependencies implicitly
- dependency fetch outputs are explicit artifacts or controlled caches
- fetch approvals are narrow and understandable

## Phase 4: Credential broker and brokered git push

### Goal
Separate code execution from authority.

### Deliverables
- broker gateway
- first credential broker implementation for `git.push`
- branch- and remote-scoped push approvals
- mission-to-publish request linkage
- audit records for brokered operations

### User-visible outcome
The agent can prepare a commit and request a push, but the push only happens:
- through Clyde
- with human approval
- without exposing SSH/Git credentials to the agent or build sandboxes

### Security value
This phase removes one of the biggest remaining ambient-power risks in typical agentic workflows.

### Exit criteria
- no direct SSH agent forwarding to agent or build sandboxes
- `git.push` works through broker only
- push prompts show branch, remote, commit, and actor clearly

## Phase 5: Frontend/full-stack extension

### Goal
Extend the model beyond Rust-only backend workflows.

### Deliverables
- `node.resolve-deps`
- `web.build`
- synthetic service support for integration tests
- optional `browser.test.synthetic` for a narrow supported path
- isolated browser/profile model

### User-visible outcome
Clyde supports a basic full-stack flow:
- fetch frontend deps under constrained network
- build frontend offline
- run synthetic browser or integration tests without real user credentials

### Security value
This phase validates that the model works for the more difficult full-stack case, not just Rust compilation.

### Exit criteria
- frontend build is offline after dependency fetch
- browser or integration tests do not reuse real developer session state
- synthetic services are usable enough to reduce pressure for real credentials

## Phase 6: Stronger isolation backend

### Goal
Upgrade untrusted execution from acceptable isolation to preferred isolation.

### Deliverables
- microVM backend or equivalent strong sandbox backend
- runtime selection in task policy
- parity for `rust.check`, `rust.test.unit`, `rust.resolve-deps`, `web.build`
- performance tuning for snapshot handoff and task startup

### User-visible outcome
No major workflow change; improved assurance and clearer policy fidelity.

### Security value
This phase improves defense against runtime escape and over-sharing risks that remain in weaker container-based setups.

### Exit criteria
- high-risk task families can run on strong isolation backend
- runtime selection is policy-driven, not manual
- performance remains viable for agent loops

## Phase 7: Sub-agent and derived lease support

### Goal
Support constrained parallelism without breaking the mission model.

### Deliverables
- derived lease issuance
- parent-child lease graph
- optional one-level sub-agent creation
- derived budget tracking
- revocation fan-out

### User-visible outcome
The primary agent can request one or more narrow sub-agents for limited scopes, and the human can inspect the resulting lease graph.

### Security value
This phase resolves the multi-agent tension while preserving the single authority model.

### Exit criteria
- sub-agents cannot exceed parent scope
- revoking parent mission/lease revokes children
- audit records clearly show parent-child lineage

## Phase 8: Publishing, signing, and provenance

### Goal
Complete the authority separation story for release workflows.

### Deliverables
- `artifact.sign`
- `artifact.publish`
- release plan review UI
- artifact lineage view
- stronger provenance generation

### User-visible outcome
Clyde can support a controlled release path with separate approvals for signing and publishing.

### Security value
This phase completes the build-vs-publish separation and improves release confidence.

### Exit criteria
- sign/publish never run in the same context as untrusted build execution
- publish acts on explicit artifacts and digests
- provenance references mission, task, snapshot, and approval chain

## MVP Detailed Slice

If implementation time is limited, the true MVP should stop after Phase 4.

That means the first end-to-end supported slice is:
- one human developer
- one agent
- mission creation
- one primary lease
- scoped workspace edits
- immutable snapshots
- `rust.check`
- `rust.test.unit`
- `rust.resolve-deps`
- approval prompt for dependency fetch
- brokered `git.push`
- task logs and mission review screen

This slice is narrow but strategically strong.

## Sequencing Rationale

### Why mission/lease first?
Because without it, agent autonomy falls back to implicit session authority and later hardening becomes messy.

### Why snapshots and check/test next?
Because the inner loop must be usable or the system will be rejected by developers.

### Why dependency fetch before fancy publish features?
Because separating fetch from compile is a core security property for Rust and full-stack ecosystems.

### Why git push broker before release publishing?
Because push is a frequent developer workflow and a common privilege exposure path.

## Acceptance Criteria by Phase

## Phase 1 acceptance
- mission creation takes a human prompt and produces a bounded mission
- lease enforcement blocks out-of-scope edits and task requests
- approval prompts are linked to mission and lease ids

## Phase 2 acceptance
- `rust.check` and `rust.test.unit` run from immutable snapshots
- agent can iterate autonomously inside lease
- tasks have visible logs and results

## Phase 3 acceptance
- dependency fetch is separate from compile
- compile fails rather than fetching implicitly
- fetch uses a constrained network profile

## Phase 4 acceptance
- push is brokered
- no raw SSH/Git credential access exists in task runtimes
- human approval is required for push

## Phase 5 acceptance
- frontend dependency fetch/build separation exists
- at least one synthetic integration/browser path works

## Engineering Workstreams

Implementation can proceed in parallel across these workstreams.

### Workstream A: Control plane and schemas
- mission manager
- lease manager
- task request model
- audit schema

### Workstream B: Workspace and UX
- CLI/TUI mission screens
- edit/task request interface
- approval UX
- mission/task summary views

### Workstream C: Execution plane
- snapshot manager
- sandbox manager
- runtime backend integration
- log streaming

### Workstream D: Broker plane
- broker gateway
- git push broker
- approval linkage

### Workstream E: Policy and testing
- task policy resolver
- lease validation rules
- mission defaults
- integration and threat-model tests

## Testing Strategy by Phase

### Unit tests
Should cover:
- mission state transitions
- lease derivation rules
- budget exhaustion
- approval decision logic
- policy resolution

### Integration tests
Should cover:
- snapshot correctness
- no-network task enforcement
- dependency fetch escalation path
- brokered push path
- revocation while tasks are pending or running

### Security tests
Should cover:
- no `~/.ssh` / `~/.gnupg` exposure in untrusted runtimes
- failed attempts to access forbidden repo paths
- failed attempts to request unauthorized task types
- task runtime network denial for offline tasks

## Risks and Mitigations

### Risk 1: inner-loop latency is too high
Mitigation:
- optimize snapshot creation
- cache immutable inputs safely
- tune runtime startup
- keep initial task catalog small

### Risk 2: UX becomes too approval-heavy
Mitigation:
- use mission-level pre-approval for safe inner-loop tasks
- keep approvals for boundary crossings only
- show narrow, comprehensible prompts

### Risk 3: escape hatch becomes the default
Mitigation:
- keep `shell.untrusted` visibly second-class
- prioritize adding first-class task types for common workflows
- log and review escape-hatch usage

### Risk 4: complexity overwhelms implementation
Mitigation:
- ship one strong Rust workflow first
- separate interface contracts from backend sophistication
- defer multi-agent, publishing, and enterprise features

## Suggested Initial Milestones

### Milestone 1
Mission + lease creation, scoped edits, and review UI.

### Milestone 2
Snapshot-based `rust.check` and `rust.test.unit` loop.

### Milestone 3
`rust.resolve-deps` with approval-gated registry-only network.

### Milestone 4
Brokered `git.push`.

### Milestone 5
Basic frontend support and synthetic integration test path.

## Summary

The recommended Clyde Next roadmap is to build from **bounded autonomy** outward:
- first missions and leases
- then safe isolated execution
- then dependency fetch separation
- then credential brokerage
- then full-stack expansion
- then stronger isolation and advanced release flows

This order gives Clyde usable agentic coding early while protecting the core trust boundaries that motivated the redesign in the first place.
