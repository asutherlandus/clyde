# Clyde Next: Schema Reference

## Purpose

This document is the canonical schema reference for Clyde Next Phases 0-4. It defines the entities, their fields, their state machines, their invariants, and their persistence layout.

It is the interface contract between phases: Phase 0 lands these as Rust types with `serde` and validation ([D4](decisions.md#d4-phase-0-delivers-flake-ci-crate-layout-and-typed-schemas)), and Phases 1-4 implement behaviour against them.

Type sketches below are illustrative Rust, not final code. Field presence, cardinality, and invariants are normative; naming may be refined during Phase 0 implementation.

## Conventions

### Identifiers
All entity identifiers are typed newtypes over a string, never bare `String` at API boundaries:

```rust
pub struct MissionId(String);   // "m-<ulid>"
pub struct LeaseId(String);     // "l-<ulid>"
pub struct ActorId(String);     // "human:<name>" | "agent:<name>" | "agent:<name>/<n>"
pub struct TaskRunId(String);   // "t-<ulid>"
pub struct SnapshotId(String);  // "s-blake3:<hex>"
pub struct ArtifactId(String);  // "a-blake3:<hex>"
pub struct ApprovalId(String);  // "ap-<ulid>"
pub struct WorkspaceId(String); // "w-<ulid>"
```

ULIDs are used for entities whose creation order matters; content hashes (BLAKE3) for entities whose identity is their content. Human-readable prefixes are retained for log legibility.

### Time
All timestamps are UTC, serialised as RFC 3339. Durations are serialised as strings (`"45m"`, `"10s"`) at config and API boundaries, and held as `Duration` internally.

### Paths
Repository paths are workspace-root-relative, normalised, and rejected if they escape the root, contain `..` after normalisation, or traverse a symlink out of the workspace. This validation lives in one place and is exercised by unit tests from Phase 0.

```rust
pub struct RepoPath(String); // validated, workspace-root-relative, no traversal
```

### Fail-closed parsing
Every deserialised structure is validated on construction. Unknown fields are rejected (`#[serde(deny_unknown_fields)]`) on config and request types, so a typo in a policy file is an error rather than a silently ignored restriction.

## Workspace

A registered project directory. Introduced in Phase 1.

```rust
pub struct Workspace {
    pub id: WorkspaceId,
    pub root: PathBuf,              // absolute host path
    pub vcs: VcsKind,               // Git { default_remote, default_branch } | None
    pub registered_at: DateTime<Utc>,
    pub policy_digest: Option<String>,  // blake3 of .clyde/policy.toml as last loaded
}
```

**Invariants**
- At most one mission in a non-terminal state per workspace ([D16](decisions.md#d16-one-active-mission-per-workspace)).
- `root` must be a directory the daemon's user can read and write.

## Actor

```rust
pub enum ActorKind { Human, Agent, SubAgent }

pub struct Actor {
    pub id: ActorId,
    pub kind: ActorKind,
    pub display_name: String,
    pub parent: Option<ActorId>,    // Some(..) for SubAgent
    pub created_at: DateTime<Utc>,
}
```

Actors hold no authority. Authority is always a property of an active lease bound to an actor.

## Actor session and token

Introduced in Phase 1 ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)).

```rust
pub struct ActorSession {
    pub actor: ActorId,
    pub lease: LeaseId,
    pub token_hash: [u8; 32],       // SHA-256 of a 256-bit random token
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,  // never later than lease.expires_at
    pub revoked_at: Option<DateTime<Utc>>,
    pub sandbox: Option<SandboxHandle>, // workspace environment hosting this actor
}
```

**Invariants**
- The plaintext token exists only in the sandbox token file and in the issuing code path. It is never logged, never returned in a query result, and never stored.
- Token expiry is derived from lease expiry, never independent of it.
- Revoking a lease revokes every session bound to it, in the same transaction.

## Mission

```rust
pub struct Mission {
    pub id: MissionId,
    pub workspace: WorkspaceId,
    pub objective: String,
    pub initiator: ActorId,             // human
    pub primary_actor: ActorId,         // agent
    pub scope: MissionScope,
    pub allowed_tasks: BTreeSet<TaskType>,
    pub network_policy: NetworkPolicy,
    pub credential_policy: CredentialPolicy,
    pub approval_policy: ApprovalPolicy,
    pub budget: Budget,
    pub expiry: DateTime<Utc>,
    pub state: MissionState,
    pub stop_conditions: BTreeSet<StopCondition>,
    pub success_criteria: Vec<String>,  // human-readable, not machine-evaluated in MVP
    pub cache_dir: Option<PathBuf>,     // per-mission cache (D3)
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

pub struct MissionScope {
    pub edit_paths: BTreeSet<RepoPath>,     // writable
    pub read_paths: BTreeSet<RepoPath>,     // additional read-only
}
```

### Mission states

```text
proposed ──> awaiting_approval ──> active ──┬──> completed
                    │                       ├──> revoked
                    ├──> denied             ├──> expired
                    │                       ├──> failed
                    │                       ├──> paused ──> active
                    │                       └──> blocked_on_escalation ──> active
                    └──> revised (new mission supersedes)
```

**Invariants**
- Terminal states (`completed`, `revoked`, `expired`, `failed`, `denied`) are final; no transition leaves them.
- Entering any terminal state revokes all leases and sessions and schedules cache teardown, in one transaction.
- `allowed_tasks` may never be widened after approval; widening requires an escalation that produces a new approval record, or a new mission.

## Lease

```rust
pub struct Lease {
    pub id: LeaseId,
    pub mission: MissionId,
    pub parent: Option<LeaseId>,
    pub actor: ActorId,
    pub issued_by: ActorId,             // always clyde
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub repo_scope: MissionScope,
    pub task_scope: BTreeSet<TaskType>,
    pub network_scope: EgressProfile,
    pub credential_scope: CredentialPolicy,
    pub authority: AuthorityFlags,
    pub budget: Budget,
    pub usage: BudgetUsage,
    pub state: LeaseState,
    pub purpose: String,
}

pub struct AuthorityFlags {
    pub may_edit: bool,
    pub may_request_tasks: bool,
    pub may_spawn_subagents: bool,
    pub may_request_publish: bool,
}
```

### Lease states

```text
issued ──> active ──┬──> exhausted
                    ├──> expired
                    ├──> revoked
                    └──> superseded (renewal issued a replacement)
```

### Derivation rules (normative)

A derived lease is valid only if **all** of the following hold against its parent:

1. `repo_scope.edit_paths ⊆ parent.repo_scope.edit_paths`
2. `repo_scope.read_paths ⊆ parent.repo_scope.read_paths ∪ parent.repo_scope.edit_paths`
3. `task_scope ⊆ parent.task_scope`
4. `network_scope` is no wider than `parent.network_scope` (see [egress profile ordering](network-egress-model.md#profile-ordering))
5. `credential_scope` is no wider than `parent.credential_scope`
6. every `authority` flag is `false` where the parent's is `false`, and `may_spawn_subagents` is `false` (one level of derivation in the MVP)
7. `expires_at <= parent.expires_at`
8. `budget` is within the parent's remaining budget, and consumption is charged to both

These rules are a pure function over two leases and are the highest-value unit-test target in the codebase.

## Budget

```rust
pub struct Budget {
    pub max_duration: Duration,
    pub max_task_runs: u32,
    pub max_parallel_subagents: u8,
    pub max_subagents: u8,
    pub max_cpu_seconds: u64,
    pub max_cache_bytes: u64,
    pub max_artifact_bytes: u64,
}

pub struct BudgetUsage { /* same dimensions, consumed */ }
```

**Invariants**
- Budget consumption is recorded before a task starts, not after it completes, so a crashed daemon cannot lose the charge.
- Exhaustion in any dimension moves the lease to `exhausted` and blocks new work, without terminating in-flight work.

## Task type and policy

```rust
pub enum TaskType {
    WorkspaceRead, WorkspaceEdit, RepoSearch,
    RustResolveDeps, RustCheck, RustTestUnit,
    GitCommitPrepare, GitPush,
}
```

The MVP catalog is closed. Task types are Rust enum variants, not strings, so an unknown task type is unrepresentable rather than a runtime lookup failure.

```rust
pub struct TaskPolicy {
    pub task: TaskType,
    pub environment: Environment,        // Workspace | Build | Broker
    pub trust_class: TrustClass,         // T0..T4
    pub min_isolation: IsolationLevel,   // NamespaceSandbox | MicroVm | Broker | InProcess
    pub input: InputSpec,                // LiveWorkspace | Snapshot { closure_aware: bool }
    pub outputs: Vec<OutputSpec>,
    pub egress: EgressProfile,
    pub credentials: CredentialPolicy,
    pub cache: CachePolicy,              // None | MissionScoped { .. }
    pub limits: ResourceLimits,
    pub approval: ApprovalRequirement,   // None | PolicyGated | HumanRequired
    pub audit_level: AuditLevel,
}
```

Built-in policies are typed Rust values ([technology choices](technology-choices.md#policy-representation)). `.clyde/policy.toml` may only narrow them ([D14](decisions.md#d14-machine-readable-policy-in-clyde-agentsmd-advisory-only)).

## Task request and task run

```rust
pub struct TaskRequest {
    pub id: TaskRunId,          // allocated at request time
    pub lease: LeaseId,
    pub actor: ActorId,
    pub task: TaskType,
    pub path: RepoPath,
    pub options: TaskOptions,   // task-specific, validated per type
    pub requested_at: DateTime<Utc>,
}

pub struct TaskRun {
    pub id: TaskRunId,
    pub request: TaskRequest,
    pub policy_digest: String,      // hash of the resolved policy actually applied
    pub snapshot: Option<SnapshotId>,
    pub dependency_bundle: Option<ArtifactId>,
    pub backend: BackendKind,       // which SandboxBackend ran it
    pub state: TaskRunState,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: Option<TaskOutcome>,
    pub artifacts: Vec<ArtifactId>,
    pub egress_log: Vec<EgressAttempt>,
}
```

### Task run states

```text
requested ──> denied
          └─> admitted ──> preparing ──> running ──┬──> succeeded
                                                   ├──> failed
                                                   ├──> cancelled
                                                   └──> timed_out
```

### Task outcome and failure classification

Structured classification is required from Phase 2a, because Phase 3's escalation flow keys off it:

```rust
pub struct TaskOutcome {
    pub exit_code: Option<i32>,
    pub classification: TaskFailureClass,
    pub summary: String,             // redaction-safe, bounded length
}

pub enum TaskFailureClass {
    Success,
    ProjectCodeError,        // compile/test failure in project code
    MissingDependencies,     // drives the Phase 3 escalation path
    PolicyDenied,            // blocked by lease or policy before execution
    EgressBlocked,           // proxy denied a destination
    ResourceExhausted,       // memory, cpu, wall clock, tasks
    SandboxFailure,          // backend or host problem, not the project's fault
    Internal,
}
```

`SandboxFailure` and `Internal` are deliberately distinguished from project errors so that "Clyde is broken" is never reported to a user as "your code is broken".

## Snapshot

```rust
pub struct Snapshot {
    pub id: SnapshotId,                 // blake3 over the manifest
    pub workspace: WorkspaceId,
    pub mission: MissionId,
    pub requested_path: RepoPath,
    pub manifest: SnapshotManifest,
    pub created_at: DateTime<Utc>,
    pub materialisation: MaterialisationKind, // Hardlink | Reflink | Copy
}

pub struct SnapshotManifest {
    pub entries: Vec<SnapshotEntry>,        // path, mode, size, blake3
    pub closure_paths: Vec<RepoPath>,       // build-closure paths outside the requested path
    pub out_of_lease_paths: Vec<RepoPath>,  // admitted read-only; see OQ1
    pub exclusions_applied: Vec<String>,
}
```

**Invariants**
- Snapshots are bound read-only into sandboxes, always. Hardlink materialisation shares inodes with the content store, so a writable bind would corrupt the store; the read-only bind is what makes hardlinking safe.
- Exclusions are applied before hashing, so the snapshot id reflects exactly what the task can see.
- A snapshot records which paths came from outside the lease's edit scope, so mission review can show it.

## Access baseline

Introduced in Phase 2a ([D18](decisions.md#d18-build-access-is-baselined-pinned-in-clyde-state-and-escalated-on-drift)). Stored in Clyde state only, never in the repository.

```rust
pub struct AccessBaseline {
    pub workspace: WorkspaceId,
    pub task: TaskType,
    pub target: RepoPath,               // build target the baseline applies to
    pub paths: Vec<BaselinePathEntry>,  // subtree grants + out-of-scope pins
    pub inventory: CodeExecInventory,
    pub origin: BaselineOrigin,         // StaticClosure | Learned | Amended
    pub confirmed_by: ActorId,          // must be Human
    pub confirmed_at: DateTime<Utc>,
    pub digest: String,                 // blake3 over the canonical encoding
}

pub enum BaselinePathEntry {
    /// First-party project code approved as a whole subtree. Not drift-sensitive:
    /// files created, renamed, or removed inside it are ordinary work.
    SubtreeGrant { path: RepoPath, source: GrantSource },
    /// A single path outside every granted subtree, confirmed individually.
    /// Drift-sensitive.
    FilePin { path: RepoPath, blake3: Option<String>, reason: String },
    /// Rollup form of many pins under one out-of-scope directory.
    SubtreePin { path: RepoPath, reason: String },
}

pub enum GrantSource {
    MissionEditScope,     // approved in the mission envelope
    MissionReadScope,     // approved in the mission envelope
    BuildClosure,         // required by cargo, within approved scope
}

pub struct CodeExecInventory {
    pub lockfile_digest: String,
    pub entries: Vec<CodeExecEntry>,
}

pub struct CodeExecEntry {
    pub crate_name: String,
    pub version: String,
    pub kind: CodeExecKind,             // BuildScript | ProcMacro | Both
    pub source_blake3: String,          // hash of the package source in the bundle
}
```

### Drift classification

```rust
pub enum AccessDrift {
    /// Read outside grants ∪ pins. Note: never triggered by a new file inside a grant.
    PathOutsideBaseline { path: RepoPath },
    /// New in-repo path dependency on a crate outside the approved scope.
    ClosureLeftApprovedScope { path: RepoPath, dependent: String },
    NewCodeExecCrate { crate_name: String, version: String, kind: CodeExecKind },
    CodeExecVersionChanged { crate_name: String, from: String, to: String },
    CodeExecContentChanged { crate_name: String, version: String },  // registry tampering signal
}
```

**Invariants**
- A baseline has no effect until `confirmed_by` names a human who confirmed it on the admin channel.
- A task with no confirmed baseline for its `(workspace, task, target)` key is refused. There is no implicit wide-scope run.
- Path enforcement is by materialization: the snapshot contains the granted subtrees minus absolute exclusions, plus the pins, so enforcement cannot drift from the record.
- `SubtreeGrant` entries are **not** drift-sensitive. A file appearing inside one is admitted on the next snapshot and produces no `AccessDrift`, because first-party editing is the work rather than the threat.
- Absolute exclusions (`.env*`, key material, `target/`, `node_modules/`, `.git` by default) are applied before grants and cannot be admitted by a grant.
- A `FilePin` or `SubtreePin` requires a `reason`, so a later reviewer can tell why the build reaches outside the project's approved scope.
- The inventory check runs **before** the sandbox starts, so a new or changed build script is detected before it executes.
- `CodeExecContentChanged` is reported distinctly from a version change, because same-version content change is a tampering signal rather than an upgrade.
- Learn-mode observation may only be initiated by a human on the admin channel, and produces a proposal, never a confirmed baseline.

## Artifact

```rust
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,     // Log | BuildOutput | DependencyBundle | FetchManifest
                                // | Coverage | CommitProposal | Signature | Provenance
    pub produced_by: Option<TaskRunId>,
    pub mission: MissionId,
    pub trust_class: TrustClass,    // trust class of the context that produced it
    pub size_bytes: u64,
    pub blake3: String,
    pub content_ref: PathBuf,      // blob store path
    pub created_at: DateTime<Utc>,
    pub retain_until: Option<DateTime<Utc>>,
}
```

**Invariant**
Every artifact carries the trust class of the environment that produced it. A consumer must be able to tell whether content came from untrusted execution without inferring it from the artifact kind.

## Approval

```rust
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub mission: MissionId,
    pub lease: LeaseId,
    pub actor: ActorId,             // the requesting actor
    pub subject: ApprovalSubject,   // TaskEscalation | ScopeExpansion | LeaseRenewal
                                    // | BrokeredOperation { .. }
    pub request_digest: String,     // blake3 over the exact normalised request
    pub reason: String,
    pub alternatives: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,  // approvals go stale
}

pub struct ApprovalDecision {
    pub request: ApprovalId,
    pub decided_by: ActorId,        // must be Human, over the admin socket
    pub decision: Decision,         // ApproveOnce | ApproveForMission | Deny
    pub decided_at: DateTime<Utc>,
    pub note: Option<String>,
}
```

**Invariants**
- `request_digest` binds the approval to the exact operation. An approved push is approved for one commit, one remote, one refspec — a later request that differs in any of those does not match.
- `decided_by` must be a human actor authenticated on the admin socket ([D2](decisions.md#d2-per-actor-capability-tokens-with-a-separate-human-approval-channel)). This is enforced at the transport layer, not by checking a field.
- `ApproveForMission` records a policy relaxation scoped to one mission, and appears in mission review as such.
- An expired approval cannot be consumed. Consumption is single-use for `ApproveOnce`.

## Brokered operation

Introduced in Phase 4.

```rust
pub struct BrokeredOperation {
    pub id: String,
    pub mission: MissionId,
    pub lease: LeaseId,
    pub approval: ApprovalId,
    pub kind: BrokeredKind,     // GitPush { remote, refspec, commit } in the MVP
    pub state: BrokerOpState,   // requested | approved | executing | succeeded | failed | frozen
    pub result_summary: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}
```

**Invariants**
- No brokered operation executes without a matching, unexpired, unconsumed `ApprovalDecision` whose `request_digest` equals this operation's normalised digest.
- Mission revocation moves in-flight operations to `frozen` rather than cancelling silently.

## Policy decision

Recorded for every admission check, allowed or denied, so that "why did this happen" is answerable after the fact.

```rust
pub struct PolicyDecision {
    pub id: String,
    pub subject: PolicySubject,      // TaskRequest | Escalation | SubagentRequest | Publish
    pub outcome: PolicyOutcome,      // Allowed | AllowedWithApproval | Denied
    pub reasons: Vec<PolicyReason>,  // structured, not prose
    pub resolved_policy_digest: Option<String>,
    pub suggested_alternative: Option<String>,
    pub decided_at: DateTime<Utc>,
}
```

`PolicyReason` is a structured enum (`OutOfLeaseScope { path }`, `TaskNotInLease { task }`, `BudgetExhausted { dimension }`, `LeaseExpired`, `EgressWiderThanLease { .. }`, ...) so that denials can be rendered as actionable messages and asserted in tests.

## Audit events

```rust
pub struct AuditEvent {
    pub seq: u64,                   // monotonic, gapless per database
    pub at: DateTime<Utc>,
    pub kind: AuditEventKind,
    pub mission: Option<MissionId>,
    pub lease: Option<LeaseId>,
    pub actor: Option<ActorId>,
    pub task_run: Option<TaskRunId>,
    pub snapshot: Option<SnapshotId>,
    pub artifacts: Vec<ArtifactId>,
    pub approval: Option<ApprovalId>,
    pub payload: serde_json::Value,  // redacted at construction, never raw secrets
    pub prev_hash: String,           // blake3 of the previous event's canonical encoding
    pub hash: String,
}
```

**Invariants**
- Append-only. No update or delete path exists in the store API.
- `prev_hash` chains events, making truncation and in-place edits detectable.
- Payload construction goes through a redaction helper; there is no path that serialises a credential-bearing type into a payload, and that is enforced by not implementing `Serialize` on those types.

### Minimum event set for Phases 0-4
Mission proposed/approved/denied/activated/closed/revoked; lease issued/derived/renewed/expired/revoked; session bound/revoked; task requested/admitted/denied/started/finished; snapshot created; artifact stored; egress attempt allowed/denied; approval requested/decided/expired; brokered operation requested/executed/failed/frozen; policy config loaded (with digest and any rejected widening); baseline proposed/confirmed/amended/reset; **learn mode initiated** (distinctly, since it is a wide-scope run); access drift detected, by class.

## Persistence layout

SQLite, at `~/.local/share/clyde/db.sqlite`, with blobs alongside:

```text
~/.local/share/clyde/
  db.sqlite
  blobs/            content-addressed artifact payloads
  snapshots/        content store + materialised snapshot trees
  deps/             dependency bundles (read-only when mounted)
  missions/<id>/    per-mission writable cache (D3), deleted at closeout
  logs/             daemon logs
  run/              sockets: clyded.sock, clyded-admin.sock, brokerd.sock
```

### Tables

`workspaces`, `actors`, `missions`, `leases`, `actor_sessions`, `task_requests`, `task_runs`, `snapshots`, `snapshot_entries`, `artifacts`, `approval_requests`, `approval_decisions`, `broker_ops`, `policy_decisions`, `egress_attempts`, `access_baselines`, `baseline_paths`, `code_exec_inventory`, `audit_events`, `config_loads`.

Access baselines live here and nowhere else. They are deliberately not repository files: an attacker who could edit the baseline could conceal their own drift.

### Storage rules

- `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, `synchronous=FULL` for the audit and approval tables' transactions.
- Every state transition that spans entities (mission closeout, lease revocation, budget charge plus task admission) happens in one transaction.
- Migrations are versioned and forward-only from Phase 0, even though the schema is expected to churn — a schema that cannot be migrated cannot be dogfooded.
- The store crate exposes typed repository traits, so policy and mission logic are testable against an in-memory implementation with no SQLite dependency ([testability requirement O](requirements.md#o-testability)).

## Wire representation

The actor API (MCP, [D10](decisions.md#d10-mcp-is-the-primary-actor-facing-api)) and admin API (JSON-RPC) share one serialisation of these types, with two rules:

1. **Actor-facing responses are filtered.** An actor sees its own lease, its own tasks, and its own artifacts. It does not see other actors' tokens, other missions, host paths outside its sandbox view, or audit payloads.
2. **Host paths are never leaked into actor-facing responses.** Paths are rendered workspace-relative or as sandbox-internal paths.

Both rules are properties of dedicated view types, not of ad hoc field skipping, so that adding a field to a domain type cannot accidentally widen what an agent can see.
