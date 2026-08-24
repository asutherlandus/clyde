# Clyde Next: High-Level Design

## Purpose

This document describes a high-level design for a clean-sheet Clyde, hereafter **Clyde Next**. It focuses on:
- overall architecture
- trust boundaries
- execution model
- user experience
- agentic coding workflows
- the interaction model between the human developer and coding agents inside a least-privilege system

It builds on the threat model in [problem-statement-threat-model.md](problem-statement-threat-model.md), the terminology in [terminology.md](terminology.md), and the requirements in [requirements.md](requirements.md).

## Design Thesis

Using the terminology defined in [terminology.md](terminology.md), Clyde Next can be described simply:

> An **actor** works on a **mission** in a **workspace**, asks to run a **task**, **policy** decides whether and how it may run, and the task runs in an appropriate **environment**.

Clyde Next should not be a single development container with a coding agent inside it.

Instead, it should be a **local or self-hosted development control plane** that coordinates multiple environments with different authority:
- a **workspace environment** for humans and agents to edit and prepare changes
- a **research environment** for web search, documentation reading, and research artifacts
- a **build environment** for fetch, build, test, and other project execution
- a **broker environment** for push, sign, publish, and other privileged external actions
- a controlled artifact and provenance layer between them

The primary design goal is to preserve the usefulness of agentic coding while ensuring that project execution never automatically inherits access to credentials, the full host filesystem, or unrestricted network access.

## Architectural Overview

At a high level, Clyde Next consists of a trusted control plane coordinating a small number of environments.

```text
+--------------------------------------------------------------+
|                      Human Developer                         |
|  editor / terminal / review UI / approvals / dashboards      |
+------------------------------+-------------------------------+
                               |
                               v
+--------------------------------------------------------------+
|                  Workspace Environment                        |
|  planner, coder, reviewer, repo editor, log/artifact viewer   |
|  low-authority code-manipulation support                       |
|  - mutable workspace                                           |
|  - no raw credentials                                          |
|  - no direct project build/test execution                      |
+------------------------------+-------------------------------+
                               |
                               v
+--------------------------------------------------------------+
|                     Clyde Control Plane                       |
|  policy engine | task scheduler | snapshot manager            |
|  sandbox manager | audit log | artifact manager               |
|  approval engine | broker coordinator                         |
+-----------+----------------------+-------------------+--------+
            |                      |                   |
            v                      v                   v
+---------------------+  +---------------------+  +--------------------+
| Build Environment   |  | Broker Environment  |  | Artifact Layer     |
| build/test/fetch    |  | git/ssh/sign/push   |  | snapshots/logs/    |
| isolated execution  |  | brokered operations |  | outputs/provenance |
+---------------------+  +---------------------+  +--------------------+
```

## Key Architectural Decision

The most important architectural decision is this:

> **The actor does not execute project build/test code directly from the workspace. The actor asks Clyde to run tasks under policy in the appropriate environment.**

That decision drives most of the system design.

It means:
- the coding agent remains productive, including for one-off codemods and helper scripts in the workspace environment
- the system can preserve a good conversational coding workflow
- untrusted project execution still runs in controlled build environments
- capabilities can be reviewed, approved, denied, and audited
- the human developer remains in control of privilege escalation

## Resolving the Centralization vs Autonomy Tension

Clyde Next should resolve the apparent conflict between "Clyde is the single point of contact" and "agents need autonomy" by redefining what centralization means.

### Clyde as sole authority, not sole actor
Clyde should be the only path to privileged effects, but not the only component that can initiate work.

Humans, primary agents, and sub-agents may all:
- read code
- edit code
- request tasks
- inspect logs and artifacts
- request escalation

But they must do so through Clyde-managed interfaces.

This means Clyde remains the sole authority for:
- policy enforcement
- capability issuance
- task scheduling
- sandbox creation
- credential use
- approvals
- audit logging
- artifact movement across trust boundaries

### Constrained autonomy
Agentic development should be supported through constrained autonomy rather than unrestricted shell access.

A human should be able to delegate a bounded mission such as:
- implement feature X in `backend/auth`
- update tests in `backend/tests/auth`
- allow `workspace.edit`, `rust.check`, and `rust.test.unit`
- do not use network or credentials
- stop and ask if dependency resolution or broader access is needed

Within that envelope, the agent should be able to iterate rapidly without repeated human confirmation.

### Mission charters
The high-level unit of delegation should be a mission charter.

A mission charter defines:
- objective
- repository scope
- allowed task types
- network policy
- credential policy
- maximum duration
- maximum parallelism
- escalation rules

Example:
```yaml
mission: implement-auth-rate-limits
scope:
  - backend/auth
  - backend/tests/auth
allowed_tasks:
  - workspace.edit
  - rust.check
  - rust.test.unit
network: none
credentials: none
parallel_subagents: 2
duration: 45m
escalation:
  - rust.resolve-deps
  - broader path access
  - publish operations
```

### Capability leases
The concrete mechanism for autonomy should be a capability lease.

A capability lease is a time-bounded grant that allows an agent to perform actions within the mission charter, for example:
- edit these paths
- run these task types
- spawn up to N sub-agents
- repeat safe inner-loop actions up to a defined budget

A lease should always be:
- scoped
- temporary
- auditable
- revocable
- non-transferable except through explicit Clyde mediation

### Derived sub-agent charters
Sub-agents should not be raw child processes with inherited ambient authority.
They should be explicitly created by Clyde as derived capability holders.

A parent agent can request a sub-agent, but Clyde issues the sub-agent's charter. That charter must be equal to or narrower than the parent's permissions.

Example:
- parent agent scope: `frontend/` and `backend/auth/`
- sub-agent A scope: `frontend/login/`
- sub-agent B scope: `backend/auth/`
- sub-agent C scope: `docs/`

No sub-agent may expand filesystem, network, or credential access beyond what Clyde grants.

### Pre-authorized inner loops
To keep the system usable, Clyde must allow pre-authorized safe loops inside a mission without requiring repeated human approval.

Examples of actions that may be repeated autonomously within policy:
- edit files in allowed paths
- snapshot the workspace subtree
- run `rust.check`
- run `rust.test.unit`
- inspect logs
- retry after fixes

These loops remain safe because they happen inside isolated sandboxes with no ambient credentials and no unauthorized network access.

### Human approval at boundary crossings
The human developer should supervise autonomy, not micromanage each step.

Human approval should be required when crossing boundaries such as:
- enabling broader network access
- accessing private dependency sources
- expanding repo scope
- requesting real external identities or credentials
- pushing, signing, or publishing

This preserves Clyde as the single trusted point of contact while still allowing fast iterative agent work.

## Core Components

## 1. Developer Workspace

The developer workspace is the place where humans and agents collaborate on source code.

### Responsibilities
- host the mutable working tree
- support interactive editing
- support code search and navigation
- render logs, diagnostics, and artifacts
- stage changes before execution
- provide approval prompts and explain policy decisions

### Characteristics
- read/write access to the checked-out repository
- no direct mount of signing keys into execution environments
- separate from untrusted build/test environments
- paired with separate low-authority edit-execution support for ad hoc code-manipulation scripts
- can be local-first, with optional remote/self-hosted variants later

### Design intent
The workspace is where code is authored and reviewed. It is **not** where untrusted project execution should happen by default.

Clyde should still support agent-authored one-off scripts for editing work, but only as a mode of `workspace.edit` with these strict properties:
- separate image/runtime from build/test/fetch sandboxes
- tooling for text and code manipulation
- no full project build toolchain available
- no raw credentials
- no direct publish/sign authority

## 2. Workspace Environment

The workspace environment provides the AI coding experience.

### Agent roles
Clyde Next should model the agent as a set of logical capabilities rather than a single omnipotent shell user.

#### Planner
Can:
- read source code
- inspect logs and results
- suggest task graphs
- ask for additional capabilities

Cannot:
- execute project code directly
- access credentials directly
- push or sign by default

#### Editor
Can:
- modify files in the workspace
- generate patches
- refactor code
- create tests and configuration changes
- use low-authority helper execution for codemods, search/replace, structured rewrites, and similar editing work

Cannot:
- bypass task policy
- directly invoke arbitrary privileged execution
- use the full project build/test toolchain
- execute repo-defined build/test/install workflows as if it were a dev container
- access credentials, publish authority, or unrestricted network

#### Executor
Can:
- request task execution through Clyde
- choose from available typed tasks
- retrieve logs and artifacts
- propose reruns with different profiles

Cannot:
- escape policy constraints
- open raw credential channels

#### Publisher
Should be separate and often human-gated.
Can:
- request commit, push, sign, or publish operations through brokered flows

Cannot:
- directly handle private keys or raw long-lived tokens

### Why this matters
This separation gives Clyde a much better security story than "agent has shell in a dev container." It also maps well to how human developers naturally think:
- think
- edit
- run checks
- review
- publish

## 3. Control Plane

The control plane is the trusted brain of Clyde Next.

### Responsibilities
- receive task requests from humans and agents
- resolve task type and policy
- prepare workspace snapshots
- decide runtime placement
- launch and monitor sandboxes
- collect outputs and logs
- broker approvals
- call credential services when needed
- record audit events

### Important property
The control plane is the only component allowed to connect the major parts of the system. Actors and build environments do not connect to each other arbitrarily.

## 4. Policy Engine

The policy engine maps a requested action to an execution profile.

### Examples
- `rust.resolve-deps`
- `rust.check`
- `rust.test.unit`
- `web.install`
- `web.build`
- `browser.test`
- `git.push`
- `artifact.sign`

Each policy defines:
- runtime type
- allowed filesystem inputs
- writable outputs
- network permissions
- allowed secret/credential access
- resource limits
- approval requirements
- audit verbosity

### Example conceptual policy
```yaml
task: rust.check
runtime: microvm
source:
  snapshot: repo:/backend
  mode: ro
dependencies:
  cargo_cache: ro
network: none
credentials: none
outputs:
  - /build-out
scratch: tmpfs
approval: none
```

## 5. Snapshot Manager

The snapshot manager creates immutable task inputs from the mutable workspace.

### Why snapshots matter
Without snapshots, a build sandbox and an editing agent are fighting over the same mutable tree. That makes policy weaker and provenance harder.

### Responsibilities
- snapshot selected repo paths
- include or exclude generated files by policy
- support subtree snapshots for monorepos
- create stable content identifiers
- allow later reproduction of a task from known inputs

### Result
The agent edits the live workspace. The execution task runs against a sealed snapshot.

## 6. Build Environment

This environment runs all code that should be treated as hostile.

### Includes
- `cargo check`, `cargo build`, `cargo test`
- proc macros and `build.rs`
- npm/pnpm/yarn installs
- frontend builds
- ORM/codegen steps
- browser and integration tests
- project-specific scripts

### Runtime model
Clyde Next should support more than one runtime class.

#### Trusted tool runner
For low-risk built-in tools that do not execute project code.
Examples:
- formatter wrappers for trusted binaries
- workspace indexing
- static inspection

#### Hardened container
For medium-risk tasks or early implementation phases.

#### MicroVM or equivalent strong sandbox
For high-risk tasks such as:
- dependency install with lifecycle scripts
- Rust compile/test
- browser test
- arbitrary repo commands

### Sandbox properties
- ephemeral
- no access to raw credentials
- read-only inputs wherever possible
- write-only outputs and scratch
- network denied by default
- destroyed after task completion unless explicitly retained for debugging

## 7. Broker Environment

The broker environment provides capability without direct credential exposure.

### Supported capability types
- git fetch
- git push
- SSH-backed repo access
- artifact signing
- release publishing
- scoped cloud/API access

### Interaction model
The agent or human does not receive an SSH socket or GPG private key.
Instead, they request an operation such as:
- fetch repository X at ref Y
- push commit Z to branch B
- sign manifest M
- publish artifact A to registry R

The broker environment performs the action if policy and approval allow it.

### Design consequence
This cleanly separates **code execution** from **authority**.

## 8. Artifact Layer

The artifact layer stores and moves outputs across trust boundaries.

### Stores
- source snapshots
- dependency bundles
- build outputs
- test logs
- screenshots and traces
- SBOMs
- provenance metadata

### Why it matters
Artifacts should move between tasks, not live processes with broad access. This allows Clyde to preserve isolation while still supporting iterative workflows.

## Usage Model

## Primary user experience
From the user's point of view, Clyde Next should feel like:
- an AI coding assistant
- a secure task runner
- a review and approval system
- a development environment dashboard

It should **not** feel like a maze of manual containers and shells.

## Human workflow model
The default human workflow should be:

1. Open project in Clyde-enabled workspace
2. Interact with one or more coding agents
3. Review proposed edits
4. Ask Clyde to run checks or tests
5. Inspect results
6. Approve elevated actions when needed
7. Commit, sign, push, or publish via brokered flows

## Agent workflow model
The default agent workflow should be:

1. Read relevant code and prior logs
2. Propose a plan
3. Edit the workspace
4. Request one or more typed tasks
5. Observe task output
6. Iterate until success or ask for escalation
7. Prepare a publish action for human approval

## Mission-oriented autonomy model

Clyde Next should present agent autonomy as a mission managed by Clyde rather than as a shell session with ambient power.

### Mission lifecycle
1. Human states a goal
2. Clyde or the agent proposes a mission charter
3. Human approves the mission envelope
4. Clyde issues a capability lease to the primary agent
5. Agent iterates autonomously within the lease
6. Agent requests escalation only when it hits a policy boundary
7. Human reviews results and approves authority transitions as needed

### What the human approves
The human should normally approve:
- the objective
- the scope of files or repo subtrees
- the families of tasks the agent may run
- whether sub-agents are allowed
- the time and resource budget
- the escalation policy

### What the agent can do autonomously inside the mission
The agent should normally be able to:
- edit code in allowed paths
- request default build and test tasks
- rerun those tasks after changes
- inspect artifacts and logs
- spawn bounded sub-agents if allowed by the mission
- summarize tradeoffs and request escalation when needed

### What requires escalation
The following should normally force an escalation or new mission approval:
- new dependency fetch outside pre-approved policy
- broader path access
- real external network access
- access to brokered credentials or real identities
- push, sign, or publish operations

This model keeps Clyde as the developer's one authority surface while enabling practical agentic loops.

## Interaction Design Between Human and Agent

The interaction design is the most important product behavior. Clyde Next should make the human-agent relationship explicit and policy-aware.

## Interaction principle 1: the agent is a collaborator, not a superuser

The agent should be highly capable but visibly constrained.

The UI should make it obvious:
- what the agent can do now
- what it is asking to do next
- which operations will execute untrusted code
- which operations require additional approval

## Interaction principle 2: execution requests should be conversational but structured

The user should be able to say:
- "Run Rust checks for the backend"
- "Test only the auth package"
- "Rebuild the frontend with network disabled"
- "Push this branch"

Internally, Clyde maps that request to a typed task and policy profile.

The agent should be able to say:
- "I want to run `rust.check` on `backend/` with the default no-network policy."
- "This repository uses a private git dependency. I need a `resolve-deps` task with brokered fetch access. Approve?"
- "Tests need external OAuth. I recommend switching to the synthetic identity profile rather than using your real credentials."

## Interaction principle 3: privilege escalation should be explicit, narrow, and understandable

When additional privilege is needed, Clyde should not present a vague prompt like:
- "Allow agent to access network?"

Instead it should present a structured request such as:
- task: `node.resolve-deps`
- repo path: `frontend/`
- network: `registry-proxy-only`
- credentials: `none`
- writable outputs: `dependency-bundle`
- reason: `download locked dependencies from approved mirror`

The human can then:
- approve once
- approve for session
- reject
- request a stricter alternative

## Interaction principle 4: human review remains the control point for authority transitions

The agent can do a lot without asking, including:
- editing files
- requesting default build/test tasks
- summarizing failures
- proposing commits

But transitions that grant authority should be reviewed by a human or an explicit policy gate, including:
- broader network access
- access to non-default repo paths
- use of production or staging credentials
- push to shared branches
- signing artifacts or commits
- publishing packages or images

## Interaction principle 5: logs and policy must be first-class in the UX

When a task runs, the user should not only see stdout/stderr. They should also see:
- which policy profile was used
- whether network was enabled
- whether credentials were accessible
- which snapshot was used
- which outputs were produced
- whether any blocked actions occurred

This helps humans and agents debug both code and policy.

## Concrete User Experience Model

## Workspace layout
A high-level Clyde interface could expose four primary panes or concepts:

### 1. Conversation pane
Where the human and agent discuss goals, plans, and results.

### 2. Workspace pane
Shows file diffs, editable code, and pending changes.

### 3. Task pane
Shows requested and running tasks, their policy profiles, logs, artifacts, and results.

### 4. Approval pane
Shows pending escalations and privileged operations.

This can be implemented in terminal UI, editor extension, or web UI later. The key is the model, not the specific presentation.

## Common interaction flows

### Flow A: routine coding loop
1. Human asks agent to implement a feature.
2. Agent reads code and proposes changes.
3. Agent edits files.
4. Agent optionally uses helper-driven `workspace.edit` for a one-off codemod or structured rewrite.
5. Agent requests `rust.check`.
6. Clyde snapshots inputs and runs tasks in isolated sandboxes.
7. Results return to task pane.
8. Agent fixes issues and reruns.
9. Human reviews final diff.

No special approvals are needed because the edit helper and typed tasks used default least-privilege profiles.

### Flow B: dependency resolution needed
1. Agent tries `rust.check`.
2. Clyde detects missing dependencies for a new lockfile state.
3. Agent requests `rust.resolve-deps`.
4. Clyde presents a prompt showing:
   - network limited to approved registry mirror
   - no credentials
   - outputs limited to dependency bundle/cache
5. Human approves.
6. Fetch completes.
7. Agent reruns `rust.check` with no network.

This keeps fetch and compile separate.

### Flow C: integration test requiring external-like services
1. Agent requests `rust.test.integration`.
2. Default profile provides synthetic database, fake SMTP, and fake OAuth.
3. Tests fail because config expects a real callback URL.
4. Agent proposes one of two options:
   - update tests to work with synthetic services
   - request a more permissive test profile
5. Human chooses.

The preferred UX nudges toward synthetic services first.

### Flow D: publish path
1. Agent completes implementation and prepares commit.
2. Human reviews diff and approves commit creation.
3. Agent requests `git.push` to a feature branch.
4. Clyde shows the exact operation and destination.
5. Human approves.
6. Credential broker performs push.
7. For a release, Clyde separately requires `artifact.sign` and `artifact.publish` approvals.

At no point does untrusted build code receive signing or push credentials.

## Agentic Coding Model

## Single-agent mode
This is the simplest initial model.

### Experience
- one primary coding agent per workspace
- strong iterative loop
- clear ownership of edits and task requests
- easy human review

### Best for
- solo developers
- small teams
- early product versions

## Multi-agent mode
Later, Clyde Next could support specialized agents.

### Example roles
- implementation agent
- test agent
- security review agent
- refactoring agent
- release preparation agent

### Important constraint
All agents still operate through the same control plane and policy framework. Multi-agent should not mean multiplied privilege.

### Sub-agent model
Sub-agents should be created as Clyde-issued derived actors rather than arbitrary child processes.

A parent agent requests a sub-agent for a narrow purpose. Clyde then:
- checks the parent lease
- narrows scope if needed
- issues a derived lease
- records the parent/child relationship in audit logs
- enforces separate budgets and revocation

This allows parallel work without breaking the trust model.

## Recommended agent behaviors

Clyde should encourage agent behavior patterns that align with least privilege.

### Good default behaviors
- prefer read/plan before edit
- prefer helper-driven `workspace.edit` for ad hoc code-manipulation scripts
- prefer narrow task scopes
- prefer subtree snapshots over whole-repo runs
- prefer no-network tasks
- prefer synthetic services over real credentials
- prefer brokered operations over raw secret access
- explain why escalation is needed

### Discouraged behaviors
- asking for broad shell access
- asking for general internet access when a registry mirror would suffice
- asking for developer credentials to make tests pass
- asking for persistent mutable mounts in build environments
- asking to reuse the same privileged environment across unrelated tasks

## Trust and Approval Model

## Capability levels
Clyde Next should define human-visible capability levels.

### Level 0: read and edit
- code read
- code write
- local reasoning
- no execution

### Level 1: safe execution
- trusted tools and low-authority edit utility scripts only
- no project build/test/install code execution
- no network
- no credentials
- no full project build toolchain in edit-helper execution

### Level 2: untrusted project execution
- build/test/codegen in isolated sandboxes
- default policy
- no network unless task type requires it
- no credentials

### Level 3: constrained external access
- registry-only fetch
- synthetic external systems
- short-lived scoped tokens where required
- approval or policy gate

### Level 4: authority operations
- git push
- sign
- publish
- production/staging credential use
- strong approval and audit requirements

This model helps both humans and agents reason about what is happening.

## Safe defaults for approvals
The system should auto-approve low-risk repeated actions only when they remain within a tightly defined profile, such as:
- `workspace.edit` for lease-scoped code-manipulation scripts
- `rust.check` on current repo subtree
- `rust.test.unit` with no network

It should not silently auto-approve:
- broader network scopes
- private dependency access without policy
- branch pushes outside allowed patterns
- signing or release publication

## Example CLI / API Usage Model

## CLI examples
```bash
# Open agent workspace
clyde workspace open

# Ask default agent to implement a change
clyde agent ask "Add rate limiting to the auth endpoint"

# Run helper-driven workspace editing
clyde workspace edit --path backend/auth -- python /tmp/codemod.py

# Run a standard typed task
clyde run rust.check --path backend/

# Resolve dependencies with registry-only profile
clyde run rust.resolve-deps --path backend/

# Request a push operation
clyde publish push --branch feature/rate-limits
```

## Agent-facing API examples
```text
read_code(paths=["backend/src/auth"])
edit_files(patch=...)
edit_files(mode="scripted", path="backend/auth", command="python /tmp/codemod.py")
run_task(type="rust.check", path="backend/")
get_task_logs(task_id="...")
request_capability(
  task="rust.resolve-deps",
  reason="new crate introduced in Cargo.lock",
  scope="registry-proxy-only",
  ttl="10m"
)
request_publish(action="git.push", branch="feature/rate-limits")
```

These interfaces make the agent productive without collapsing the trust model.

## Rust and Full-Stack Execution Model

## Rust path
The standard Rust loop should be:
1. edit in workspace
2. optionally use helper-driven `workspace.edit` for one-off codemods or batch edits
3. separate dependency resolution if needed
4. run `rust.check` in networkless sandbox
5. run unit tests in separate sandbox
6. run integration tests in synthetic network sandbox
7. package in separate sandbox
8. sign/push/publish via brokered flow

## Full-stack path
The standard full-stack loop should be:
1. edit backend/frontend/shared code
2. optionally use helper-driven `workspace.edit` for one-off codemods or config rewrites
3. run dependency fetch separately for Rust and JS ecosystems
4. run backend compile/test in networkless sandbox
5. run frontend build in networkless sandbox
6. run integration stack in synthetic network environment
7. run browser tests with isolated browser profile and synthetic identity
8. publish via brokered flow

## Why this high-level design is different

Compared with ordinary dev containers or hosted workspaces, Clyde Next is different in four important ways:

1. **task-based isolation instead of workspace-only isolation**
2. **brokered authority instead of mounted credentials**
3. **policy-aware agent interaction instead of unconstrained shell access**
4. **artifact movement across trust boundaries instead of shared mutable execution state**

These differences are what let Clyde preserve agentic productivity while taking the Rust and full-stack supply-chain threat model seriously.

## Initial Scope Recommendation

For an initial version, Clyde Next should focus on a narrow but powerful workflow:

### Must-have first slice
- one human developer
- one primary coding agent
- mutable workspace + low-authority scripted editing + typed task runner
- snapshot-based task inputs for build/test/fetch tasks
- isolated execution for `rust.check` and `rust.test.unit`
- no raw SSH/GPG mounting
- brokered git push
- approval UX for network and publish actions
- task logs and policy visibility

### Defer until later
- multi-agent collaboration
- complex remote execution fleets
- full release signing pipeline
- advanced provenance attestations
- enterprise policy federation

This gives Clyde Next a clear path to delivering meaningful security improvements without trying to solve every platform problem at once.

## Summary

Clyde Next should be designed as a **policy-driven control plane for agentic software development**, not merely as a better container wrapper.

The human and the coding agent should share a productive editing workspace, but all project execution should happen through typed, isolated, policy-governed tasks. Credentials should be brokered, not mounted. Publishing should be separated from building. The system should make privilege, policy, and approvals visible and understandable.

If done well, Clyde Next can give developers the speed of modern coding agents without requiring them to trust every build script, proc macro, npm hook, or generated command with their machine and credentials.
