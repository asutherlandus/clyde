# The Mission and Capability Lease Model

How Clyde stays the sole policy and authority interface while still supporting fast, practical development — including agentic development.

This is an **authority model over requests**, not over a hosted process, so it holds identically whether the actor is a human at a terminal, CI, an agent running outside Clyde, or an agent Clyde hosts. Only [derived sub-agent leases](../warden/design.md#sub-agents) belong to the warden, because only a hosted agent can spawn one.

Field-level schemas are in [schema.md](schema.md). Where the requester and the approver are the same human, an approval is a **confirmation** and is recorded as one ([D2 amendment](decisions.md#amendment-confirmation-semantics-when-the-operator-is-the-driver)); read "approval" throughout as covering both.

## The answer in short

> How can an actor work autonomously without bypassing Clyde?

- a human or policy creates a **mission**
- Clyde turns that mission into one or more **capability leases**
- actors operate only within active leases
- sub-agents receive **derived leases** equal to or narrower than the parent
- boundary crossings require escalation, renewal, or a new mission

A mission defines the bounded goal and envelope. A lease is the time-bounded grant that lets a specific actor work inside it.

The model must preserve fast edit/build/test iteration, allow constrained autonomy, support sub-agents without multiplying privilege, prevent ambient long-lived authority, make delegation explicit and reviewable, keep authority transitions auditable, and support revocation, expiry, and renewal.

## Core concepts

**Mission** — the top-level unit of delegated work: objective, permitted actors, repository and artifact scope, allowed tasks, resource and time budget, escalation rules, approval conditions. Not itself a credential or runtime token; a Clyde-managed policy object. One active mission per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace)).

**Capability lease** — a time-bounded grant permitting an actor to read and edit selected paths, request specific typed tasks, inspect logs and artifacts, spawn a limited number of sub-agents, and repeat approved inner-loop actions up to a budget. A lease never implies raw credential access, unrestricted shell access, unbounded network, or authority outside the mission scope.

**Actor** — a human developer on the admin channel authenticated by `SO_PEERCRED`; an agent running outside Clyde or a CI job holding a session token; a coding agent Clyde hosts; a derived sub-agent; or a trusted automation component acting through Clyde.

Actors hold no ambient power **within Clyde**. That is a claim about what Clyde grants, not about what an actor may already have: a human developer plainly has ambient power over their own machine, and so does an agent Clyde does not host. The lease bounds what Clyde will do on their behalf, and [posture](../terminology.md#posture) reports whether anything else bounds them.

**Derived lease** — issued to a sub-agent from a parent lease. Equal to or narrower than the parent, explicitly linked to it, independently auditable, independently revocable, separately budgeted.

**Escalation** — a request to act outside the current lease: broader repository scope, additional task types, different network policy, brokered external access, a publish or sign operation, or a longer duration or larger budget. It does not automatically succeed.

**Boundary crossing** — any requested action exceeding the mission or lease envelope: switching from no-network compile to dependency fetch, moving between repo subtrees, switching from synthetic identities to real credentials, exceeding the sub-agent limit, or pushing, signing, and publishing.

## Mission structure

Required: `mission_id`, `objective`, `initiator`, `primary_actor`, `scope`, `allowed_tasks`, `network_policy`, `credential_policy`, `approval_policy`, `budget`, `expiry`.

Recommended: `priority`, `parallelism_limit`, `artifact_retention_policy`, `allowed_subagent_roles`, `default_runtime_class`, `audit_level`, `stop_conditions`, `success_criteria`.

```yaml
mission_id: m-2026-02-15-auth-rate-limit
objective: Implement rate limiting for auth endpoints and update tests
initiator: human:andrew
primary_actor: agent:default
scope:
  repo_paths: [backend/auth, backend/tests/auth]
allowed_tasks: [workspace.edit, rust.check, rust.test.unit]
network_policy: none
credential_policy: none
approval_policy:
  auto_approve_within_lease: true
  requires_human_for: [rust.resolve-deps, broader_path_access, git.push]
budget:
  max_duration: 45m
  max_task_executions: 40
  max_parallel_subagents: 2
  max_cpu_minutes: 60
expiry: 2026-02-15T18:00:00Z
stop_conditions: [success, escalation_required, budget_exhausted]
success_criteria: [rust.check passes, auth unit tests pass]
```

## Lease structure

Smaller and more operational than a mission.

Required: `lease_id`, `mission_id`, `actor`, `issued_by`, `issued_at`, `expires_at`, `repo_scope`, `task_scope`, `network_scope`, `credential_scope`, `authority_flags`, `budget`.

Recommended: `parent_lease_id`, `subagent_limit`, `retry_limit`, `revocation_conditions`, `current_usage`, `purpose`.

```yaml
lease_id: l-2026-02-15-auth-rate-limit-main
mission_id: m-2026-02-15-auth-rate-limit
actor: agent:default
issued_by: clyde
expires_at: 2026-02-15T17:45:00Z
repo_scope: [backend/auth, backend/tests/auth]
task_scope: [workspace.edit, rust.check, rust.test.unit]
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
  max_cpu_minutes: 60
purpose: implement and validate auth rate limiting
```

## Invariants

| | |
|---|---|
| **I1. No ambient authority** | An actor gains nothing merely by existing in a session or process tree. |
| **I2. Lease required for action** | Every action with side effects is attributable to an active lease, through a session token bound to it or an operator authenticated on the admin socket. Process ancestry, user id, and self-declared identity confer nothing. |
| **I3. Derived leases cannot widen privilege** | A child never exceeds the parent in scope, task rights, network, credential access, duration, or authority. |
| **I4. Expiry is enforced** | Expired leases stop authorizing new actions. |
| **I5. Revocation is immediate for new work** | Revoked leases authorize no further edits, task requests, or sub-agent creation. |
| **I6. Credentials are brokered separately** | A lease may authorize *requesting* a brokered operation; it never contains raw credentials. |
| **I6b. No actor may approve its own escalation** | An actor cannot grant, consume, or modify an approval. Approvals happen on a channel unreachable from any actor environment ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)). |
| **I7. Task execution remains policy-bound** | A lease authorizes *asking* for a task, not bypassing the task policy engine. |
| **I8. Audit linkage is preserved** | Every task, artifact, escalation, sub-agent, and privileged action traces back to mission, lease, actor, and approval record where applicable. |

## Mission lifecycle

1. **Creation** — by a human directly, by Clyde from a human instruction plus policy defaults, or by a trusted automation flow. Inputs: a natural-language goal, a repo or subsystem target, a risk profile, optional budget preferences. Outputs: a proposed charter, a risk summary, and the approval requirements.
2. **Approval and issuance** — Clyde evaluates against policy. Outcomes: auto-approved, human approval required, denied, or a revised mission suggested. On approval Clyde issues the primary lease. The proposal is shown as the exact envelope that will be issued; no field is decided after approval.
3. **Autonomous execution** — within the lease, the actor reads and edits allowed files, requests approved typed tasks, inspects artifacts, repeats safe inner-loop work, and creates derived sub-agents if allowed.
4. **Escalation or renewal** — at a boundary, the actor may request lease renewal, lease expansion, a new derived lease, a brokered privileged operation, or a replacement mission.
5. **Completion** — success criteria met, the human ends it, the budget is exhausted, expiry occurs without renewal, or policy denies further escalation.
6. **Closeout, in one transaction** — revoke active leases and their session tokens; tear down actor sandboxes and terminate derived actors; delete the mission's writable build cache; compute and store the closing workspace diff; preserve audit and artifacts per policy; summarize changes, task history, escalations, and egress attempts.

## Lease lifecycle

**Issuance** follows mission approval and policy evaluation. **Activation** binds the lease to an actor session. Where Clyde hosts the actor, attachment means creating the workspace-environment sandbox with mounts derived from the lease scope, issuing a session token into it, and starting the actor process there — so the lease's scope is realised as a mount table, not only as a policy record. Where Clyde does not, activation issues a token or admits the operator, and the scope is a policy record enforced at admission and recorded in the closing diff.

**Use** consumes budget: an edit batch, a task execution, a sub-agent slot, CPU or wall-clock time. **Expiry** happens at `expires_at` or earlier if budget is exhausted. **Renewal** is by Clyde only and only if policy allows; it issues a *replacement* lease and marks the old one superseded, rather than mutating expiry in place, so the audit trail shows the extension as an event. **Revocation** may come from the human, the policy engine, supervisory automation, or incident response.

Three rules about renewal, each of which was a bug before it was a rule:

- **The extension runs from the later of now and the current expiry.** An unexpired lease loses none of its remaining time; an expired one — the ordinary case, since renewal exists for leases that have run out — gets a lease valid from now rather than one born expired.
- **The mission's expiry moves with the lease.** They are two records of one envelope: a lease outliving the mission is authority nobody approved, and a mission expiring before its lease refuses work the lease still permits.
- **A terminal mission is not renewable, and neither is a revoked lease.** Closing or revoking a mission revokes its leases, so renewal must refuse rather than issue an active replacement for one — otherwise revocation is undoable by an operator who types `renew`, and revocation that can be undone is not revocation.

## Budgets

Budgets limit both accidental runaway loops and malicious persistence: wall-clock duration, task executions, retries, sub-agents, CPU minutes, memory ceiling, network egress, artifact retention size.

They keep autonomous work bounded, reduce surprise compute cost, prevent endless loops, surface when work needs re-approval, and constrain abuse if a driver behaves badly. Consumption is recorded **before** a task starts, not after it completes, so a crashed daemon cannot lose the charge.

## Approvals

**Auto-approved within a lease:** editing allowed files; `workspace.edit` on lease-scoped code-manipulation scripts; `rust.check`; `rust.test.unit`; rerunning failed unit tests; inspecting logs and artifacts; creating one allowed narrow sub-agent.

**Requiring approval or a policy gate:** `rust.resolve-deps`; private dependency fetch; broader filesystem scope; external network beyond policy; real browser identities or cookies; `git.push`; signing; publishing.

**Granularity:** approve once, approve for mission, approve for session, or deny and suggest a narrower alternative. Every decision binds to a digest of the exact normalised request; a request differing in any authority-relevant field does not match and needs a new approval. `ApproveOnce` consumption is single-use and transactional with the action it authorises.

An escalation request carries the current mission and lease id, the requested capability, the reason, the affected scope, the expected duration, and any safer alternatives:

```yaml
mission_id: m-2026-02-15-auth-rate-limit
lease_id: l-2026-02-15-auth-rate-limit-main
request:
  type: task_addition
  capability: rust.resolve-deps
  reason: new crate was introduced in Cargo.lock
  requested_network: rust-registry
  requested_credentials: none
  affected_scope: [backend/]
  duration: 10m
alternatives:
  - vendor dependency bundle manually
  - remove new dependency
```

## Failure and safety behaviour

**On lease expiry:** stop accepting new actions; let running tasks complete or terminate per policy; notify the human and the actor; offer renewal if allowed.

**On revocation:** block further actions immediately; terminate or quarantine sub-agents; prevent further task requests; preserve logs for review.

**On policy violation:** deny, log the request, explain the denial in structured terms, and suggest an allowed alternative where one exists.

**On suspected compromise:** revoke all leases in the mission, suspend artifact movement, freeze publish operations, and retain forensic logs and snapshots.

## Relationship to task policy

The mission and lease model sits **above** task policy and does not replace it.

- **Mission** — the work objective and the broad autonomy envelope
- **Lease** — a particular actor's temporary authority inside that envelope
- **Task policy** — how a specific task runs: runtime, mounts, network, credentials, limits

A lease can authorize `rust.check`; the [task policy](tasks-and-policy.md) still decides snapshot behaviour, runtime class, network denial, writable outputs, and resource limits. That separation is what prevents a broad lease being converted into arbitrary execution semantics.

## API sketch

```text
create_mission(objective, scope, preferred_tasks, duration)
run_task(lease_id, task_type, path)
request_escalation(lease_id, capability, reason, requested_scope, requested_network)
request_subagent(parent_lease_id, purpose, scope, tasks, duration)   # warden only
```

## Implementation guidance

**Builder.** One mission per unit of work — a conversation, a CI run, or a session at a terminal; one primary lease usable from either the operator or the actor surface; simple budgets (duration, task count, cache size); explicit approvals for network and publish, recorded as confirmations where the requester is the decider.

**Sandbox.** One mission per agent conversation; a single level of sub-agent leases with derived budgets; escalation as an actor-facing operation.

**Later.** Mission templates; policy-driven default missions by repo type; automatic renewal suggestions; richer quotas and analytics; reusable approval policies; multi-agent orchestration and hierarchical mission graphs; delegated review agents; enterprise policy controls.
