# Clean-Sheet Clyde: Mission and Capability Lease Model

## Purpose

This document defines the mission and capability lease model for Clyde Next.

It describes how Clyde can remain the sole policy and authority interface while still supporting fast, practical agentic development. The model provides a structured way for humans to delegate bounded autonomy to agents and sub-agents without granting ambient shell authority, raw credentials, or unrestricted execution.

This document builds on:
- [problem-statement-threat-model.md](problem-statement-threat-model.md)
- [requirements.md](requirements.md)
- [high-level-design.md](high-level-design.md)

## Summary

The mission model answers the question:

> How can an agent iterate autonomously without bypassing Clyde?

The answer is:
- a human or policy creates a **mission**
- Clyde turns that mission into one or more **capability leases**
- agents operate only within active leases
- sub-agents receive **derived leases** that are equal to or narrower than the parent lease
- boundary crossings require escalation, renewal, or a new mission

In short:

> A mission defines the goal and operating envelope. A capability lease is the time-bounded grant that allows an actor to work inside that envelope.

## Design Goals

The mission and lease model should satisfy these goals:
- preserve fast edit / build / test iteration
- allow constrained agent autonomy
- support sub-agents without multiplying privilege
- prevent ambient long-lived authority
- make delegation explicit and reviewable
- ensure all authority transitions are auditable
- support revocation, expiry, and renewal

## Core Concepts

## 1. Mission

A **mission** is the top-level unit of delegated work.

A mission captures:
- the objective to be achieved
- the actor or actors allowed to work on it
- the repository and artifact scope
- the task families allowed
- the resource and time budget
- the escalation rules
- the approval conditions

A mission is not itself a credential or runtime token. It is a Clyde-managed policy object.

### Examples
- implement rate limiting in `backend/auth`
- migrate frontend login flow to a new API
- investigate and fix failing unit tests in `crate/payment`
- prepare a release candidate without publishing it

## 2. Capability Lease

A **capability lease** is a time-bounded grant issued by Clyde that allows an actor to perform specific actions within a mission.

A lease is the concrete mechanism that enables autonomous work.

A lease may permit actions such as:
- read code in selected paths
- edit code in selected paths
- request specific typed tasks
- inspect logs and artifacts
- spawn a limited number of derived sub-agents
- repeat approved inner-loop actions up to a budget

A lease must never imply:
- raw access to credentials
- unrestricted shell access
- unbounded network access
- authority outside the mission scope

## 3. Actor

An **actor** is any entity that can operate under a mission or lease.

Actors include:
- human developer
- primary coding agent
- derived sub-agent
- trusted automation component acting through Clyde

Actors do not possess ambient power. They operate only through Clyde-managed authority.

## 4. Derived Lease

A **derived lease** is a lease issued to a sub-agent or delegated actor based on a parent lease.

A derived lease must be:
- equal to or narrower than the parent lease
- explicitly linked to its parent
- independently auditable
- independently revocable
- separately budgeted where appropriate

## 5. Escalation

An **escalation** is a request to perform an action outside the current lease.

Escalations may request:
- broader repository scope
- additional task types
- different network policy
- brokered external access
- a publish or sign operation
- a longer duration or larger budget

Escalation does not automatically succeed. Clyde must evaluate policy and may require human approval.

## 6. Boundary Crossing

A **boundary crossing** is any requested action that would exceed the mission or lease envelope.

Typical boundary crossings include:
- switching from no-network compile to dependency fetch
- moving from one repo subtree to another
- switching from synthetic test identities to real credentials
- creating more sub-agents than allowed
- pushing, signing, or publishing

## Mission Structure

A mission should contain at least the following fields.

## Required fields
- `mission_id`
- `objective`
- `initiator`
- `primary_actor`
- `scope`
- `allowed_tasks`
- `network_policy`
- `credential_policy`
- `approval_policy`
- `budget`
- `expiry`

## Recommended fields
- `priority`
- `parallelism_limit`
- `artifact_retention_policy`
- `allowed_subagent_roles`
- `default_runtime_class`
- `audit_level`
- `stop_conditions`
- `success_criteria`

## Example mission schema
```yaml
mission_id: m-2026-02-15-auth-rate-limit
objective: Implement rate limiting for auth endpoints and update tests
initiator: human:andrew
primary_actor: agent:default
scope:
  repo_paths:
    - backend/auth
    - backend/tests/auth
allowed_tasks:
  - rust.check
  - rust.test.unit
  - rust.test.integration.synthetic
network_policy: none
credential_policy: none
approval_policy:
  auto_approve_within_lease: true
  requires_human_for:
    - rust.resolve-deps
    - broader_path_access
    - real_external_network
    - git.push
    - artifact.sign
budget:
  max_duration: 45m
  max_task_executions: 40
  max_parallel_subagents: 2
  max_cpu_minutes: 60
expiry: 2026-02-15T18:00:00Z
stop_conditions:
  - success
  - escalation_required
  - budget_exhausted
success_criteria:
  - rust.check passes
  - auth unit tests pass
```

## Lease Structure

A lease should be smaller and more operational than a mission.

## Required fields
- `lease_id`
- `mission_id`
- `actor`
- `issued_by`
- `issued_at`
- `expires_at`
- `repo_scope`
- `task_scope`
- `network_scope`
- `credential_scope`
- `authority_flags`
- `budget`

## Recommended fields
- `parent_lease_id`
- `subagent_limit`
- `retry_limit`
- `revocation_conditions`
- `current_usage`
- `purpose`

## Example lease schema
```yaml
lease_id: l-2026-02-15-auth-rate-limit-main
mission_id: m-2026-02-15-auth-rate-limit
actor: agent:default
issued_by: clyde
issued_at: 2026-02-15T17:00:00Z
expires_at: 2026-02-15T17:45:00Z
repo_scope:
  - backend/auth
  - backend/tests/auth
task_scope:
  - rust.check
  - rust.test.unit
  - rust.test.integration.synthetic
network_scope: none
credential_scope: none
authority_flags:
  may_edit: true
  may_request_tasks: true
  may_spawn_subagents: true
  may_request_publish: false
budget:
  max_task_executions: 40
  max_parallel_subagents: 2
  max_iterations: 20
  max_cpu_minutes: 60
purpose: implement and validate auth rate limiting
```

## Invariants

The following invariants should hold.

## I1. No ambient authority
An actor must not gain authority merely by existing in a session or process tree.

## I2. Lease required for action
Every agent action with side effects must be attributable to an active lease.

## I3. Derived leases cannot widen privilege
A child lease must never exceed the parent lease in scope, task rights, network, credential access, duration, or authority.

## I4. Expiry is enforced
Expired leases must stop authorizing new actions.

## I5. Revocation is immediate for new work
Revoked leases must not authorize further edits, task requests, or sub-agent creation.

## I6. Credentials are brokered separately
A lease may authorize a request for a brokered operation, but it must not directly contain raw credentials.

## I7. Task execution remains policy-bound
A lease authorizes asking for a task, not bypassing the task policy engine.

## I8. Audit linkage is preserved
Every task, artifact, escalation, sub-agent, and privileged action must be traceable back to:
- mission
- lease
- actor
- approval record if applicable

## Mission Lifecycle

## Phase 1: Mission creation
A mission may be created by:
- a human directly
- Clyde from a human instruction plus policy defaults
- a trusted automation flow under policy

### Inputs
- natural language goal
- repo or subsystem target
- risk profile
- optional budget preferences

### Outputs
- proposed mission charter
- risk summary
- approval requirements

## Phase 2: Approval and issuance
Clyde evaluates the mission against policy.

Possible outcomes:
- auto-approved under policy
- human approval required
- denied
- revised mission suggested

If approved, Clyde issues an initial lease to the primary actor.

## Phase 3: Autonomous execution
Within the lease, the agent may:
- read and edit allowed files
- request approved typed tasks
- inspect artifacts
- repeat safe inner-loop work
- create derived sub-agents if allowed

## Phase 4: Escalation or renewal
If the actor hits a boundary, it may request:
- lease renewal
- lease expansion
- new derived lease
- brokered privileged operation
- replacement mission

## Phase 5: Completion
A mission completes when:
- success criteria are met
- the human ends it
- the budget is exhausted
- lease expiry occurs without renewal
- policy denies further escalation

## Phase 6: Closeout
On closeout, Clyde should:
- revoke active leases
- terminate or detach derived actors
- preserve audit and artifacts according to policy
- summarize changes, task history, and escalations

## Lease Lifecycle

## Issuance
A lease is issued only after mission approval and policy evaluation.

## Activation
A lease becomes active when attached to an actor session or agent instance.

## Use
Each action consumes some amount of lease budget, such as:
- one edit batch
- one task execution
- one sub-agent slot
- CPU or time budget

## Expiry
A lease expires automatically at `expires_at` or earlier if its budget is exhausted.

## Renewal
A lease may be renewed only by Clyde and only if policy allows it.

## Revocation
A lease may be revoked by:
- human developer
- Clyde policy engine
- supervisory automation
- incident response control

## Derived Sub-Agent Model

Sub-agents are important for practical agentic development, but they must fit the lease model.

## Creation flow
1. Parent agent requests a sub-agent
2. Request includes:
   - purpose
   - scope
   - requested tasks
   - expected duration
3. Clyde validates the request against the parent lease
4. Clyde issues a derived lease if allowed
5. Clyde records parent-child linkage

## Example derived lease
```yaml
lease_id: l-2026-02-15-auth-rate-limit-frontend-helper
mission_id: m-2026-02-15-auth-rate-limit
parent_lease_id: l-2026-02-15-auth-rate-limit-main
actor: agent:subagent-frontend
repo_scope:
  - frontend/login
task_scope:
  - web.build
  - browser.test.synthetic
network_scope: synthetic-only
credential_scope: none
authority_flags:
  may_edit: true
  may_request_tasks: true
  may_spawn_subagents: false
expires_at: 2026-02-15T17:20:00Z
```

## Required constraints
- parent cannot create unbounded descendants
- child cannot outlive mission without explicit renewal
- child cannot broaden repo scope
- child cannot request stronger network or credential scopes than parent
- child publish rights default to false

## Budgets and Quotas

Budgets are important because they limit both accidental runaway loops and malicious persistence attempts.

## Budget dimensions
Clyde should support lease budgets such as:
- wall clock duration
- number of task executions
- number of retries
- number of sub-agents
- CPU minutes
- memory ceiling
- network egress budget where relevant
- artifact retention size

## Why budgets matter
Budgets help:
- keep autonomous work bounded
- reduce surprise cloud or compute cost
- prevent endless loops
- surface when work needs re-approval
- constrain abuse if an agent behaves badly

## Approval Model

## What should be auto-approved within a lease
Typical examples:
- edit allowed files
- run `rust.check`
- run `rust.test.unit`
- rerun failed unit tests
- inspect logs and artifacts
- create one allowed narrow sub-agent

## What should usually require approval or policy gate
Typical examples:
- `rust.resolve-deps`
- private dependency fetch
- broader filesystem scope
- external network access beyond policy
- real browser identities or cookies
- `git.push`
- signing
- publishing

## Approval granularity
Clyde should support:
- approve once
- approve for mission
- approve for session
- deny and suggest narrower alternative

## Escalation Flow

An escalation request should include:
- current mission and lease ID
- requested new capability
- reason
- affected scope
- expected duration
- safer alternatives if available

## Example escalation
```yaml
mission_id: m-2026-02-15-auth-rate-limit
lease_id: l-2026-02-15-auth-rate-limit-main
request:
  type: task_addition
  capability: rust.resolve-deps
  reason: new crate was introduced in Cargo.lock
  requested_network: registry-proxy-only
  requested_credentials: none
  affected_scope:
    - backend/
  duration: 10m
alternatives:
  - vendor dependency bundle manually
  - remove new dependency
```

## Failure and Safety Behavior

## On lease expiry
Clyde should:
- stop accepting new actions under the lease
- allow already-running tasks to complete or terminate according to policy
- notify the human and actor
- offer renewal if allowed

## On revocation
Clyde should:
- block further actions immediately
- terminate or quarantine sub-agents if needed
- prevent further task requests
- preserve logs for review

## On policy violation
If an actor requests a forbidden action, Clyde should:
- deny the action
- log the request
- explain the denial
- optionally suggest an allowed alternative

## On suspected compromise
Clyde should be able to:
- revoke all leases in a mission
- suspend artifact movement
- freeze publish operations
- retain forensic logs and snapshots

## Interaction Model

## Human experience
The human should interact with missions and leases at a high level, not as raw security internals.

A good UX pattern is:
- human states goal
- Clyde proposes mission envelope
- human approves or narrows it
- agent works autonomously within lease
- Clyde interrupts only for escalations or completion

## Agent experience
The agent should see:
- current mission objective
- active lease scope
- allowed tasks
- remaining budget
- escalation paths
- reason for denials

The agent should not need to reason about raw credentials or sandbox internals.

## Example Human-Facing Flow

### Prompt
"Implement refresh-token rotation in backend auth and update relevant tests."

### Clyde proposes mission
- scope: `backend/auth`, `backend/tests/auth`
- tasks: `rust.check`, `rust.test.unit`, `rust.test.integration.synthetic`
- network: none
- credentials: none
- duration: 45m
- sub-agents: up to 1
- escalation required for dependency fetch or push

### Human approves
Clyde issues primary lease.

### Agent works
- edits files
- runs checks
- reruns tests
- asks for escalation only if needed

## API Sketch

## Mission creation
```text
create_mission(
  objective="Implement refresh-token rotation",
  scope=["backend/auth", "backend/tests/auth"],
  preferred_tasks=["rust.check", "rust.test.unit"],
  duration="45m"
)
```

## Lease-aware task request
```text
run_task(
  lease_id="l-2026-02-15-auth-rate-limit-main",
  task_type="rust.check",
  path="backend/auth"
)
```

## Sub-agent request
```text
request_subagent(
  parent_lease_id="l-2026-02-15-auth-rate-limit-main",
  purpose="update login page integration",
  scope=["frontend/login"],
  tasks=["web.build", "browser.test.synthetic"],
  duration="15m"
)
```

## Escalation request
```text
request_escalation(
  lease_id="l-2026-02-15-auth-rate-limit-main",
  capability="rust.resolve-deps",
  reason="new crate introduced",
  requested_scope=["backend/"],
  requested_network="registry-proxy-only"
)
```

## Relationship to Task Policies

The mission and lease model does not replace task policy. It sits above it.

### Division of responsibility
- **Mission**: defines the work objective and broad autonomy envelope
- **Lease**: grants a particular actor temporary authority inside that envelope
- **Task policy**: defines how a specific task runs, including runtime, mounts, network, and credentials

A lease can authorize `rust.check`, but the `rust.check` task policy still decides:
- snapshot behavior
- runtime class
- network denial
- writable outputs
- resource limits

This separation is important because it prevents agents from converting a broad lease into arbitrary execution semantics.

## Implementation Guidance

## Phase 1
Start with:
- one mission per agent conversation
- one primary lease
- optional single level of sub-agent leases
- simple budgets: duration, task count, sub-agent count
- explicit escalations for network and publish

## Phase 2
Add:
- mission templates
- policy-driven default missions by repo type
- automatic renewal suggestions
- richer quotas and analytics
- reusable approval policies

## Phase 3
Add:
- multi-agent orchestration
- hierarchical mission graphs
- delegated review agents
- enterprise policy controls and reporting

## Summary

The mission and capability lease model gives Clyde Next a practical answer to the central tension in secure agentic development.

It allows humans to delegate meaningful autonomy to coding agents while ensuring that:
- Clyde remains the sole authority surface
- autonomy is bounded in scope and time
- sub-agents do not multiply privilege
- boundary crossings are explicit
- credentials remain brokered
- every meaningful action remains attributable and auditable

This model should serve as the foundation for Clyde's human-agent interaction design, task orchestration, and approval workflow.
