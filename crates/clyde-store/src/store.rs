//! The repository traits.
//!
//! One trait, with coarse-grained operations rather than a general transaction
//! handle. That is deliberate: every state transition that spans entities —
//! mission closeout, lease revocation, budget charge plus task admission — is a
//! single method here, so it is transactional by construction and a caller
//! cannot perform half of one (schema reference: storage rules).

use chrono::{DateTime, Utc};
use std::path::Path;

use clyde_core::actor::Actor;
use clyde_core::approval::{ApprovalDecision, ApprovalRequest};
use clyde_core::artifact::Artifact;
use clyde_core::audit::{AuditChainHead, AuditEvent, AuditEventDraft};
use clyde_core::baseline::{AccessBaseline, BaselineKey, BaselineProposal};
use clyde_core::broker::{BrokerOpState, BrokeredOperation};
use clyde_core::budget::{BudgetCost, BudgetUsage};
use clyde_core::decision::PolicyDecision;
use clyde_core::egress::EgressAttempt;
use clyde_core::ids::{
    ActorId, ApprovalId, ArtifactId, BrokerOpId, LeaseId, MissionId, SnapshotId, TaskRunId,
    WorkspaceId,
};
use clyde_core::lease::{Lease, LeaseState};
use clyde_core::mission::{Mission, MissionState};
use clyde_core::session::{ActorSession, TokenHash};
use clyde_core::snapshot::Snapshot;
use clyde_core::task::{TaskOutcome, TaskRun, TaskRunState, TaskType};
use clyde_core::workspace::Workspace;

use crate::error::Result;
use crate::types::{
    ApprovalRecord, AuditFilter, ConfigLoad, LeaseRenewal, MissionCloseout, ResolvedSession,
};

/// The persistence interface.
///
/// Implementations must be safe to share across tasks. Methods are synchronous:
/// the operations are short local transactions, and an async signature would
/// invite holding a lock across an await point.
pub trait Store: Send + Sync + std::fmt::Debug {
    // -- workspaces ------------------------------------------------------
    fn register_workspace(&self, workspace: Workspace) -> Result<()>;
    fn get_workspace(&self, id: &WorkspaceId) -> Result<Workspace>;
    fn find_workspace_by_root(&self, root: &Path) -> Result<Option<Workspace>>;
    fn list_workspaces(&self) -> Result<Vec<Workspace>>;
    fn set_workspace_policy_digest(
        &self,
        id: &WorkspaceId,
        digest: Option<clyde_core::Digest>,
    ) -> Result<()>;

    // -- actors ----------------------------------------------------------
    fn upsert_actor(&self, actor: Actor) -> Result<()>;
    fn get_actor(&self, id: &ActorId) -> Result<Option<Actor>>;

    // -- missions --------------------------------------------------------
    /// Creates a mission, enforcing at most one non-terminal mission per
    /// workspace (D16) inside the same transaction as the insert.
    fn create_mission(&self, mission: Mission) -> Result<()>;
    fn get_mission(&self, id: &MissionId) -> Result<Mission>;
    fn list_missions(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<Mission>>;
    /// Finds the workspace's non-terminal mission, if any.
    fn active_mission(&self, workspace: &WorkspaceId) -> Result<Option<Mission>>;
    /// Applies a state transition, rejecting one the state machine forbids.
    fn transition_mission(&self, id: &MissionId, to: MissionState) -> Result<Mission>;
    fn set_mission_cache_dir(
        &self,
        id: &MissionId,
        cache_dir: Option<std::path::PathBuf>,
    ) -> Result<()>;
    /// Closes a mission: revokes every lease and session, records the closing
    /// diff and summary, and applies the terminal transition, in one
    /// transaction.
    fn close_mission(&self, closeout: MissionCloseout) -> Result<Mission>;

    // -- leases ----------------------------------------------------------
    fn insert_lease(&self, lease: Lease) -> Result<()>;
    fn get_lease(&self, id: &LeaseId) -> Result<Lease>;
    fn list_leases(&self, mission: &MissionId) -> Result<Vec<Lease>>;
    fn set_lease_state(&self, id: &LeaseId, state: LeaseState) -> Result<Lease>;
    /// Revokes a lease, every lease derived from it, and every session bound to
    /// any of them, in one transaction. Returns the revoked lease identifiers.
    fn revoke_lease_tree(&self, id: &LeaseId, at: DateTime<Utc>) -> Result<Vec<LeaseId>>;
    /// Charges a lease's budget atomically, returning the new usage.
    ///
    /// Read-modify-write happens inside the transaction so two concurrent
    /// admissions cannot both see the same headroom.
    fn charge_lease(&self, id: &LeaseId, cost: &BudgetCost) -> Result<BudgetUsage>;
    /// Releases a parallel sub-agent slot without releasing cumulative usage.
    fn release_parallel_slot(&self, id: &LeaseId) -> Result<BudgetUsage>;
    /// Supersedes a lease and inserts its replacement in one transaction, so a
    /// renewal shows in the audit trail as an event rather than as a mutated
    /// expiry.
    fn renew_lease(&self, renewal: LeaseRenewal) -> Result<Lease>;

    // -- sessions --------------------------------------------------------
    fn bind_session(&self, session: ActorSession) -> Result<()>;
    /// Resolves a token to its session, lease, and mission.
    ///
    /// Returns `None` for unknown, expired, and revoked tokens alike: the caller
    /// must not be able to tell which (Phase 1 deliverable 4).
    fn resolve_token(
        &self,
        hash: &TokenHash,
        now: DateTime<Utc>,
    ) -> Result<Option<ResolvedSession>>;
    fn list_sessions(&self, mission: &MissionId) -> Result<Vec<ActorSession>>;
    fn revoke_sessions_for_lease(&self, lease: &LeaseId, at: DateTime<Utc>) -> Result<usize>;
    fn set_session_sandbox(&self, lease: &LeaseId, sandbox: Option<String>) -> Result<()>;

    // -- tasks -----------------------------------------------------------
    fn insert_task_run(&self, run: TaskRun) -> Result<()>;
    fn get_task_run(&self, id: &TaskRunId) -> Result<TaskRun>;
    fn list_task_runs(&self, mission: &MissionId) -> Result<Vec<TaskRun>>;
    fn transition_task_run(&self, id: &TaskRunId, to: TaskRunState) -> Result<TaskRun>;
    fn complete_task_run(
        &self,
        id: &TaskRunId,
        state: TaskRunState,
        outcome: TaskOutcome,
        finished_at: DateTime<Utc>,
        artifacts: Vec<ArtifactId>,
    ) -> Result<TaskRun>;
    fn set_task_run_snapshot(&self, id: &TaskRunId, snapshot: SnapshotId) -> Result<()>;
    fn set_task_run_bundle(&self, id: &TaskRunId, bundle: ArtifactId) -> Result<()>;

    // -- policy decisions ------------------------------------------------
    fn record_policy_decision(&self, decision: PolicyDecision) -> Result<()>;
    fn list_policy_decisions(&self, mission: &MissionId) -> Result<Vec<PolicyDecision>>;

    // -- snapshots -------------------------------------------------------
    fn insert_snapshot(&self, snapshot: Snapshot) -> Result<()>;
    fn get_snapshot(&self, id: &SnapshotId) -> Result<Snapshot>;
    /// Whether a snapshot contains a path.
    ///
    /// Path enforcement is by materialisation, so this is also the question
    /// "could the task have read it" — which is how the Phase 3 property that a
    /// fetch snapshot contains no application source is asserted.
    fn snapshot_contains(&self, id: &SnapshotId, path: &clyde_core::RepoPath) -> Result<bool>;

    // -- artifacts -------------------------------------------------------
    fn insert_artifact(&self, artifact: Artifact) -> Result<()>;
    fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact>;
    fn list_artifacts(&self, mission: &MissionId) -> Result<Vec<Artifact>>;

    // -- approvals -------------------------------------------------------
    fn insert_approval_request(&self, request: ApprovalRequest) -> Result<()>;
    fn get_approval(&self, id: &ApprovalId) -> Result<ApprovalRecord>;
    fn list_pending_approvals(&self, now: DateTime<Utc>) -> Result<Vec<ApprovalRequest>>;
    fn list_approvals(&self, mission: &MissionId) -> Result<Vec<ApprovalRecord>>;
    fn record_approval_decision(&self, decision: ApprovalDecision) -> Result<()>;
    /// Finds an approval whose digest matches and which still authorises.
    fn find_authorising_approval(
        &self,
        mission: &MissionId,
        digest: &clyde_core::Digest,
        now: DateTime<Utc>,
    ) -> Result<Option<ApprovalRecord>>;
    /// Marks an `ApproveOnce` decision consumed. Single-use: a second call fails.
    fn consume_approval(&self, id: &ApprovalId, at: DateTime<Utc>) -> Result<()>;

    // -- brokered operations ---------------------------------------------
    fn insert_broker_op(&self, op: BrokeredOperation) -> Result<()>;
    fn get_broker_op(&self, id: &BrokerOpId) -> Result<BrokeredOperation>;
    fn list_broker_ops(&self, mission: &MissionId) -> Result<Vec<BrokeredOperation>>;
    fn transition_broker_op(
        &self,
        id: &BrokerOpId,
        to: BrokerOpState,
        summary: Option<String>,
        at: DateTime<Utc>,
    ) -> Result<BrokeredOperation>;
    /// Freezes every in-flight brokered operation for a mission, rather than
    /// cancelling them silently.
    fn freeze_broker_ops(&self, mission: &MissionId, at: DateTime<Utc>) -> Result<Vec<BrokerOpId>>;

    // -- egress ----------------------------------------------------------
    fn insert_egress_attempt(&self, attempt: EgressAttempt) -> Result<()>;
    fn list_egress_attempts(&self, task_run: &TaskRunId) -> Result<Vec<EgressAttempt>>;
    fn list_mission_egress_attempts(&self, mission: &MissionId) -> Result<Vec<EgressAttempt>>;

    // -- access baselines ------------------------------------------------
    fn put_baseline(&self, baseline: AccessBaseline) -> Result<()>;
    fn get_baseline(&self, key: &BaselineKey) -> Result<Option<AccessBaseline>>;
    fn list_baselines(&self, workspace: &WorkspaceId) -> Result<Vec<AccessBaseline>>;
    fn delete_baseline(&self, key: &BaselineKey) -> Result<bool>;
    fn put_baseline_proposal(&self, proposal: BaselineProposal) -> Result<()>;
    fn get_baseline_proposal(&self, key: &BaselineKey) -> Result<Option<BaselineProposal>>;
    fn delete_baseline_proposal(&self, key: &BaselineKey) -> Result<bool>;

    // -- audit -----------------------------------------------------------
    /// Appends an event, assigning the sequence number and chaining it to the
    /// current head inside the transaction. There is no update or delete path.
    fn append_audit(&self, draft: AuditEventDraft) -> Result<AuditEvent>;
    fn list_audit(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>>;
    fn audit_head(&self) -> Result<Option<AuditChainHead>>;
    /// Verifies the whole chain against the recorded head.
    fn verify_audit(&self) -> Result<()>;

    // -- config loads ----------------------------------------------------
    fn record_config_load(&self, load: ConfigLoad) -> Result<()>;
    fn list_config_loads(&self, workspace: Option<&WorkspaceId>) -> Result<Vec<ConfigLoad>>;

    // -- dependency bundles ---------------------------------------------
    /// Records that a dependency bundle satisfies a lockfile digest, so a later
    /// build can state which bundle it used.
    fn record_bundle(&self, record: crate::types_bundle::BundleRecord) -> Result<()>;
    fn find_bundle_for_lockfile(
        &self,
        lockfile_digest: &clyde_core::Digest,
    ) -> Result<Option<crate::types_bundle::BundleRecord>>;
    fn list_bundles(&self) -> Result<Vec<crate::types_bundle::BundleRecord>>;

    /// The confirmed inventory a bundle was last approved with, if any.
    ///
    /// Phase 3 gates builds against a bundle whose inventory diff is
    /// unconfirmed, and this is where that confirmation lives.
    fn set_bundle_inventory_confirmed(
        &self,
        bundle: &ArtifactId,
        confirmed_by: ActorId,
        at: DateTime<Utc>,
    ) -> Result<()>;
    fn is_bundle_inventory_confirmed(&self, bundle: &ArtifactId) -> Result<bool>;

    /// Whether a task type is recorded as having run successfully against the
    /// current tree, used as the push prompt's task evidence.
    fn passing_task_evidence(&self, mission: &MissionId) -> Result<Vec<(TaskType, TaskRunId)>>;
}
