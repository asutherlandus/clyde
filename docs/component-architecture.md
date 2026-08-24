# Clean-Sheet Clyde: Component Architecture

## Purpose

This document defines the major components of Clyde Next and the responsibilities, interfaces, and trust boundaries between them.

It translates the high-level architecture, mission/lease model, task policy matrix, and sequence flows into a concrete subsystem view suitable for implementation planning.

This document builds on:
- [high-level-design.md](high-level-design.md)
- [mission-lease-model.md](mission-lease-model.md)
- [task-policy-matrix.md](task-policy-matrix.md)
- [sequence-flows.md](sequence-flows.md)

## Architecture Summary

Clyde Next should be implemented as a **trusted control plane** coordinating several specialized subsystems:
- workspace and agent interface
- mission and lease manager
- policy engine
- snapshot manager
- sandbox manager
- artifact plane
- credential broker
- audit and provenance system

The core rule is:

> No human, agent, sub-agent, or untrusted build task bypasses the control plane for privileged effects.

## Top-Level Component Map

```text
+----------------------------------------------------------------+
|                        Human / IDE / TUI                       |
+-------------------------------+--------------------------------+
                                |
                                v
+----------------------------------------------------------------+
|                    Workspace & Agent Gateway                    |
|  conversation | file ops | task requests | review | approvals  |
+-------------------------------+--------------------------------+
                                |
                                v
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
| +------------------+  +------------------+                      |
| | Broker Gateway   |  | Audit/Provenance |                      |
| +------------------+  +------------------+                      |
+----+--------------------+-------------------+-------------------+
     |                    |                   |
     v                    v                   v
+----------+      +---------------+    +----------------+
| Sandboxes|      | Credential    |    | Artifact Store |
| / MicroVM|      | Brokers       |    | + Logs + SBOMs |
+----------+      +---------------+    +----------------+
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
- outputs originating from untrusted execution until validated by policy

## Component Specifications

## 1. Workspace and Agent Gateway

### Purpose
Provide the single interactive surface through which humans and agents interact with Clyde.

### Responsibilities
- expose code read/write APIs
- host conversation state and task requests
- enforce path-scoped file operations based on lease
- present task status, logs, and artifacts
- present approval prompts and mission summaries
- provide a stable interface for IDE, TUI, or web UI clients

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
- trusted surface
- may access live workspace within configured repo root
- must not directly expose raw credentials
- must not directly execute untrusted project code

### Interface sketch
```text
read_code(paths)
edit_files(patch)
create_mission(objective, scope?, preferences?)
run_task(lease_id, task_type, path, options?)
request_subagent(parent_lease_id, purpose, scope, tasks, duration)
request_escalation(lease_id, capability, reason, details)
review_mission(mission_id)
approve(request_id)
reject(request_id)
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

## 4. Policy Engine

### Purpose
Resolve missions, task policies, escalations, and approval requirements.

### Responsibilities
- map task types to task policy profiles
- determine runtime class and resource profile
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
- org/team policy overlays
- runtime environment policy

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
- snapshots should be read-only to untrusted tasks
- snapshot creation should be fast enough for inner-loop iteration
- snapshots should not accidentally include excluded paths such as secrets, local browser state, or unrelated repos

### Trust properties
- trusted
- essential for keeping execution separated from mutable edits

## 7. Sandbox Manager and Task Scheduler

### Purpose
Launch and supervise isolated task execution.

### Responsibilities
- choose runtime class based on resolved task policy
- materialize sandbox inputs and outputs
- configure network, mounts, scratch, and resource limits
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
Initial implementations may support:
- container backend
- hardened container backend
- microVM backend

Later implementations may add:
- remote isolated runners
- browser-specialized runners
- policy-specialized fetch runners

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
- trusted metadata plane
- content may be untrusted; consumers should know artifact origin and trust class

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

### Trust properties
- highly trusted
- must be isolated from untrusted execution plane

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
- `Mission`
- `Lease`
- `Actor`
- `TaskRequest`
- `TaskRun`
- `Snapshot`
- `Artifact`
- `ApprovalRequest`
- `ApprovalDecision`
- `BrokeredOperation`
- `PolicyDecision`
- `AuditEvent`

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
- workspace and agent gateway
- mission manager
- lease manager
- policy engine with a small built-in task catalog
- snapshot manager
- sandbox manager with one hardened backend and one stronger backend path if practical
- artifact store for logs and outputs
- broker gateway + git push broker
- audit event log
- approval manager

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

## Summary

Clyde Next should be built as a modular control plane where each subsystem has a narrow, explicit responsibility.

The critical implementation insight is that the product does not need one giant secure container. It needs a set of cooperating trusted components that:
- grant bounded autonomy through missions and leases
- translate agent requests into typed policy-resolved tasks
- run hostile code in isolated sandboxes
- move outputs through an artifact plane
- keep credentials behind brokered authority boundaries

This component model provides the implementation backbone for the security and UX model described in the rest of the Clyde Next design docs.
