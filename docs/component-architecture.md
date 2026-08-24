# Clyde Next: Component Architecture

## Purpose

This document defines the major components of Clyde Next and the responsibilities, interfaces, and trust boundaries between them.

It translates the high-level architecture, mission/lease model, task policy matrix, and sequence flows into a concrete subsystem view suitable for implementation planning.

This document builds on:
- [terminology.md](terminology.md)
- [high-level-design.md](high-level-design.md)
- [mission-lease-model.md](mission-lease-model.md)
- [task-policy-matrix.md](task-policy-matrix.md)
- [sequence-flows.md](sequence-flows.md)

> **Decision status.** Component boundaries here reflect the binding decisions in [decisions.md](decisions.md). Three changes are worth noting before reading further: the agent is *hosted* by Clyde inside a workspace-environment sandbox rather than being an external caller ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)); the gateway is split into an actor surface and a human-only admin surface ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)); and an egress proxy is a first-class component from Phase 1 ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)).

## Architecture Summary

Using the terminology from [terminology.md](terminology.md), this document describes how Clyde's core terms map onto concrete subsystems:
- the **workspace** maps to workspace-facing interfaces and workspace-environment execution
- the **mission** and **actor** model map to mission and lease management
- **policy** maps to task resolution and approval decisions
- **environments** map to workspace-environment execution, build-environment execution, and brokered external actions

Clyde Next should be implemented as a **trusted control plane** coordinating several specialized subsystems:
- actor gateway and admin gateway
- mission and lease manager
- session and token manager
- policy engine
- snapshot manager
- sandbox manager, with pluggable isolation backends
- egress proxy
- artifact layer
- credential broker
- audit and provenance system

The core rule is:

> No human, agent, sub-agent, or untrusted build task bypasses the control plane for privileged effects.

## Top-Level Component Map

```text
+---------------------------+        +---------------------------+
|  Human: CLI / TUI         |        |  Agent (hosted by Clyde)  |
|  clyded-admin.sock        |        |  workspace environment    |
|  approvals, missions      |        |  clyded.sock (MCP+token)  |
+-------------+-------------+        +-------------+-------------+
              |                                    |
              |  admin surface                     |  actor surface
              v                                    v
+----------------------------------------------------------------+
|                         Clyde Control Plane                     |
| +------------------+  +------------------+  +----------------+ |
| | Mission Manager  |  | Policy Engine    |  | Approval       | |
| | Lease Manager    |  | Task Resolver     |  | Manager        | |
| +------------------+  +------------------+  +----------------+ |
| +------------------+  +------------------+  +----------------+ |
| | Snapshot Manager |  | Sandbox Manager   |  | Artifact       | |
| |                  |  | Task Scheduler    |  | Coordinator    | |
| +------------------+  +------------------+  +----------------+ |
| +------------------+  +------------------+  +----------------+ |
| | Broker Gateway   |  | Audit/Provenance |  | Egress Proxy   | |
| | Session/Tokens   |  |                  |  |                | |
| +------------------+  +------------------+  +----------------+ |
+----+--------------------+-------------------+-------------------+
     |                    |                   |
     v                    v                   v
+-------------------+  +---------------+  +----------------+
| Sandboxes         |  | clyde-brokerd |  | Artifact Store |
| bwrap / microVM   |  | (brokerd.sock)|  | + Logs + SBOMs |
| nix runtime roots |  |               |  |                |
+-------------------+  +---------------+  +----------------+
```

## Trust Boundaries

### Trusted components
The following are part of Clyde's trusted computing base for product behavior:
- workspace and agent gateway
- mission manager
- lease manager
- policy engine
- approval manager
- snapshot manager
- sandbox manager
- broker gateway
- audit/provenance system
- credential broker implementation

### Untrusted or hostile-by-default components
The following must be treated as untrusted:
- project source code
- dependencies
- build scripts
- proc macros
- tests
- package install hooks
- browser automation hooks
- arbitrary repo scripts
- agent-authored utility scripts
- **repository git configuration and hooks** (`.git/config`, `.git/hooks`) — attacker-controlled content that a naive credentialed git invocation would execute ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only))
- outputs originating from untrusted execution until validated by policy

### Semi-trusted components
The coding agent occupies a middle position that is worth naming explicitly. It is not hostile-by-default in the way dependency code is, but it is not trusted either: it runs in a low-authority sandbox with no project build toolchain, its writable surface is its lease scope, and its only egress is an allowlisted model API. The design does not rely on agent good behaviour for any security property except the residual exfiltration risk stated in [D11](decisions.md#d11-workspace-environment-model-api-egress-goes-through-the-clyde-proxy).

## Component Specifications

## 1. Actor Gateway and Admin Gateway

### Purpose
Provide the interactive surfaces through which actors and humans interact with Clyde — as **two** surfaces with different trust properties, not one ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).

### Actor gateway (`clyded.sock`)
- serves MCP tools to hosted agents and sub-agents ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api))
- authenticates every request by session capability token
- accepts task requests, escalation requests, sub-agent requests, publish requests
- returns task status, logs, artifacts, and structured denials
- may be bind-mounted into workspace-environment sandboxes

### Admin gateway (`clyded-admin.sock`)
- serves the human operator over JSON-RPC: mission create, approve, deny, revoke, renew, workspace register, audit read
- mode `0600`, `SO_PEERCRED`-checked, and **never** mounted into any sandbox
- the only surface on which an approval can be made

### Not in either gateway
File read and write are not gateway operations. The agent uses its own filesystem tools against its sandbox mounts, and edit scope is enforced by mount topology ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)). This removes a large API surface and replaces a policy check with a kernel-enforced boundary.

### Inputs
- human instructions
- agent requests
- workspace file operations
- approval responses

### Outputs
- mission creation requests
- lease-bound task requests
- edit operations
- review summaries
- approval events

### Trust properties
- both gateways are trusted surfaces
- neither exposes raw credentials
- neither executes untrusted project build/test code
- the actor gateway never grants an approval; the admin gateway never accepts an actor token

### Interface sketch

Actor surface (MCP, token-authenticated):
```text
mission_status()
list_capabilities()
run_task(task_type, path, options?)
task_status(task_run_id)
task_logs(task_run_id, range?)
list_artifacts(filter?)
request_escalation(capability, reason, details)
request_subagent(purpose, scope, tasks, duration)
request_publish(action, details)
commit_prepare(message, paths?)
```

Admin surface (JSON-RPC, human only):
```text
workspace_register(root)
create_mission(objective, scope?, preferences?)
review_mission(mission_id)
approve(request_id, grant)      // once | for_mission
deny(request_id, reason)
revoke_mission(mission_id, reason)
renew_lease(lease_id)
audit_show(filter)
```

## 2. Mission Manager

### Purpose
Create, track, and close missions.

### Responsibilities
- create missions from human intent and policy defaults
- track mission objective, scope, and lifecycle state
- associate missions with actors, leases, tasks, approvals, and artifacts
- determine mission completion, expiry, suspension, or revocation

### Mission states
- proposed
- awaiting_approval
- active
- blocked_on_escalation
- paused
- completed
- revoked
- expired
- failed

### Key data
- mission metadata
- objective
- repo scope
- allowed tasks
- allowed sub-agent roles
- budget and stop conditions
- approval history
- completion summary

### Trust properties
- trusted control-plane component
- authoritative source for mission state

## 3. Lease Manager

### Purpose
Issue and validate capability leases for agents and sub-agents.

### Responsibilities
- issue primary leases from missions
- derive child leases from parent leases
- track lease budgets and expiry
- validate every lease-bound action
- revoke or renew leases

### Lease states
- issued
- active
- exhausted
- expired
- revoked
- superseded

### Key checks
- actor identity matches lease binding
- requested repo path is within lease scope
- requested task is in task scope
- lease is not expired or revoked
- budget remains
- derived lease does not exceed parent rights

### Trust properties
- trusted
- every side-effecting action should pass through lease validation

## 3b. Session and Token Manager

### Purpose
Bind an actor process to a lease, and make that binding checkable on every request.

### Responsibilities
- issue a 256-bit session capability token when an actor is bound to a lease
- store only the token hash; deliver the plaintext to the sandbox token file (`0400`) and nowhere else
- resolve token → session → lease → mission on every actor request, before any other check
- expire sessions with their lease, and revoke them transactionally with lease revocation
- track which sandbox hosts which session, so teardown is complete

### Trust properties
- trusted
- an unknown, expired, or revoked token is rejected identically, revealing nothing about which

## 4. Policy Engine

### Purpose
Resolve missions, task policies, escalations, and approval requirements.

### Responsibilities
- map task types to task policy profiles
- determine environment, isolation profile, and resource profile
- evaluate mission defaults by repo or organization policy
- decide whether a request is auto-approvable, approval-gated, or denied
- enforce invariants such as no raw credentials in untrusted tasks

### Inputs
- mission creation request
- task request
- escalation request
- publish request
- lease renewal request

### Outputs
- approved/denied decision
- resolved task policy
- required approvals
- suggested narrower alternatives

### Policy sources
Potential policy inputs include:
- built-in Clyde defaults
- repo-local policy config
- repo-local guidance such as `AGENTS.md`
- org/team policy overlays
- host environment policy

### Trust properties
- trusted
- must fail closed when policy is missing or ambiguous for privileged actions

## 5. Approval Manager

### Purpose
Coordinate human-visible decisions at boundary crossings.

### Responsibilities
- create approval prompts from policy decisions
- present the narrowest understandable request to the human
- record approvals, denials, and timeouts
- support approve-once, approve-for-mission, and deny patterns

### Typical approval cases
- dependency fetch with network access
- private dependency access
- broader repo scope
- external integration testing
- git push
- signing
- publishing
- lease renewal beyond defaults

### Trust properties
- trusted
- approval artifacts should be immutable and linked to mission + lease + action

## 6. Snapshot Manager

### Purpose
Convert live mutable workspace content into immutable task inputs.

### Responsibilities
- create subtree snapshots
- normalize inputs for reproducibility where practical
- compute stable snapshot identifiers
- support lockfile-only or config-only snapshots for fetch tasks
- optionally support diff-based or content-addressed deduplication

### Inputs
- workspace paths
- task policy
- mission/lease scope

### Outputs
- snapshot id
- snapshot manifest
- materialized runtime input bundle or mount source

### Design constraints
- snapshots are **always** read-only to tasks; hardlink materialisation shares inodes with the content store, which makes the read-only bind load-bearing rather than stylistic
- snapshot creation should be fast enough for inner-loop iteration
- snapshots should not accidentally include excluded paths such as secrets, local browser state, or unrelated repos
- **input scope is a pinned access baseline**, seeded from the cargo build closure of the requested path rather than from the lease's edit scope: cargo cannot build a subtree without the workspace root manifest, `Cargo.lock`, every member manifest, and in-repo path-dependency sources. The baseline is stored in Clyde state, enforced by materialising granted subtrees plus confirmed pins, and drift is escalated ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift))
- editing work is intentionally not snapshot-based; it happens in the workspace environment against the live tree ([D17](decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment))

### Trust properties
- trusted
- essential for keeping execution separated from mutable edits

## 6b. Access Baseline Store

### Purpose
Hold the confirmed record of what each build target may read and which dependencies execute code during its build, and detect divergence from it ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).

### Responsibilities
- store baselines keyed by workspace, task type, and build target, in Clyde's own state
- hold first-party project code as **subtree grants** derived from the mission-approved scope, and out-of-scope reads as individually confirmed **file pins**
- propose an initial baseline from the statically computed build closure
- record learn-mode observations as a proposal, never as a confirmed baseline
- compute the code-execution inventory from the lockfile and dependency bundle, and diff it against the pinned one
- classify drift and hand it to the approval manager
- refuse tasks whose target has no confirmed baseline

### Design constraints
- baselines are never stored in the repository: repository content is untrusted, and an attacker able to edit the baseline could conceal their own drift
- the inventory check must complete **before** the sandbox starts, so new dependency code is caught before it runs
- path enforcement is delegated to the snapshot manager by materialising granted subtrees minus absolute exclusions plus confirmed pins, so enforcement cannot drift from the record
- a subtree grant is not drift-sensitive: ordinary editing inside the mission's approved scope must never produce a prompt, or the control becomes noise and stops being read
- a confirmed baseline requires a human decision on the admin channel

### Trust properties
- trusted
- because the baseline lives only in Clyde state, it is invisible to code review and lost with that state; the mission envelope therefore summarises the baseline in force, and export/import for backup is worth adding post-MVP

## 7. Sandbox Manager and Task Scheduler

### Purpose
Launch and supervise isolated task execution.

### Responsibilities
- choose the appropriate environment and isolation backend based on resolved task policy
- materialize sandbox inputs and outputs
- configure network, mounts, scratch, and resource limits
- support both snapshot-based build-environment execution and live-workspace utility execution
- start, monitor, stop, and clean up sandboxes
- stream logs and status back to Clyde
- support retries and debug retention policies

### Inputs
- resolved task policy
- snapshot reference
- dependency bundle reference
- lease and actor metadata
- resource budget

### Outputs
- task id
- lifecycle events
- exit status
- logs
- output artifact references
- policy violation observations if available

### Runtime backends
Two backends, behind one `SandboxBackend` trait ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D9](decisions.md#d9-the-firecracker-backend-lands-as-phase-2b-before-dependency-resolution)):
- **bubblewrap namespace sandbox** (Phase 2a) — the workspace environment, and build/test during bring-up
- **Firecracker microVM** (Phase 2b) — untrusted project execution and everything network-bearing

Later implementations may add remote isolated runners, browser-specialized runners, and policy-specialized fetch runners.

Backend selection comes from the policy's `min_isolation`. A stronger backend may be substituted; a weaker one never may, and any configured downgrade is audited and unavailable for T3 tasks.

Anything a backend cannot honour in a `SandboxSpec` is a preflight failure, never a silent relaxation. That rule is what stops the trait from becoming the place where boundaries quietly weaken.

### Runtime roots
Execution roots are nix closures, not OCI images ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)). The workspace runtime root has strict requirements:
- separate from the build/test/fetch roots
- text and code manipulation tooling
- **no** project build toolchain — asserted by a test over the closure, not by review

### Trust properties
- trusted orchestrator, untrusted workload
- must never collapse policy boundaries for performance convenience

## 8. Artifact Coordinator and Artifact Store

### Purpose
Store and move snapshots, dependency bundles, outputs, logs, traces, and provenance.

### Responsibilities
- persist task outputs by task id and artifact id
- separate trusted metadata from untrusted payloads
- support retrieval by mission, lease, task, or actor
- retain or garbage-collect artifacts according to policy
- support lineage and provenance views

### Artifact categories
- source snapshots
- dependency bundles
- build outputs
- package outputs
- logs
- screenshots/videos/traces
- coverage reports
- SBOM fragments
- provenance records
- approval records

### Trust properties
- trusted metadata layer
- content may be untrusted; consumers should know artifact origin and trust class

## 8b. Egress Proxy

### Purpose
Make "no network" and "registry-only" mechanically true rather than declarative.

### Responsibilities
- give every sandbox a loopback-only network namespace, with no route to the host network
- accept HTTP `CONNECT` from the in-sandbox forwarder over a bound Unix socket (bubblewrap) or vsock (Firecracker)
- allowlist by destination host per egress profile, and refuse everything else
- record every attempt, allowed or refused, as an `EgressAttempt`, and emit a fetch manifest artifact
- enforce per-task byte and connection budgets, charged to the lease

### Design constraints
- the allowlist decision is host-side and trusted; nothing inside the sandbox can widen it
- profile `none` is implemented by not binding the socket at all — there is no flag that disables egress
- no TLS interception, therefore destination scoping without content inspection. The limitation is stated in the approval UX rather than hidden

See [network-egress-model.md](network-egress-model.md) for the full design.

### Trust properties
- trusted
- the in-sandbox forwarder is trusted code running in an untrusted namespace; compromising it grants nothing beyond what the proxy already permits

## 9. Broker Gateway

### Purpose
Provide a single Clyde-side adapter for all privileged external operations.

### Responsibilities
- translate high-level authority requests into broker-specific calls
- isolate the rest of Clyde from direct credential handling
- normalize audit and approval behavior across brokers

### Brokered operations
- git fetch / push
- signing
- registry publish
- scoped token minting
- possibly SSH-backed repo operations

### Trust properties
- trusted
- should expose capability-oriented operations, not generic secret retrieval

## 10. Credential Broker

### Purpose
Execute privileged actions without exposing raw credentials to agents or untrusted tasks.

### Responsibilities
- authenticate to external services
- perform requested high-privilege actions
- mint short-lived scoped tokens where policy allows
- sign manifests or digests
- push or publish artifacts

### Design constraints
- should accept narrowly typed requests
- should not expose raw private keys or long-lived tokens to callers
- should be separately auditable
- should support explicit allowlists and destination restrictions
- must **independently verify** the approval record rather than trusting the caller's assertion that approval exists
- must not execute repository-controlled code: git invocations run with hooks disabled and system, global, and repository configuration neutralised, from a sanitised temporary repository ([Phase 4](phase-4-credential-broker.md#5-hostile-repository-hardening))
- runs as a separate process from Phase 0, so the boundary is never retrofitted ([D15](decisions.md#d15-three-binaries-from-phase-0))

### Trust properties
- highly trusted
- must be isolated from the build environment

## 11. Audit and Provenance System

### Purpose
Provide a complete record of decisions, actions, artifacts, and authority transitions.

### Responsibilities
- record mission creation, approvals, denials, renewals, and revocations
- record lease issuance and derived lease graphs
- record task execution metadata
- record brokered actions
- link artifacts to tasks and inputs
- provide human-readable summaries and machine-readable logs

### Minimum linkage model
Each significant event should link:
- mission id
- lease id
- actor id
- task id where relevant
- snapshot id
- artifact ids
- approval id where relevant

### Trust properties
- trusted system of record
- should be append-oriented and tamper-evident where practical

## Data Model Overview

## Primary entities
- `Workspace`
- `Mission`
- `Lease`
- `Actor`
- `ActorSession`
- `TaskRequest`
- `TaskRun`
- `Snapshot`
- `Artifact`
- `ApprovalRequest`
- `ApprovalDecision`
- `BrokeredOperation`
- `PolicyDecision`
- `EgressAttempt`
- `AuditEvent`

Field-level definitions, state machines, invariants, and the persistence layout are in [schema-reference.md](schema-reference.md).

## Critical relationships
- a mission has many leases
- a lease may have a parent lease
- a mission has many task runs
- a task run references one task policy and one snapshot
- a task run produces artifacts
- a brokered operation may require an approval decision
- audit events reference all of the above

## Core APIs Between Components

## Gateway -> Mission Manager
```text
createMission(objective, scope, preferences, initiator)
getMission(missionId)
closeMission(missionId)
revokeMission(missionId, reason)
```

## Gateway / Agent -> Lease Manager
```text
getActiveLease(actorId, missionId)
requestSubagent(parentLeaseId, scope, tasks, duration, purpose)
renewLease(leaseId)
validateAction(leaseId, actionDescriptor)
```

## Control Plane -> Policy Engine
```text
resolveMissionPolicy(request)
resolveTaskPolicy(taskType, repoPath, missionId, leaseId)
evaluateEscalation(request)
checkApprovalRequirement(action)
```

## Control Plane -> Snapshot Manager
```text
createSnapshot(paths, mode, exclusions, missionId, leaseId)
getSnapshot(snapshotId)
```

## Control Plane -> Sandbox Manager
```text
startTask(taskPolicy, snapshotId, inputs, outputs, actorContext)
getTaskStatus(taskId)
terminateTask(taskId)
```

## Control Plane -> Broker Gateway
```text
gitPush(request)
gitFetch(request)
signArtifact(request)
publishArtifact(request)
mintScopedToken(request)
```

## Control Plane -> Artifact Store
```text
storeArtifact(metadata, contentRef)
getArtifact(artifactId)
listArtifactsByTask(taskId)
```

## Non-Functional Design Constraints

### Performance
- safe inner-loop tasks must have low enough latency for iterative agent workflows
- snapshot creation and task launch must be optimized without relaxing isolation

### Failure handling
- task failures must preserve logs and output references
- lease expiry and revocation must stop new work immediately
- broker failure must not leave partial ambiguous authority state

### Portability
- architecture should allow local-first Linux implementation first
- component boundaries should not assume only one runtime backend forever

### Testability
- policy engine, lease validation, and mission transitions should be testable without real sandboxes
- broker gateway should be mockable
- snapshot manager should have deterministic fixtures

## Recommended MVP Component Scope

For an initial implementation, Clyde Next should build the minimum useful slice of each component.

### MVP components
- actor gateway (MCP) and admin gateway (JSON-RPC)
- mission manager
- lease manager
- session and token manager
- policy engine with a closed built-in task catalog
- snapshot manager with build-closure-aware scoping
- sandbox manager with the namespace backend and then the microVM backend
- egress proxy
- artifact store for logs and outputs
- broker gateway + git push broker
- append-only hash-chained audit log
- approval manager with digest-bound approvals

### Deferred components
- advanced org policy hierarchy
- distributed runner fleet
- rich provenance attestations
- multiple broker implementations
- advanced analytics and dashboards

## MVP Trust Boundary Recommendation

The MVP should preserve these boundaries even if implementation is simple:
- live workspace separate from task snapshots
- untrusted task execution separate from credential broker
- publish/sign separate from build/test
- lease validation on every side-effecting agent action
- actor surface separate from the human approval surface, so no actor can approve its own escalation
- the workspace environment separate from the build toolchain, so typed tasks are the only path to project execution
- egress decisions made host-side, so no sandbox can widen its own reachability

## Summary

Clyde Next should be built as a modular control plane where each subsystem has a narrow, explicit responsibility.

The critical implementation insight is that the product does not need one giant secure container. It needs a set of cooperating trusted components that:
- grant bounded autonomy through missions and leases
- translate agent requests into typed policy-resolved tasks
- run hostile code in isolated sandboxes
- move outputs through an artifact layer
- keep credentials behind brokered authority boundaries

This component model provides the implementation backbone for the security and UX model described in the rest of the Clyde Next design docs.
