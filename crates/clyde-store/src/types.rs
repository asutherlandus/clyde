//! Store-facing value types: filters, composite results, and the inputs to
//! multi-entity transactions.

use chrono::{DateTime, Utc};
use clyde_core::audit::AuditEventKind;
use clyde_core::ids::{ActorId, ApprovalId, LeaseId, MissionId, TaskRunId, WorkspaceId};
use clyde_core::lease::Lease;
use clyde_core::mission::Mission;
use clyde_core::session::ActorSession;
use serde::{Deserialize, Serialize};

/// What a valid session token resolves to.
///
/// Every actor request resolves token → session → lease → mission before any
/// other check (Phase 1 deliverable 4), so the store returns all three together
/// and the caller cannot forget one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSession {
    pub session: ActorSession,
    pub lease: Lease,
    pub mission: Mission,
}

/// Filters for an audit query.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub mission: Option<MissionId>,
    pub workspace: Option<WorkspaceId>,
    pub actor: Option<ActorId>,
    pub task_run: Option<TaskRunId>,
    pub kinds: Vec<&'static str>,
    pub since_seq: Option<u64>,
    pub limit: Option<usize>,
}

impl AuditFilter {
    pub fn for_mission(mission: MissionId) -> Self {
        Self {
            mission: Some(mission),
            ..Self::default()
        }
    }

    pub(crate) fn matches(&self, event: &clyde_core::audit::AuditEvent) -> bool {
        if let Some(mission) = &self.mission
            && event.mission.as_ref() != Some(mission)
        {
            return false;
        }
        if let Some(workspace) = &self.workspace
            && event.workspace.as_ref() != Some(workspace)
        {
            return false;
        }
        if let Some(actor) = &self.actor
            && event.actor.as_ref() != Some(actor)
        {
            return false;
        }
        if let Some(task_run) = &self.task_run
            && event.task_run.as_ref() != Some(task_run)
        {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&event.kind.name()) {
            return false;
        }
        if let Some(since) = self.since_seq
            && event.seq < since
        {
            return false;
        }
        true
    }
}

/// Everything mission closeout records, applied in one transaction.
///
/// Closeout revokes leases, revokes sessions, stores the closing diff, and
/// writes the summary together, so a crash cannot leave a mission closed with
/// live sessions (Phase 1 deliverable 2).
#[derive(Debug, Clone)]
pub struct MissionCloseout {
    pub mission: MissionId,
    pub final_state: clyde_core::mission::MissionState,
    pub closed_at: DateTime<Utc>,
    /// Unified diff of the mission's changes, stored as an artifact by the
    /// caller; the identifier is recorded here.
    pub closing_diff: Option<clyde_core::ids::ArtifactId>,
    pub summary: String,
}

/// A recorded approval, with its decision if one exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub request: clyde_core::approval::ApprovalRequest,
    pub decision: Option<clyde_core::approval::ApprovalDecision>,
}

impl ApprovalRecord {
    /// Whether this record authorises an action with `digest` at `now`.
    pub fn authorises(&self, digest: &clyde_core::Digest, now: DateTime<Utc>) -> bool {
        self.decision
            .as_ref()
            .is_some_and(|decision| decision.authorises(&self.request, digest, now))
    }
}

/// A configuration load, for the `config_loads` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigLoad {
    pub workspace: Option<WorkspaceId>,
    pub source: String,
    pub digest: clyde_core::Digest,
    pub rejected_keys: Vec<String>,
    pub loaded_at: DateTime<Utc>,
}

/// A lease renewal: the replacement lease plus the identifier it supersedes.
#[derive(Debug, Clone)]
pub struct LeaseRenewal {
    pub superseded: LeaseId,
    pub replacement: Lease,
}

/// Which audit events a mission timeline should include.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineDetail {
    /// High-signal events only: approvals, drift, denied egress, brokered work.
    HighSignal,
    Full,
}

impl TimelineDetail {
    pub fn includes(self, kind: &AuditEventKind) -> bool {
        match self {
            Self::Full => true,
            Self::HighSignal => kind.is_high_signal(),
        }
    }
}

/// A pending approval as shown by `clyde approvals list`.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request: clyde_core::approval::ApprovalRequest,
    pub mission_objective: String,
}

/// Identifier of an approval awaiting a decision.
pub type PendingApprovalId = ApprovalId;
