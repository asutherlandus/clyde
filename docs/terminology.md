# Terminology

The canonical vocabulary for both products. Prefer these terms; define new ones here rather than locally.

## The core model

> An **actor** works on a **mission** in a **workspace**, asks to run a **task**, **policy** decides whether and how it may run, and the task runs in an appropriate **environment**.

| Term | Meaning |
|---|---|
| **Mission** | A bounded goal and autonomy envelope: objective, path scope, allowed work, time and resource limits, escalation boundaries. |
| **Actor** | A human or agent doing work. An actor holds no ambient authority within Clyde. |
| **Workspace** | The mutable project files being edited. Authoring, not the default place for hostile project execution. |
| **Task** | A named unit of work Clyde can reason about and control: fetch, build, test, push. |
| **Task request** | An actor asking Clyde to perform a task. |
| **Task run** | One concrete execution of a task. |
| **Task result** | The outcome of a run: status, exit code, diagnostics, log and artifact references, the policy and environment used. |
| **Policy** | The rules deciding whether and how a task may run — which actor, which mission, which environment, what network, what credentials, with or without approval. |
| **Environment** | The execution context where a task runs. |

Actors **request** tasks; Clyde **runs** them; runs produce **task results** and may produce **artifacts**; actors **spawn** sub-agents, not tasks; humans **approve** or **deny** boundary crossings.

## The two products

The MVP ships as two products ([D23](builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)), previously called Part 1 and Part 2.

**Builder** — the build pipeline: snapshots, baselines, typed build tasks, the fetch/compile split, brokered authority, and audit. Driven by a human, by CI, or by an agent running outside Clyde.

**Warden** — the agent harness: hosting a coding agent inside a workspace environment.

> Builder = what happens when code runs.
> Warden = who gets to ask.

The warden is named for what it holds custody of: the driver's **authority**. It is not a jailer, and the agent is not a prisoner — the agent is semi-trusted, careless rather than hostile, and the warden's job is to make its lease real rather than to confine an attacker. Hostile code is the builder's problem, and the builder answers it with an execution boundary.

The name deliberately avoids "sandbox", which in this codebase always means the isolation *mechanism* — a bubblewrap namespace or a Firecracker microVM — that both products use. The warden creates sandboxes; it is not one.

## Environments

Four environment types. Each has a distinct authority level and a distinct [runtime root](#execution).

### Workspace environment
Low-authority environment for editing and code manipulation, and where a Clyde-hosted coding agent runs. **A warden environment**: a builder-only deployment has none, and editing happens wherever the driver already lives.

Used for direct edits, patch application, codemods, batch and structured rewrites, one-off helper scripts, and hosting an actor. It uses the live mutable workspace, is scoped to allowed repo paths by mount topology, holds no raw credentials, has no egress but the model API allowlist, and carries **no project build toolchain**.

### Build environment
Isolated environment for project code execution: dependency fetch, build, test, code generation. Snapshot-based, isolated from the live workspace, and **treated as hostile** whenever project code may execute.

### Broker environment
Privileged environment for push, sign, publish, and similar authority-bearing operations. Performs privileged external effects, exposes no raw credentials to task execution, and never runs untrusted project code in the same context.

### Research environment
Read-oriented environment for web search, documentation reading, and research artifacts. Read-only workspace access, network per policy, no raw credentials, produces notes and summaries rather than edits. Post-MVP.

## Authority

| Term | Meaning |
|---|---|
| **Lease** | A time-bounded, scoped grant allowing a specific actor to act within a mission. A mission is the envelope; a lease is the active grant. |
| **Derived lease** | A lease issued to a sub-agent from a parent lease, and never wider than it in any dimension. |
| **Approval** | An explicit human decision allowing or denying a boundary crossing. |
| **Confirmation** | An approval where the requester and the decider are the same human. Not an authority boundary and not described as one; its value is that a human saw what was about to happen before it happened ([D2 amendment](builder/decisions.md#amendment-confirmation-semantics-when-the-operator-is-the-driver)). |
| **Boundary crossing** | A requested action outside the current approved mission or lease envelope. |
| **Escalation** | A request to act outside the current lease. It does not automatically succeed. |
| **Session token** | The capability binding a running actor process to a lease. Requests are authorised by the token, not by process ancestry, user id, or self-declared identity. |
| **Admin channel** | The human-only interface for missions and approvals. Never reachable from an actor's environment — that unreachability is what makes an approval mean something. |
| **Principal** | Who acted: a human operator on the admin channel, an external actor holding a session token, or an agent Clyde hosts. Every side-effecting record names the kind ([D25](builder/decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)). |
| **Broker** | A privileged service performing an authority-bearing action without exposing raw credentials to the actor or task. |
| **Safe inner loop** | The repeated actions an actor may perform inside an approved mission and lease without asking the human each time: edit, request a permitted task, inspect results, retry. |

## Posture

**Posture** is whether the driver can execute project code outside Clyde ([D26](builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

- **`enforcing`** — it cannot. Typed tasks are the only path to project execution.
- **`advisory`** — it can, and Clyde names the specific bypass.

Posture is derived and reported, never configured. It appears in `clyde doctor`, on every task run, and in mission review, so that the weaker mode is never the mode you are silently in.

## Execution

| Term | Meaning |
|---|---|
| **Snapshot** | An immutable copy of workspace inputs used by a build environment. Its scope is the build closure of the requested path, not the actor's edit scope: a build tool generally cannot compile a subtree without the surrounding manifests and path dependencies. |
| **Runtime root** | The read-only, content-pinned set of tools a task executes against. One per task family; a nix closure identified by store path in the MVP. The workspace runtime root deliberately contains no project build toolchain. |
| **Egress profile** | A named network reachability level attached to a task policy and bounded by a lease. The set is closed. `none` means a loopback-only namespace with no channel out; other profiles allow an allowlist through a Clyde-managed proxy. |
| **Mission cache** | The writable build cache created for one mission and destroyed when it closes. Keeps the inner loop fast without letting build state persist across unrelated work. |
| **Access baseline** | The confirmed record of what a build task may read and which dependencies execute code during it. The repository side has two tiers: **subtree grants** for first-party code inside the mission's approved scope, and **file pins** for anything outside it. The dependency side is the inventory of build scripts and proc macros, pinned by crate, version, and content hash. A human confirms it once; afterwards only change raises a prompt. |
| **Drift** | Build access diverging from the baseline: reaching outside the granted subtrees, a new build script, a changed build script, or the same dependency version with different content. Editing the project is **not** drift — files created, renamed, or removed inside a granted subtree are the work the mission authorised. |
| **Artifact** | A stored input or output moving between environments through explicit channels: snapshots, dependency bundles, build outputs, logs, traces, signatures. A task result describes what happened; artifacts are what it produced. |

## Terms to avoid

Prefer **environment** over *plane*, *runtime*, or *runtime class* when explaining the core model. Prefer **task** over *typed operation*. Do not use *trust domain* or *task family* as user-facing terms, and do not present *edit-execution mode* as a primary concept — it is the workspace environment.
