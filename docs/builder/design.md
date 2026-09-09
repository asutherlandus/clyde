# The Builder: Design

The builder is the sandboxed build pipeline: immutable snapshots, access baselines, the code-execution inventory, typed build tasks, the fetch/compile split, artifacts, the credential broker, and the audit chain.

It is driven by a human operator on the admin socket ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)), or by any machine driver over the actor socket — CI, or a coding agent the developer already runs. **It has no notion of an agent process.** Hosting an agent is [the warden](../warden/design.md), a separate product ([D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)).

Prerequisites: the [threat model](../threat-model.md) and [terminology](../terminology.md).

## Goal

Provide a development platform where untrusted project code, dependencies, build scripts, tests, and requested execution run with fine-grained isolation over filesystem access, network access, credential access, artifact flow, and publish authority — and make the secure path the easiest path.

## The key architectural decision

> **The driver does not execute project build or test code directly. It asks Clyde to run typed tasks under policy, in the appropriate environment.**

Most of the system follows from that. The driver stays productive, the conversational workflow survives, untrusted execution stays in controlled build environments, and capabilities can be reviewed, approved, denied, and audited.

**How firmly it is guaranteed depends on the deployment.** Under the [warden](../warden/design.md) it is structural: the driver's environment has no build toolchain, so asking Clyde is the only option. Under the builder alone it is a property of the host and a discipline of the driver, and Clyde's contribution is to make the pipeline good enough to use and to report honestly when the property does not hold ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

That distinction is worth holding while reading the rest. Everything below constrains what happens *when a task runs*, and a hostile dependency has no say in who requested the build.

## Design principles

1. Least privilege by default
2. Explicit trust boundaries
3. Ephemeral execution for untrusted code
4. Credentials are brokered, not mounted
5. Network is denied unless specifically required
6. Build and publish are separate security domains
7. Requested capabilities are policy-controlled
8. All privilege escalations are explicit and auditable

## Architecture

```text
+--------------------------+          +---------------------------------+
|     Human Developer      |          |   Any actor holding a token     |
|  CLI / TUI               |          |   CI, an external agent, or —   |
|  missions, approvals     |          |   with the warden — an agent    |
|  operator task surface   |          |   Clyde hosts                   |
+------------+-------------+          +----------------+----------------+
             |                                         |
             | admin channel (human only)              | actor channel
             | missions, approvals, tasks              | (MCP + session token)
             v                                         v
+--------------------------------------------------------------------+
|                        Clyde Control Plane                          |
|  policy engine | task scheduler | snapshot manager | egress proxy   |
|  sandbox manager | session/token manager | audit log | artifacts    |
|  approval manager | baseline store | broker gateway                 |
+-----------+---------------------------+-------------------+---------+
            |                           |                   |
            v                           v                   v
+---------------------+  +-----------------------+  +--------------------+
| Build Environment   |  | clyde-brokerd         |  | Artifact Layer     |
| build/test/fetch    |  | git push, later sign  |  | snapshots/logs/    |
| snapshot inputs     |  | credentials in-process|  | outputs/provenance |
| microVM by default  |  | never in a sandbox    |  |                    |
+---------------------+  +-----------------------+  +--------------------+
```

Three properties matter more than the boxes.

**The human's channel and the actor's channel are separate sockets**, so a sandboxed actor cannot approve its own escalation. The human can also run tasks on their own channel, which is what makes the pipeline complete without anything holding a token ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)).

**Untrusted project code executes in a microVM by default**, not only for the one task that touches the network ([D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)). That boundary carries the supply-chain guarantees, and it does not depend on who requested the build.

**One boundary is deliberately missing**: nothing stops a driver Clyde does not host from executing project code outside the pipeline entirely. The builder reports that rather than enforcing it, and closing it is the single reason the warden exists as a product.

## Components

The component model is agnostic about whether an actor is hosted: components take requests from a gateway and do not know what is on the other end of the socket. That is what makes the product split a daemon-layer boundary rather than an architectural one.

> The core rule: no human, agent, or untrusted build task bypasses the control plane for privileged effects.

| Component | Responsibility |
|---|---|
| **Actor gateway** (`clyded.sock`) | MCP tools for any token-holding driver; authenticates every request by session token; returns task status, logs, artifacts, and structured denials. May be bind-mounted into workspace-environment sandboxes. |
| **Admin gateway** (`clyded-admin.sock`) | JSON-RPC for the human: mission create/approve/deny/revoke/renew, workspace register, baseline confirmation, learn mode, audit read, and the operator task surface. Mode `0600`, `SO_PEERCRED`-checked, **never** mounted into any sandbox. The only surface on which an approval can be made. |
| **Mission manager** | Creates, tracks, and closes missions; authoritative for mission state. |
| **Lease manager** | Issues primary leases, derives child leases, tracks budgets and expiry, validates every lease-bound action, revokes and renews. |
| **Session/token manager** | Binds an actor process to a lease and makes the binding checkable on every request. Issues 256-bit tokens, stores only hashes, resolves token → session → lease → mission before any other check, and revokes transactionally with the lease. |
| **Policy engine** | Maps task types to policy profiles; decides environment, isolation, and resource profile; decides auto-approvable, approval-gated, or denied; suggests narrower alternatives. Fails closed. |
| **Approval manager** | Creates prompts from policy decisions, presents the narrowest understandable request, records approvals, denials, and timeouts, and binds each decision to a request digest. |
| **Snapshot manager** | Converts live mutable workspace content into immutable task inputs: content-addressed store, build-closure computation, materialisation by hardlink/reflink/copy, stable identifiers. |
| **Access baseline store** | Holds the confirmed record of what each build target may read and which dependencies execute code during its build, and detects divergence ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). |
| **Sandbox manager and scheduler** | Chooses backend from the policy's `min_isolation`, materialises inputs and outputs, configures mounts/network/scratch/limits, starts and supervises, streams logs. |
| **Egress proxy** | Makes "no network" and "registry-only" mechanically true rather than declarative ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)). |
| **Artifact coordinator and store** | Persists outputs by task and artifact id, separates trusted metadata from untrusted payloads, supports retrieval by mission/lease/task/actor, retains or collects by policy. |
| **Broker gateway** | The single Clyde-side adapter for privileged external operations. No other part of clyded may call the broker. |
| **Credential broker** (`clyde-brokerd`) | Executes privileged actions without exposing raw credentials. Capability-oriented, never secret-oriented. Verifies the approval record itself rather than trusting the caller. Separate process from Phase 0 so the boundary is never retrofitted ([D15](decisions.md#d15-three-binaries-from-phase-0)). |
| **Audit and provenance** | Append-only, hash-chained record of decisions, actions, artifacts, and authority transitions, linking mission, lease, actor, task, snapshot, artifacts, and approval. |

Field-level entity definitions, state machines, and invariants are in [schema.md](schema.md). The authority model is in [mission-and-lease.md](mission-and-lease.md). Task semantics, isolation classes, baselines, and egress are in [tasks-and-policy.md](tasks-and-policy.md).

### What is deliberately not a gateway operation

File read and write. An actor uses its own filesystem tools against the paths its lease permits. Where Clyde hosts the actor, that scope is a mount table and the boundary is kernel-enforced; where it does not, the scope is recorded in the closing diff rather than enforced. Either way this removes a large API surface and lets an off-the-shelf agent work unmodified.

### Design constraints worth stating

- **Snapshots are always read-only to tasks.** Hardlink materialisation shares inodes with the content store, which makes the read-only bind load-bearing rather than stylistic.
- **Materialisation carries an mtime contract, not just a content one.** Cargo decides freshness for local sources by mtime, so a snapshot must give an unchanged path the mtime it had last run and a changed one a fresh mtime — including a path *reverted* to content the store already holds, which hardlinking would otherwise hand an mtime older than the build output ([D27](decisions.md#d27-snapshot-materialisation-preserves-change-ordering-in-mtime)). Get this wrong in one direction and every run is a full rebuild; get it wrong in the other and a task reports a pass for a tree it did not build.
- **Input scope is a pinned baseline, not merely the build closure.** Cargo cannot build a subtree without the workspace root manifest, `Cargo.lock`, every member manifest, and in-repo path-dependency sources, and that closure routinely exceeds a lease's edit scope. Read-only admission of those paths is a confidentiality delta rather than an authority one, but breadth is still exfiltration surface — so the closure is the *starting point* for a baseline that a human confirms.
- **Anything a backend cannot honour in a sandbox spec is a preflight failure, never a silent relaxation.** That rule is what stops the `SandboxBackend` trait becoming the place where boundaries quietly weaken.
- **The baseline lives only in Clyde state.** It is therefore invisible to code review, not shared with a team or a fresh clone, and lost with that state. The mission envelope summarises the baseline in force; export/import is worth adding post-MVP.
- **A subtree grant is not drift-sensitive.** Ordinary editing inside the approved scope must never produce a prompt, or the control becomes noise and stops being read.

### Trust classification

**Trusted:** both gateways, mission and lease managers, session manager, policy engine, approval manager, snapshot manager, baseline store, sandbox manager, egress proxy and its in-sandbox forwarder, broker gateway, credential broker, audit system.

**Hostile by default:** project source, dependencies, build scripts, proc macros, tests, package install hooks, browser automation hooks, arbitrary repo scripts, agent-authored utility scripts, **repository git configuration and hooks** (`.git/config`, `.git/hooks` — attacker-controlled content a naive credentialed git invocation would execute), and any output of untrusted execution until validated.

**Semi-trusted:** a hosted coding agent, if the [sandbox](../warden/design.md) is deployed. Not hostile the way dependency code is, and not trusted either.

## Requirements

Normative requirements, grouped by area. Several are made mechanical by the [decisions](decisions.md); citations point at the deciding entry rather than restating rationale.

### A. Task model
- **A0.** A repo-local `AGENTS.md` may carry development and coding guidance for humans and agents. It guides how work is performed and never overrides security policy or widens authority.
- **A1.** Common workflows are exposed as **typed tasks** rather than unrestricted shell execution. Each typed task has a predefined policy covering filesystem scope, network scope, credential access, writable paths, execution runtime, resource limits, and output locations.
- **A2.** Arbitrary commands, if supported, are classified into explicit risk classes and never bypass policy enforcement.
- **A3.** Low-authority execution of ephemeral scripts that manipulate the live workspace is supported as `workspace.edit`. It is distinct from build/test/fetch execution and enforces: live access limited to lease-scoped paths; writable outputs limited to allowed repo paths and scratch; no raw credentials; no host home, unrelated projects, browser state, or container sockets; no egress but the configured model API allowlist; a separate runtime root; text and code manipulation tooling; and **no project build toolchain**. That absence is verified by an automated test over the runtime root, not by policy text. Work needing the toolchain runs as a different typed task. *In the MVP this requirement is satisfied by the workspace environment itself ([D17](../warden/decisions.md#d17-workspaceedit-helper-execution-is-the-workspace-environment)), which is a warden environment.*
- **A5.** Every build, fetch, and brokered task is drivable **without hosting anything** ([D25](decisions.md#d25-task-execution-has-an-operator-surface-on-the-admin-socket)): a human operator authenticated on the admin socket, or an external machine driver holding a session token, may run any task the mission's lease permits. A task admitted on either surface is evaluated against the same lease, policy, budget, and access baseline, produces the same records, and every record names the kind of principal that acted.
- **A6.** Enforcement **posture** is derived, reported, and recorded ([D26](decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)). `clyde doctor` reports it and names the specific bypass when `advisory`; every task run records the posture in force; mission review states it. No configuration value sets it, and posture participates in no admission decision.

> **A4 (agent hosting)** belongs to the warden; see [its design](../warden/design.md). Where Clyde does not host the driver, mount-topology enforcement of edit scope does not exist and **shall not be claimed**: out-of-scope edits are *detected* in the closing diff rather than prevented, and the difference belongs in user-facing documentation.

### B. Filesystem isolation
- **B1.** Untrusted tasks run against immutable snapshots, not a shared mutable workspace mount.
- **B2.** A task can be limited to a repository subtree or explicit path set.
- **B3.** Each untrusted task receives isolated writable scratch and isolated output directories.
- **B4.** Never mounted into untrusted build and test sandboxes: host home, `~/.ssh`, `~/.gnupg`, cloud credential directories, browser profiles, editor IPC sockets, container runtime sockets, unrelated projects.
- **B5.** Caches are isolated ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)): dependency caches content-addressed and read-only; writable build caches scoped to one mission and destroyed at closeout; no cache shared across missions, projects, or with the host's own package-manager caches.

### C. Network isolation
- **C1.** Compile, build, codegen, and most test tasks have no external network by default, and deny-by-default is **structural**: a task with egress profile `none` receives a loopback-only namespace with no proxy socket bound, so "no network" is the absence of a channel rather than a flag ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)).
- **C2.** Dependency download and update run in a distinct task from compilation.
- **C3.** Where network is allowed, it is one of a closed set of named **egress profiles**, enforced by a host-side proxy the sandbox reaches over a bound socket, with every attempt recorded whether allowed or refused. No code inside a sandbox can widen its own reachability. Approval prompts must state the limit: allowlisting is by destination, not by content.
- **C4.** Isolated internal test networks with fake or local-only services are supported for integration and browser tests (post-MVP).
- **C5.** Any profile enabling real external network access is explicit, narrowly scoped, and logged.

### D. Credential security
- **D1.** Untrusted tasks receive no raw SSH keys, GPG private keys, long-lived API tokens, or general-purpose agent sockets.
- **D2.** Credentialed git operations are mediated by the broker.
- **D3.** Signing accepts explicit data, digests, or manifests; private key material stays outside untrusted execution environments.
- **D4.** Where external service access is required, Clyde issues short-lived, purpose-scoped credentials bound to a task or policy.
- **D5.** Push, sign, publish, and production-facing credential access support explicit approval workflows.

### E. Rust-specific
- **E1.** Rust build actions are treated as untrusted code execution, including `cargo check`, `cargo build`, `cargo test`, `build.rs`, proc macros, doctests, and custom cargo workflows.
- **E2.** Compile and test run without network by default.
- **E3.** Dependency retrieval happens in a separate fetch stage, from a controlled mirror, proxy, vendor bundle, or content-addressed cache.
- **E4.** Policy over dependency sources is supported, including restrictions on git dependencies, lockfile drift, and unexpected dependency changes.
- **E5.** Enough build execution metadata is captured to identify suspicious subprocesses, file access patterns, and blocked network attempts where feasible.
- **E6.** Build and test tasks run against a **confirmed access baseline** in Clyde's own state, covering the repository content the task may read — subtree grants for first-party code within the mission-approved scope, plus individually confirmed file pins for anything outside it — and the inventory of packages executing code at build time, by crate, version, and source content hash. Path enforcement is by **materialisation**. A task with no confirmed baseline is refused, not run wide. Absolute exclusions are applied before grants and are not admissible by one. Ordinary development within a granted subtree is not drift and requires no approval. The baseline is never stored in the repository ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)).
- **E7.** Drift escalates: a read outside grants ∪ pins, including a new in-repo path dependency on a crate outside the approved scope; a new code-executing dependency, a version change to one, or a content change at the same version. Path drift and dependency drift are presented distinctly. The inventory check runs **before** the sandbox starts.
- **E8.** Learn mode is a privilege: human-invocable on the admin channel only, unavailable to any actor, never selected by Clyde as a fallback, limited to a single run, recorded distinctly, and without effect until a human confirms the proposal. Clyde proposes a baseline from static analysis so learn mode is the exception.

### F. Full-stack (post-MVP)
- **F1.** JavaScript and frontend dependency installation is untrusted execution.
- **F2.** Install, build, and browser/e2e execution are separable task policies.
- **F3.** Browser test tasks use isolated browser state, never the developer's session or profile.
- **F4.** Browser and integration tests use synthetic or narrowly scoped test identities.

### G. Machine-driver API
G1–G4 describe an API for any machine driver and hold whether or not Clyde hosts one. Capability *elevation* is the only piece specific to a hosted agent, because a human operator has nobody to ask.

- **G1.** An API lets an actor request task execution, retrieve logs, read artifacts, and request capability elevation. It does not assume the actor is hosted, and is drivable by CI, an external agent, and the CLI with no bespoke adapter ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api)).
- **G2.** Planning, editing, execution, and publishing behaviours are logically separated even within one product surface.
- **G3.** No implicit privilege inheritance: the ability to edit code does not imply permission to execute untrusted code with credentials, push, or publish.
- **G4.** Capability requests carry a stated reason, desired scope, and time limit. **Denials are structured and actionable**: what was denied, which constraint denied it, and what narrower or escalated alternative exists. A bare denial makes a driver loop or route around the boundary; both are worse than a clear next step.

### H. Artifact flow and provenance
- **H1.** Artifacts move through explicit output channels, not direct access to privileged environments.
- **H2.** Each task records stable identifiers for the source snapshot, dependency inputs, and toolchain.
- **H3.** An audit trail per task records task type, policy profile, input identifiers, network policy, credential policy, result, and outputs.
- **H4.** Provenance or attestation metadata for build outputs is supported.

### I. Runtime
- **I0.** A workspace runtime separate from build/test/fetch runtimes: text and code manipulation tooling, **no** project build toolchain (verified by automated test over the root's contents), no credentials, no egress beyond the configured model API allowlist, and only lease-scoped live workspace paths plus isolated scratch.
- **I1.** Untrusted tasks get a stronger boundary than a shared development shell; ephemeral microVMs are the intended boundary. Each policy declares a minimum isolation level; the manager may select stronger and never weaker. Any configured downgrade is audited and unavailable for T3 tasks. No task with an egress profile other than `none` runs below microVM isolation.
- **I2.** Untrusted task environments are short-lived and destroyed after completion unless explicitly retained for debugging.
- **I3.** Tasks execute against read-only, content-pinned runtime roots — nix closures identified by store path in the MVP ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)). The requirement is the property, not the packaging format.
- **I4.** Memory, CPU, time, and process-count limits per task. For trust class T2 and above these are enforced by cgroup v2, and where delegation is unavailable such tasks are **refused** — there is no configuration permitting untrusted project execution under rlimits alone ([D22](decisions.md#d22-no-degraded-resource-limits-for-untrusted-execution)).

### J. Policy engine
- **J1.** Task policies are declarative so they can be reviewed, tested, and versioned.
- **J2.** Clyde can show what a task can access — mounts, network, outputs, credentials — before or during execution.
- **J3.** If policy cannot be determined the system fails closed or uses a clearly restricted default.

### K. Developer experience
- **K1.** The normal workflow for checking, testing, building, and publishing routes through policy-enforced task execution.
- **K2.** The CLI and any driver-facing UI clearly show when a task has no network, limited network, brokered credentials, approval requirements, or elevated risk.
- **K3.** Isolation failures are diagnosed clearly: filesystem restriction, blocked network, missing capability, or sandbox runtime issue.
- **K4.** Any unsafe or compatibility mode is clearly labelled, auditable, and disabled by default.

### L–O. Non-functional
- **L. Security.** Default operation exposes no raw credentials to untrusted execution; compromised build steps cannot directly sign or publish; cross-project contamination through caches and mounts is minimised.
- **M. Performance.** Task startup is optimised through caching, snapshots, or reuse mechanisms that do not collapse security boundaries. Secure execution must be fast enough for iterative development and agent loops.
- **N. Portability.** Linux-first, without unnecessary coupling to a single host deployment model.
- **O. Testability.** Policy resolution, capability decisions, and broker interfaces are testable independently of a full runtime. Policy resolution, lease derivation, egress profile ordering, and budget charging are **pure functions with no I/O**. Security claims are expressed as automated tests, including: no credential path in any generated sandbox mount table; egress refusal from a `none`-profile sandbox; cache non-persistence across missions; no code execution during a brokered push against a repository containing hostile hooks and configuration.

### Out of scope for the first version
Perfect reproducibility across every ecosystem; every package manager and framework; zero-cost compatibility with arbitrary developer machine state; production-grade remote or distributed execution. The architecture should leave room for them.

## Technology stack

| Subsystem | MVP choice | Later |
|---|---|---|
| Control plane, CLI/TUI | **Rust** | unchanged |
| Process model | three binaries: `clyde` (CLI/TUI), `clyded` (control plane), `clyde-brokerd` (broker) ([D15](decisions.md#d15-three-binaries-from-phase-0)) | optional fourth for the warden ([D23](decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)) |
| Actor API | **MCP over a Unix socket**, newline-delimited JSON-RPC ([D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api), [D19](decisions.md#d19-mcp-over-the-actor-socket-is-line-framed-json-rpc)) | gRPC or streamable HTTP for remote deployment |
| Admin API | JSON-RPC over a Unix socket, same message codec | — |
| Config and policy | **TOML** for static config, **Rust enums/structs** for built-in task policy | a Cedar/OPA-like layer if the task model stabilises |
| Metadata store | **SQLite** | optional Postgres for shared/remote control planes |
| Snapshot storage | content-addressed filesystem store; hardlink, reflink, or copy materialisation | dedup, overlay materialisation, OCI-style layer export |
| Sandbox backends | **Firecracker microVM by default for build tasks; bubblewrap for the workspace environment, Part 1a bring-up, and hosts without KVM** ([D5](decisions.md#d5-bubblewrap-first-behind-a-sandboxbackend-trait), [D9](decisions.md#d9-the-firecracker-backend-lands-as-part-1b-before-dependency-resolution), [D24](decisions.md#d24-firecracker-is-the-default-backend-for-build-execution)) | warm pool via snapshot/restore; Kata or gVisor only if Firecracker proves impractical |
| Guest file surface | block devices only — a long-lived per-mission cache image plus a per-run source image | read-only base plus writable delta, if measured cost demands it |
| Runtime roots | **nix closures per task family, no OCI images** ([D6](decisions.md#d6-runtime-roots-are-nix-closures-not-oci-images)) | the same closures build the guest images |
| Egress control | **Clyde CONNECT proxy, socket-bridged into a loopback-only netns**; vsock under Firecracker ([D7](decisions.md#d7-registry-only-egress-is-enforced-by-a-clyde-managed-proxy)) | — |
| Actor authentication | per-session capability tokens, separate admin socket ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)) | OS-user separation for multi-user hosts |
| Caches | read-only dependency bundles, per-mission writable build cache ([D3](decisions.md#d3-build-caches-are-per-mission-and-writable-dependency-caches-are-read-only)) | — |
| Task orchestration | Tokio-based async supervisor | unchanged |
| Artifact store | filesystem blob store + SQLite metadata | optional S3/OCI/CAS |
| Hashing | **BLAKE3** locally; SHA-256 where external compatibility matters | — |
| Logs and audit | `tracing` + JSON logs; hash-chained audit events in SQLite | OpenTelemetry export, in-toto/SLSA provenance |
| Credential broker | separate local Rust daemon over a Unix socket, credential held in-process only ([D8](decisions.md#d8-brokered-gitpush-uses-the-developers-existing-credential-inside-the-broker-only)) | split brokers, HSM/Vault |
| Git | git CLI through a single sanitised-invocation helper | selective libgit2 |
| Client | CLI through Phase 3, TUI in Phase 4 ([D13](decisions.md#d13-cli-through-phases-1-3-tui-in-phase-4)) | editor plugins |
| Browser, synthetic services | post-MVP (Phase 5); Playwright in a dedicated sandboxed profile is the intended direction | dedicated browser runner |

### Why Rust
Clyde is a local control plane, a security-sensitive orchestrator, a policy engine, a concurrent task supervisor, and a typed API surface. Rust fits long-running daemons, precise data modelling, async orchestration, CLI/TUI, compile-time guarantees around policy and state transitions, and Linux-first single-binary packaging.

Bash is a poor fit for state machines, structured policy enforcement, audit trails, and socket APIs; it remains fine for helper scripts and probing. TypeScript is weaker for process orchestration, durable daemon behaviour, and namespace/cgroup/mount integration, but is a reasonable choice for future editor and UI adapters. Go would be a viable alternative; Rust is preferred for richer type modelling of policy and state invariants.

Suggested crates: `tokio`; `anyhow` and `thiserror`; `serde`/`serde_json`/`toml`; `clap`; `tracing`; `rusqlite` or `sqlx`; `blake3`; `zstd`; `walkdir` and `ignore`; `ratatui` and `crossterm`; `inotify` ([R3](decisions.md#r3-learn-mode-observation-uses-the-inotify-crate)); `e2fsprogs` (`mke2fs -d`) and `erofs-utils` (`mkfs.erofs`) invoked to build guest images ([R12](decisions.md#r12-the-guest-root-image-is-uncompressed-erofs)), never to read one back ([R11](decisions.md#r11-all-guest-output-leaves-over-vsock)). Testing: `cargo-nextest`, `insta`, `assert_cmd`, `tempfile`.

### Why a daemon rather than a one-shot CLI
Active missions and leases, long-running task supervision, background log streaming, revocation and renewal, broker coordination, and UI clients reconnecting to the same mission state all want a resident process.

### Policy representation
A hybrid: **built-in task policies in Rust code** (the meaning of `rust.check`, the meaning of `git.push`, default runtime and trust class, invariants such as no credentials in T2 tasks), and **repo/user/org config in TOML** (allowed registries, subtree defaults, mission pre-approvals, push branch patterns, synthetic service defaults).

Precedence is built-in defaults → host config → user config → `.clyde/policy.toml` ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)). **Repository configuration may only narrow.** A repo value that would widen authority is rejected with a diagnostic rather than silently clamped, because a silent clamp leaves the user believing something is configured that is not. Repository content is untrusted, and a repo that can widen its own authority is a self-signed permission slip. `AGENTS.md` is prose passed into agent context and never parsed for authority.

A fully dynamic policy engine (OPA/Rego, Cedar) is not needed for the MVP and would add complexity before the task and mission model is stable.

### Socket topology

| Socket | Callers | Mounted into sandboxes | Authentication |
|---|---|---|---|
| `clyded.sock` | actors: external agents, CI, and hosted agents | yes, workspace environments only | session capability token |
| `clyded-admin.sock` | the human operator — missions, approvals, and tasks | never | `0600` plus `SO_PEERCRED` |
| `brokerd.sock` | clyded only | never | `SO_PEERCRED`, plus independent approval verification |

"The human approves boundary crossings" is only enforceable if a sandboxed actor cannot impersonate the human. With one socket and a self-declared identity, the agent whose escalation is under review can approve it. The split makes self-approval structurally impossible rather than policy-prohibited, and `clyde approve` refusing to run inside a sandbox is the visible form of it.

The operator task surface does not weaken this: a human running a task is authenticated by `SO_PEERCRED` and admitted against the same lease and policy as any actor. What stays impossible is a *sandboxed* actor reaching the admin socket at all. Both sockets are bound in every deployment, including human-only ones, because retrofitting a channel split into a running protocol is the change that goes wrong.

### Runtime roots
Each task family executes against a nix closure defined in the project flake and identified by its store path. The flake is already the source of truth for tooling, so reuse gives content-pinned reproducibility for free, keeps one definition of "the Rust toolchain we use", and removes upstream base-image trust from the supply chain. Under bubblewrap the closure is bind-mounted read-only; under Firecracker the guest rootfs is built from the same closure.

- `runtimeRoots.workspace` — shell utilities, search and filter tools, structured text and code transformation helpers, and an interpreter for one-off codemods
- `runtimeRoots.rust` — Rust toolchain for check/test/build
- `runtimeRoots.fetch` — cargo plus network client tooling

`runtimeRoots.workspace` must not contain `cargo`, `rustc`, `rustup`, `node`, `npm`/`pnpm`/`yarn`, a browser, `gpg`, `ssh`, `docker`, or `podman`. Because a runtime root is a derivation this is checkable rather than merely reviewable, and [Phase 0](roadmap.md#phase-0-foundations) requires the test.

Not recommended: one mutable root shared across task families; installing packages inside a sandbox at task time; reusing the build root as the workspace root.

**Cost accepted:** nix becomes a requirement on any host that executes tasks, a deliberate narrowing of portability for the MVP.

### Broker API style
The broker exposes typed actions, not raw secret access. `git_push(branch, commit)`, `sign_digest(digest, key_profile)`, and `publish_artifact(artifact_id, destination)` exist; `get_ssh_key()`, `read_gpg_secret()`, and `return_github_token()` do not and must not. For the MVP it wraps the git CLI rather than libgit2 — simpler to reason about operationally and closer to normal user git behaviour. Signing wraps `gpg` or SSH signing, later Sigstore/Cosign. The design rule matters more than the tool: the signer stays in the broker, signing acts on explicit inputs, and untrusted code never gets key material.

## Interaction flows

The exact wire protocol is unspecified here; these are conceptual flows. Human steps happen on the admin channel, which is unreachable from any actor environment. Where the requester and the approver are the same human, an approval is a **confirmation**, with the same prompt and the same record.

### Start a mission

```text
Human -> Clyde: "Implement refresh-token rotation in backend auth"
Clyde -> Policy Engine: propose mission from request + repo defaults
Clyde -> Human: mission proposal
  scope backend/auth, backend/tests/auth · tasks rust.check, rust.test.unit
  network none · credentials none · duration 45m
Human -> Clyde: approve mission                       [admin channel]
Clyde -> Audit Log: record mission + lease issuance
```

The mission becomes active at the audit step and the lease is usable immediately — from the operator surface, or by an external driver Clyde issues a token to. **Mission activation must not depend on there being something to host.** Where the warden is deployed, activation additionally creates the workspace environment and starts the agent; see [its session lifecycle](../warden/design.md#session-lifecycle).

*Properties:* the human approves a bounded envelope up front; the driver gets a lease rather than ambient power; allowed actions are explicit before any code executes.

### The inner loop

```text
Driver: edit files on the live workspace
Driver -> Clyde: run_task(rust.check, backend/auth)   [operator or actor surface]
Clyde -> Policy Engine: resolve policy under the mission's lease
Policy Engine -> Clyde: allowed · T2 · microVM · egress none · baseline required
Clyde -> Baseline Store: load confirmed baseline; recompute code-exec inventory
Clyde -> Snapshot Manager: materialise the baselined snapshot from the live tree
Clyde -> Sandbox Runtime: run, no network, no credentials
Sandbox Runtime -> Clyde: exit status + structured cargo diagnostics
Clyde -> Artifact Layer: logs, diagnostics, task result
Clyde -> Driver: streamed logs, then classified outcome
Clyde -> Audit Log: task run, acting principal, posture in force
... repeat, mission cache warm ...
```

*Properties:* build and test use sealed snapshots, never live mutable mounts; no egress or credentials are available to them at all; each run is attributable to mission + lease + actor + session; the record names the principal so review does not have to infer it.

*What differs without the warden, and it is not small:* nothing prevents the driver running `cargo check` themselves — the pipeline's guarantees cover what is run through it, and posture says so. Nothing constrains where they edit; out-of-scope edits appear in the closing diff rather than failing at the kernel. **The boundary that matters is unchanged**: a hostile `build.rs` runs in a microVM against a read-only snapshot either way.

*UX:* failure classification carries more weight when there is no agent to interpret a raw cargo error — `MissingDependencies` must name the next step. `clyde doctor` should have reported `advisory` posture before the first task, not after.

### Missing dependency escalates

```text
rust.check fails with MissingDependencies
  → actor calls request_escalation(rust.resolve-deps, reason, scope)
    or, human-driven, the failure names rust.resolve-deps as the next step
  → policy engine: allowed with human approval, egress rust-registry
  → prompt on the admin channel:
      task rust.resolve-deps
      input manifests + lockfile only, no application source
      egress profile rust-registry, with the exact host allowlist shown
      lockfile change: 4 additions, 0 source changes
      credentials none · outputs dependency bundle only
      caveat: allowlisting is by destination, not content
  → human approves once / for mission / denies
  → side lease or amendment recorded, bound to the approval digest
  → rust.resolve-deps runs in a fetch sandbox, loopback-only netns,
    rust-registry allowlist activated, every attempt logged
  → rerun rust.check with egress none and the new bundle
```

There is no `request_escalation` in the human-driven path because there is nobody to ask. What must **not** differ is the prompt or the record. The prompt must explain *why* the previous profile failed; a request that does not say what went wrong invites reflexive approval, and a human confirming their own request is if anything more prone to it.

*Properties:* compile and fetch stay separate; broader network is requested explicitly rather than silently inherited; outputs are limited to dependency artifacts.

### New dependency code triggers a pre-execution check

```text
Driver -> Clyde: run_task(rust.check, backend/auth)
Clyde -> Inventory Check: recompute from lockfile + bundle, diff against baseline
Inventory Check -> Clyde: drift
  + serde_derive_internals 0.29.1  (new proc-macro crate)
  ~ ring 0.17.8 -> 0.17.9          (build.rs content changed)
Clyde -> Driver: task refused, inventory confirmation required
Clyde -> Human: drift prompt — the diff, not the whole baseline  [admin channel]
Human -> Clyde: confirm
Clyde -> Baseline Store: amend baseline, record confirming actor
... rust.check now proceeds ...
```

*Properties:* the check runs **before** the sandbox starts, so new dependency code is surfaced before it executes; a same-version content change is reported distinctly from a version upgrade, because it is a tampering signal rather than a normal one; the human reviews a diff so the prompt stays readable as the graph grows.

*UX:* path drift and code-execution drift must look different — the first is common and low-signal, the second is the reason this control exists. If these prompts become routine the control has failed, so frequency is worth measuring against a real repository.

### Prepare a commit, then push through the broker

```text
Driver -> Clyde: git.commit.prepare
Clyde -> Workspace: compute scoped diff and commit proposal
Clyde -> Human: review proposed commit
Human -> Clyde: approve; Clyde creates the local commit (sanitised git)

Driver -> Clyde: request_publish(git.push, feature/refresh-token-rotation)
Clyde -> Human: push prompt — remote, resolved URL, branch, commit id and
  subject, diff statistics, requesting principal, mission, posture, and the
  passing task evidence for that tree
Human -> Clyde: approve
Clyde -> Credential Broker: push commit to origin/feature/...
  Broker: verify the approval digest independently
  Broker: create sanitised temp repo; fetch the commit with hooks and
          object-transfer hooks disabled
  Broker: verify commit id and tree match the approval, then push with hooks
          disabled and system/global git config neutralised
Clyde -> Audit Log: record brokered push
```

*Properties:* no SSH socket or long-lived token is mounted into any build or workspace environment; the broker executes the privileged effect outside untrusted execution; the broker verifies the approval itself rather than trusting the caller; repository hooks and configuration cannot execute during the push.

### Expiry, revocation, and review

On approaching expiry Clyde warns, the driver may request renewal, and policy decides whether that needs approval. On expiry, no new work is scheduled under the lease and the mission pauses pending renewal. On revocation, further actions are blocked immediately, derived leases and sessions are revoked in the same transaction, running tasks are stopped or quarantined per policy, in-flight brokered operations are **frozen** rather than silently cancelled, and logs, snapshots, and outputs are preserved for review.

Mission review collects the objective, files changed, tasks run with pass/fail and policy digests, escalations and their outcomes, approvals granted, egress attempts, brokered operations, budget consumed, posture, and the closing diff. It should be much higher-level than raw sandbox logs, with drill-down available.

## Cross-cutting flow rules

1. **All privileged effects go through Clyde.** No direct credential channels to drivers or build sandboxes.
2. **All untrusted execution uses task policy.** Missions and leases do not redefine runtime isolation.
3. **All meaningful actions are attributable** — mission, lease, session, actor, task, artifacts.
4. **No actor can authorise itself.** Approvals arrive only on a channel absent from every actor environment.
5. **Boundary crossings interrupt autonomy.** Safe loops continue; authority expansion stops for review.
6. **Artifacts move across trust boundaries, not ambient process access.**

## Escalation UX

When additional privilege is needed, the prompt must be structured rather than vague. Not "Allow network access?" but:

```text
task:              node.resolve-deps
repo path:         frontend/
egress profile:    rust-registry  (crates.io, static.crates.io, index.crates.io)
credentials:       none
writable outputs:  dependency-bundle
reason:            download locked dependencies from approved mirror
caveat:            allowlisting is by destination, not content
```

The human can approve once, approve for the mission, reject, or request a stricter alternative. Logs and policy are first-class in the UX: a task result shows the policy profile used, whether network was enabled, whether credentials were reachable, which snapshot was used, which outputs were produced, and whether any actions were blocked.

## What makes this different

Compared with dev containers or hosted workspaces: **task-based isolation** rather than workspace-only isolation; **brokered authority** rather than mounted credentials; **policy-aware driver interaction** rather than unconstrained shell access; and **artifact movement across trust boundaries** rather than shared mutable execution state.
