# Clyde Next: Sequence Flows and Interaction Scenarios

## Purpose

This document describes the major interaction and sequence flows for Clyde Next.

It shows how humans, agents, missions, leases, task policies, sandboxes, artifacts, and brokers interact during normal development and during privilege boundary crossings.

This document is intended to make the high-level architecture and mission/lease model concrete enough to guide implementation and UX design.

It builds on:
- [terminology.md](terminology.md)
- [high-level-design.md](high-level-design.md)
- [mission-lease-model.md](mission-lease-model.md)
- [task-policy-matrix.md](task-policy-matrix.md)

> **Decision status.** These flows hold, with two mechanical changes from [decisions.md](decisions.md). Human steps ("Human -> Clyde: approve") happen on the admin channel, which is unreachable from any actor environment ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)). And `edit_files` / `read_code` steps are not API calls: the agent edits the mounts its lease permits directly, and Clyde records diffs at task and closeout boundaries ([D1](decisions.md#d1-the-coding-agent-runs-inside-a-clyde-managed-workspace-environment)).

## Participants

Using the terminology from [terminology.md](terminology.md), the flows below are mostly about:
- **actors**: the human, agent, or sub-agent doing work
- the **workspace**: the mutable project being changed
- **tasks**: the units of work being requested
- **policy**: the rules deciding whether and how tasks may run
- **environments**: the workspace, build, or broker contexts where work happens

The main participants in the flows below are:
- **Human**: the developer supervising the work
- **Agent**: the primary coding agent
- **Sub-agent**: a derived bounded agent, when used
- **Workspace**: mutable project files being edited
- **Clyde Control Plane**: policy, missions, leases, scheduling, approvals
- **Policy Engine**: resolves task and approval policy
- **Snapshot Manager**: seals workspace inputs for build execution
- **Sandbox Runtime**: runs tasks in isolated environments
- **Artifact Layer**: stores logs, outputs, bundles, traces, and provenance
- **Credential Broker**: performs git/sign/publish operations without exposing raw credentials

## Reading Guide

Each scenario includes:
- a short intent description
- a step-by-step sequence
- the key security properties being enforced
- the UX implications

The exact wire protocol is intentionally unspecified here. These are conceptual interaction flows.

## Scenario 1: Start a mission and issue a primary lease

### Intent
A human actor asks Clyde to implement a feature. Clyde creates a mission, proposes an autonomy envelope, gets approval, and issues a primary agent lease.

### Sequence
```text
Human -> Clyde: "Implement refresh-token rotation in backend auth"
Clyde -> Policy Engine: propose mission from request + repo defaults
Policy Engine -> Clyde: suggested mission envelope
Clyde -> Human: mission proposal
  - scope: backend/auth, backend/tests/auth
  - tasks: workspace.edit, rust.check, rust.test.unit, rust.test.integration.synthetic
  - network: none
  - credentials: none
  - duration: 45m
  - sub-agents: 1
Human -> Clyde: approve mission                      [admin channel]
Clyde -> Sandbox Runtime: create workspace environment for primary actor
  - lease edit paths mounted rw, other permitted paths ro, .git ro
  - workspace runtime root (no build toolchain)
  - actor socket + session token file (0400)
  - egress: model-api profile only
Clyde -> Agent: start agent process inside that environment
Clyde -> Agent: issue primary lease + session token
Clyde -> Audit Log: record mission + lease issuance + session binding
```

### Security properties
- the human approves a bounded autonomy envelope up front
- the agent gets a lease, not ambient shell power
- allowed actions are explicit before code execution starts

### UX notes
- the mission proposal should be easy to read and easy to narrow
- the human should approve outcomes and envelopes, not every loop iteration

## Scenario 2: Safe inner-loop edit / helper-edit / check / test iteration

### Intent
The actor works autonomously inside its lease, editing code, using low-authority edit helpers, and running safe offline tasks without repeated approval.

### Sequence
```text
Agent: read and edit files directly within its mounted scope
  - out-of-scope writes fail at the kernel; no policy call involved
  - codemods and helper scripts run in the agent's own environment (T1)

Agent -> Clyde: run_task(type=rust.check, path=backend/auth)  [token-authenticated]
Clyde -> Policy Engine: resolve rust.check policy
Policy Engine -> Clyde: T2 / R3 / no-network / no-credentials
Clyde -> Snapshot Manager: snapshot build closure of backend/auth
Snapshot Manager -> Clyde: snapshot id (records any out-of-lease read-only paths)
Clyde -> Sandbox Runtime: launch rust.check with snapshot + policy
Sandbox Runtime -> Artifact Layer: stream logs, outputs, provenance
Sandbox Runtime -> Clyde: task result
Clyde -> Agent: task status + logs reference

Agent -> Clyde: run_task(type=rust.test.unit, path=backend/auth)
Clyde -> Policy Engine: resolve rust.test.unit policy
Clyde -> Snapshot Manager: snapshot build closure of current subtree
Clyde -> Sandbox Runtime: run unit tests (mission cache reused, warm)
Sandbox Runtime -> Artifact Layer: logs + coverage + failure artifacts
Sandbox Runtime -> Clyde: result + failure classification
Clyde -> Agent: logs + failure summary

Agent: edit files to fix failures
... repeat until pass or boundary reached ...
```

### Security properties
- editing happens in a low-authority environment with a live scoped workspace mount
- that environment does not carry the project build/test toolchain, so project code cannot execute there
- build and test use sealed snapshots, not live mutable mounts
- no egress or credentials are available to build and test at all
- each task is attributable to mission + lease + actor + session

### UX notes
- this should feel fast and nearly continuous
- one-off codemods and edit helpers should be cheap to run
- the agent should see policy, logs, and failure artifacts together
- the human should be able to watch but not be required to intervene

## Scenario 3: Missing dependency triggers escalation

### Intent
A normal safe loop hits a boundary: the agent needs dependency fetch authority that is outside the current no-network lease.

### Sequence
```text
Agent -> Clyde: run_task(type=rust.check, path=backend/auth, lease=...)
Clyde -> Sandbox Runtime: run rust.check in no-network sandbox
Sandbox Runtime -> Clyde: failure (missing dependency inputs)
Clyde -> Agent: task failed, dependency resolution required

Agent -> Clyde: request_escalation(
  capability=rust.resolve-deps,
  reason="new crate introduced in Cargo.lock",
  scope=backend/
)
Clyde -> Policy Engine: evaluate escalation
Policy Engine -> Clyde: allowed only with approval, registry-only network
Clyde -> Human: escalation prompt                     [admin channel]
  - task: rust.resolve-deps
  - input: manifests + lockfile only, no application source
  - egress profile: rust-registry, with the exact host allowlist shown
  - lockfile change: 4 additions, 0 source changes
  - credentials: none
  - outputs: dependency bundle only
  - caveat: allowlisting is by destination, not content
Human -> Clyde: approve once for mission
Clyde -> Agent: side lease issued for rust.resolve-deps, bound to the approval digest
```

### Follow-on execution
```text
Agent -> Clyde: run_task(type=rust.resolve-deps, path=backend/, lease=...)
Clyde -> Snapshot Manager: snapshot lockfiles + config
Clyde -> Sandbox Runtime: launch fetch sandbox (microVM), loopback-only netns
Clyde -> Egress Proxy: activate rust-registry allowlist for this task
Sandbox Runtime -> Artifact Layer: dependency bundle + fetch manifest (all attempts logged)
Sandbox Runtime -> Clyde: success
Clyde -> Agent: dependency bundle available
Agent -> Clyde: rerun rust.check
```

### Security properties
- compile and dependency fetch remain separate
- broader network is requested explicitly rather than silently inherited
- outputs are limited to dependency artifacts, not a general mutable workspace

### UX notes
- the prompt should explain why the previous safe profile failed
- Clyde should suggest the narrowest acceptable escalation

## Scenario 3b: New dependency code triggers a pre-execution check

### Intent
A fetch succeeds, but the new lockfile brings a crate that executes code at build time. The build does not proceed until the human has seen that.

### Sequence
```text
Agent -> Clyde: run_task(type=rust.check, path=backend/auth)
Clyde -> Policy Engine: resolve rust.check policy
Clyde -> Baseline Store: load confirmed baseline for (workspace, rust.check, backend/auth)
Clyde -> Inventory Check: recompute code-execution inventory from lockfile + bundle
Inventory Check -> Clyde: drift
  + serde_derive_internals 0.29.1  (new proc-macro crate)
  ~ ring 0.17.8 -> 0.17.9          (build.rs content changed)
Clyde -> Agent: task refused, inventory confirmation required
Clyde -> Human: drift prompt                          [admin channel]
  - 2 changes to code that will execute during your build
  - diff shown against the pinned baseline, not the whole baseline
Human -> Clyde: confirm new inventory
Clyde -> Baseline Store: amend baseline, record confirming actor
Agent -> Clyde: run_task(type=rust.check, path=backend/auth)   # now proceeds
```

### Security properties
- the check runs before the sandbox starts, so new dependency code is surfaced before it executes
- a same-version content change is reported distinctly from a version upgrade, because it is a tampering signal rather than a normal one
- the human reviews a diff, not a whole inventory, so the prompt stays readable as the dependency graph grows

### UX notes
- path drift and code-execution drift should look different: the first is common and low-signal, the second is rare and the reason this control exists
- if these prompts become routine the control has failed, so prompt frequency is worth measuring against a real repository

## Scenario 4: Sub-agent creation under a derived lease

### Intent
The primary agent wants parallel work. Clyde allows a sub-agent, but only by issuing a narrower derived lease.

### Sequence
```text
Agent -> Clyde: request_subagent(
  purpose="update frontend login flow",
  scope=frontend/login,
  tasks=[web.build, browser.test.synthetic],
  duration=15m
)
Clyde -> Policy Engine: validate against mission + parent lease
Policy Engine -> Clyde: allowed, narrower scope than parent
Clyde -> Sub-agent: create derived actor
Clyde -> Sub-agent: issue derived lease
Clyde -> Audit Log: record parent-child relationship

Clyde -> Sandbox Runtime: create narrower workspace environment for sub-agent
  - only frontend/login mounted rw
Sub-agent: edits files directly within that narrower mount scope

Sub-agent -> Clyde: run_task(type=web.build, path=frontend/login, lease=derived)
Clyde -> Snapshot Manager: create scoped snapshot
Clyde -> Sandbox Runtime: run build in isolated sandbox
Sandbox Runtime -> Artifact Layer: build logs/assets
Clyde -> Sub-agent: result

Sub-agent -> Clyde: run_task(type=browser.test.synthetic, path=frontend/login, lease=derived)
Clyde -> Sandbox Runtime: run browser test with synthetic identity
Sandbox Runtime -> Artifact Layer: screenshots/traces/logs
Clyde -> Sub-agent: result

Sub-agent -> Clyde: summarize status
Clyde -> Agent: merge sub-agent results into main conversation/task graph
```

### Security properties
- the sub-agent cannot exceed the parent's scope
- the sub-agent never inherits ambient authority by process ancestry alone
- sub-agent activity is separately auditable and revocable

### UX notes
- the human may not need to know every sub-agent detail in real time
- the UI should still make it possible to inspect who did what and under which lease

## Scenario 5: Integration testing with synthetic services

### Intent
The agent needs more than unit tests, but Clyde still prefers synthetic infrastructure over real credentials or unrestricted network.

### Sequence
```text
Agent -> Clyde: run_task(type=rust.test.integration.synthetic, path=backend/, lease=...)
Clyde -> Policy Engine: resolve synthetic integration profile
Policy Engine -> Clyde: synthetic network only, no credentials
Clyde -> Snapshot Manager: create snapshot
Clyde -> Sandbox Runtime: launch integration sandbox
Clyde -> Sandbox Runtime: attach synthetic Postgres, fake SMTP, fake OAuth
Sandbox Runtime -> Artifact Layer: logs + traces + db snapshots if needed
Sandbox Runtime -> Clyde: result
Clyde -> Agent: results available
```

### Security properties
- integration behavior is tested without real external identities
- supporting services are ephemeral and internal-only
- no host or developer credentials are needed

### UX notes
- Clyde should advertise synthetic options before external ones
- the agent should be able to reason about available synthetic profiles

## Scenario 6: Request real external test access

### Intent
A test genuinely requires a real preview or staging service. Clyde handles this as a controlled boundary crossing.

### Sequence
```text
Agent -> Clyde: request_escalation(
  capability=browser.test.external,
  reason="must validate against staging OAuth callback behavior",
  scope=frontend/login,
  requested_network=staging-oauth-only
)
Clyde -> Policy Engine: evaluate request
Policy Engine -> Clyde: human approval required, scoped test identity allowed
Clyde -> Human: escalation prompt
  - task: browser.test.external
  - destination: staging-oauth.example.com
  - identity: short-lived test identity only
  - browser state: isolated
  - developer cookies/profile: not allowed
Human -> Clyde: approve
Clyde -> Credential Broker: mint scoped test credential if needed
Credential Broker -> Clyde: scoped token reference
Clyde -> Agent: temporary authority granted

Agent -> Clyde: run_task(type=browser.test.external, lease=...)
Clyde -> Sandbox Runtime: launch isolated browser sandbox
Clyde -> Sandbox Runtime: attach scoped token, isolated browser profile
Sandbox Runtime -> Artifact Layer: screenshots, traces, logs
Sandbox Runtime -> Clyde: result
Clyde -> Agent: results
```

### Security properties
- real external access is scoped by destination and identity
- the developer's personal browser state is never reused
- the token is short-lived and purpose-bound

### UX notes
- prompts should explain why synthetic options were insufficient
- the approval must be understandable, not a generic "allow network?"

## Scenario 7: Human review of task history and policy use

### Intent
The human wants to understand what the agent actually did during an autonomous mission.

### Sequence
```text
Human -> Clyde: show mission status
Clyde -> Artifact Layer: collect task records, outputs, logs
Clyde -> Audit Log: collect edits, leases, sub-agents, escalations
Clyde -> Human: mission summary
  - files changed
  - tasks executed
  - passes/failures
  - sub-agents created
  - escalations requested
  - approvals granted
  - current budget usage
```

### Security properties
- autonomous work is observable after the fact
- every meaningful action can be traced to lease + task + artifact output

### UX notes
- this is crucial for trust in autonomous operation
- the summary should be much higher-level than raw sandbox logs, with drill-down available

## Scenario 8: Prepare commit without push authority

### Intent
The agent finishes work and prepares a commit candidate, but does not automatically gain push rights.

### Sequence
```text
Agent -> Clyde: request operation git.commit.prepare
Clyde -> Workspace: compute scoped diff and commit proposal
Clyde -> Artifact Layer: store commit metadata and diff summary
Clyde -> Human: review proposed commit
Human -> Clyde: approve commit preparation
Clyde -> Workspace or control plane: create local commit object if policy allows
Clyde -> Audit Log: record commit preparation
```

### Security properties
- commit preparation is separated from remote authority
- the human reviews proposed changes before external publication steps

### UX notes
- this should feel like a natural end to an agent mission
- Clyde should summarize evidence: passing tasks, open risks, and unapproved escalations

## Scenario 9: Brokered push to a feature branch

### Intent
A feature is ready. Clyde performs a push through the credential broker rather than exposing Git credentials to the agent or workspace.

### Sequence
```text
Agent -> Clyde: request_publish(action=git.push, branch=feature/refresh-token-rotation)
Clyde -> Policy Engine: check mission/lease rights and branch policy
Policy Engine -> Clyde: human approval required
Clyde -> Human: push prompt
  - branch: feature/refresh-token-rotation
  - remote: origin
  - commit: abc1234
  - actor: agent:default
Human -> Clyde: approve
Clyde -> Credential Broker: push commit abc1234 to origin/feature/refresh-token-rotation
Credential Broker: verify approval digest independently
Credential Broker: create sanitised temp repo, fetch commit with hooks/config disabled
Credential Broker: verify commit id + tree match the approval, then push
Credential Broker -> Clyde: push success
Clyde -> Audit Log: record brokered push
Clyde -> Human + Agent: branch updated
```

### Security properties
- no SSH socket or long-lived Git token is mounted into build or agent environments
- the broker executes the privileged effect outside untrusted code execution sandboxes
- the broker verifies the approval itself rather than trusting the caller
- repository hooks and repository git configuration cannot execute during the push

### UX notes
- prompts should show exact destination and commit id
- branch policy can be stricter for shared or protected branches

## Scenario 10: Sign and publish release artifacts

### Intent
Release publication is handled as a separate authority flow from build and test execution.

### Sequence
```text
Human -> Clyde: publish release candidate X
Clyde -> Policy Engine: require artifact selection + approval + signing policy
Clyde -> Human: publish plan
  - artifact ids
  - source snapshot ids
  - build task ids
  - test evidence
  - destination registry
Human -> Clyde: approve
Clyde -> Credential Broker: sign selected manifest or digest
Credential Broker -> Artifact Layer: store signature
Clyde -> Credential Broker: publish selected artifact set
Credential Broker -> Clyde: publish result
Clyde -> Audit Log: record sign + publish chain
```

### Security properties
- signing happens over explicit digests/manifests
- the publish environment never re-executes untrusted build code
- provenance and approval linkage are preserved

### UX notes
- Clyde should make it obvious what evidence backs the release
- approval should happen over a release plan, not a vague publish button

## Scenario 11: Lease expiry during autonomous work

### Intent
An autonomous mission times out or consumes its budget.

### Sequence
```text
Clyde -> Agent: warning, lease expires in 5 minutes
Agent -> Clyde: optional renewal request
Clyde -> Policy Engine: evaluate renewal against mission policy
Policy Engine -> Clyde: renewal allowed or approval required
Clyde -> Human: renewal prompt if needed
Human -> Clyde: approve or deny
Clyde -> Agent: renewed lease or expiry notice
```

If the lease expires:
```text
Clyde -> Agent: lease expired
Clyde -> Sandbox Runtime: do not schedule new tasks under expired lease
Clyde -> Human: mission paused, renewal required
Clyde -> Audit Log: record expiry
```

### Security properties
- time-bounded autonomy is actually enforced
- long-lived implicit power does not accumulate

### UX notes
- warnings should arrive before hard expiry
- renewal should preserve context without silently extending authority forever

## Scenario 12: Mission revocation or suspected compromise

### Intent
The human or Clyde suspects something is wrong and revokes the mission.

### Sequence
```text
Human or Policy Engine -> Clyde: revoke mission
Clyde -> Audit Log: record revocation reason
Clyde -> Agent: lease revoked
Clyde -> Sub-agent(s): derived leases revoked
Clyde -> Sandbox Runtime: stop or quarantine running tasks per policy
Clyde -> Artifact Layer: preserve logs, snapshots, outputs for review
Clyde -> Credential Broker: freeze pending authority operations
Clyde -> Human: revocation complete, review package available
```

### Security properties
- revocation fans out across all derived actors
- publish and credential actions can be frozen quickly
- forensic evidence is retained

### UX notes
- the UI should make revocation easy and obvious
- Clyde should separate "stop work" from "discard evidence"

## Cross-Cutting Flow Rules

Across all scenarios, Clyde should enforce these rules:

1. **All privileged effects go through Clyde**
   - no direct credential channels to agents or build sandboxes

2. **All untrusted code execution uses task policy**
   - missions and leases do not redefine runtime isolation

3. **All meaningful actions are attributable**
   - mission id, lease id, session, actor, task id, artifact ids

4. **No actor can authorise itself**
   - approvals arrive only on a channel absent from every actor environment

5. **Boundary crossings interrupt autonomy**
   - safe loops continue, authority expansion stops for review

6. **Artifacts move across trust boundaries, not ambient process access**
   - outputs are explicit and controlled

## Suggested UX Surfaces

These flows suggest a few core surfaces in the Clyde UI.

### Conversation surface
- human goal
- agent plan
- escalation requests
- completion summaries

### Mission surface
- active objective
- scope
- remaining lease budget
- sub-agent count
- stop conditions

### Task surface
- running/completed tasks
- policy profile used
- logs, traces, artifacts
- blocked actions

### Approval surface
- pending escalations
- push/sign/publish requests
- renewal prompts
- policy explanations

### Audit surface
- mission timeline
- edits by actor
- derived lease graph
- artifact lineage

## Summary

These sequence flows show how Clyde Next can remain the sole authority and mediation layer without blocking practical agentic development.

The core pattern is consistent across scenarios:
- humans approve missions and boundary crossings
- agents operate quickly within leases
- task policies govern how untrusted execution actually runs
- sandboxes never inherit raw credentials
- artifacts and audits preserve traceability

This gives Clyde a concrete interaction model for secure, high-autonomy development rather than falling back to long-lived privileged workspaces or unrestricted agent shells.
