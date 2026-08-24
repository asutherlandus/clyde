//! Missions: the unit of delegated work a human approves.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::budget::Budget;
use crate::entities::classification::{CredentialPolicy, EgressProfile};
use crate::entities::task::TaskType;
use crate::error::{TransitionError, ValidationError};
use crate::ids::{ActorId, MissionId, WorkspaceId};
use crate::repo_path::RepoPath;

/// The repository surface a mission or lease covers.
///
/// `edit_paths` is the writable surface and, inside a workspace environment,
/// *is* the edit-scope enforcement: it becomes the read-write bind set (D1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionScope {
    pub edit_paths: BTreeSet<RepoPath>,
    /// Additional read-only context beyond the edit paths.
    pub read_paths: BTreeSet<RepoPath>,
}

impl MissionScope {
    /// Whether `path` may be written under this scope.
    pub fn may_write(&self, path: &RepoPath) -> bool {
        crate::repo_path::is_within_any(path, self.edit_paths.iter())
    }

    /// Whether `path` may be read. Edit paths are readable by construction.
    pub fn may_read(&self, path: &RepoPath) -> bool {
        self.may_write(path) || crate::repo_path::is_within_any(path, self.read_paths.iter())
    }

    /// Every path the scope admits, edit and read.
    pub fn all_paths(&self) -> impl Iterator<Item = &RepoPath> {
        self.edit_paths.iter().chain(self.read_paths.iter())
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.edit_paths.is_empty() && self.read_paths.is_empty() {
            return Err(ValidationError::EmptyField { field: "scope" });
        }
        Ok(())
    }

    /// Whether every path in `self` is admitted by `parent` at the same or a
    /// lower authority. This is derivation rules 1 and 2 in one place, so the
    /// two cannot drift apart.
    pub fn is_derivable_from(&self, parent: &MissionScope) -> Result<(), ScopeViolation> {
        if let Some(path) = self.edit_paths.iter().find(|path| !parent.may_write(path)) {
            return Err(ScopeViolation::EditNotInParent { path: path.clone() });
        }
        if let Some(path) = self.read_paths.iter().find(|path| !parent.may_read(path)) {
            return Err(ScopeViolation::ReadNotInParent { path: path.clone() });
        }
        Ok(())
    }
}

/// Why a scope was not derivable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeViolation {
    EditNotInParent { path: RepoPath },
    ReadNotInParent { path: RepoPath },
}

/// Conditions that stop a mission without a human intervening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    BudgetExhausted,
    Expiry,
    RepeatedTaskFailure,
    AccessDriftDetected,
    EgressRefusal,
}

/// Network ceiling for a mission. A lease may narrow it, never widen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicy {
    /// Ceiling for the mission's primary lease and every derived lease.
    pub ceiling: EgressProfile,
}

/// When approval is required within a mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    /// Task types pre-approved for this mission, which still may not exceed the
    /// mission's `allowed_tasks`.
    pub pre_approved_tasks: BTreeSet<TaskType>,
    /// Whether `ApproveForMission` decisions are accepted at all.
    pub allow_mission_scoped_approvals: bool,
}

/// Mission state (schema reference: mission states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    Proposed,
    AwaitingApproval,
    Active,
    Paused,
    BlockedOnEscalation,
    Completed,
    Denied,
    Revoked,
    Expired,
    Failed,
    /// A new mission supersedes this one.
    Revised,
}

impl std::fmt::Display for MissionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Proposed => "proposed",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::BlockedOnEscalation => "blocked_on_escalation",
            Self::Completed => "completed",
            Self::Denied => "denied",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Failed => "failed",
            Self::Revised => "revised",
        };
        f.write_str(text)
    }
}

impl MissionState {
    /// Terminal states are final; nothing leaves them.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Denied
                | Self::Revoked
                | Self::Expired
                | Self::Failed
                | Self::Revised
        )
    }

    /// Whether an actor may perform new work in this state.
    pub fn permits_work(self) -> bool {
        matches!(self, Self::Active)
    }

    /// The explicit transition function. Illegal transitions are values a caller
    /// must handle, not assignments that silently succeed (Phase 0 deliverable 4).
    pub fn transition(self, to: MissionState) -> Result<MissionState, TransitionError> {
        if self.is_terminal() {
            return Err(TransitionError::terminal("mission", self, to));
        }
        let permitted = match (self, to) {
            (Self::Proposed, Self::AwaitingApproval | Self::Revised | Self::Denied) => true,
            (Self::AwaitingApproval, Self::Active | Self::Denied | Self::Revised) => true,
            (
                Self::Active,
                Self::Completed
                | Self::Revoked
                | Self::Expired
                | Self::Failed
                | Self::Paused
                | Self::BlockedOnEscalation,
            ) => true,
            (
                Self::Paused | Self::BlockedOnEscalation,
                Self::Active | Self::Revoked | Self::Expired | Self::Failed,
            ) => true,
            _ => false,
        };
        if permitted {
            Ok(to)
        } else {
            Err(TransitionError::not_permitted("mission", self, to))
        }
    }
}

const MAX_OBJECTIVE: usize = 4096;

/// A mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mission {
    pub id: MissionId,
    pub workspace: WorkspaceId,
    pub objective: String,
    /// The human who created the mission.
    pub initiator: ActorId,
    /// The agent the mission is delegated to.
    pub primary_actor: ActorId,
    pub scope: MissionScope,
    pub allowed_tasks: BTreeSet<TaskType>,
    pub network_policy: NetworkPolicy,
    pub credential_policy: CredentialPolicy,
    pub approval_policy: ApprovalPolicy,
    pub budget: Budget,
    pub expiry: DateTime<Utc>,
    pub state: MissionState,
    pub stop_conditions: BTreeSet<StopCondition>,
    /// Human-readable; not machine-evaluated in the MVP.
    pub success_criteria: Vec<String>,
    /// Per-mission writable cache (D3), present once activated.
    pub cache_dir: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl Mission {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.objective.trim().is_empty() {
            return Err(ValidationError::EmptyField { field: "objective" });
        }
        if self.objective.len() > MAX_OBJECTIVE {
            return Err(ValidationError::FieldTooLong {
                field: "objective",
                max: MAX_OBJECTIVE,
            });
        }
        if !self.initiator.is_human() {
            return Err(ValidationError::NotHumanActor {
                actor: self.initiator.to_string(),
            });
        }
        if self.primary_actor.is_human() {
            return Err(ValidationError::MalformedId {
                kind: "actor",
                value: self.primary_actor.to_string(),
                expected: "an agent identifier for the mission's primary actor",
            });
        }
        if self.allowed_tasks.is_empty() {
            return Err(ValidationError::EmptyField {
                field: "allowed_tasks",
            });
        }
        if self.expiry <= self.created_at {
            return Err(ValidationError::ExpiryNotAfterIssue {
                issued_at: self.created_at.to_rfc3339(),
                expires_at: self.expiry.to_rfc3339(),
            });
        }
        self.scope.validate()?;
        self.budget.validate()?;
        // Pre-approvals cannot reach outside the envelope: approving a mission
        // must approve exactly what is shown, with nothing decided afterwards.
        if let Some(task) = self
            .approval_policy
            .pre_approved_tasks
            .iter()
            .find(|task| !self.allowed_tasks.contains(task))
        {
            return Err(ValidationError::MalformedId {
                kind: "task",
                value: task.name().to_owned(),
                expected: "a pre-approved task inside the mission's allowed_tasks",
            });
        }
        Ok(())
    }

    /// Whether the mission has expired as of `now`.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expiry
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;
    use crate::duration::HumanDuration;
    use crate::ids;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn scope() -> MissionScope {
        MissionScope {
            edit_paths: [path("crates/core")].into_iter().collect(),
            read_paths: [path("docs")].into_iter().collect(),
        }
    }

    fn mission() -> Mission {
        let created = Utc::now();
        Mission {
            id: ids::new::mission_id().unwrap(),
            workspace: ids::new::workspace_id().unwrap(),
            objective: "tidy the core crate".to_owned(),
            initiator: ActorId::parse("human:andrew").unwrap(),
            primary_actor: ActorId::parse("agent:claude").unwrap(),
            scope: scope(),
            allowed_tasks: [TaskType::WorkspaceEdit, TaskType::RustCheck]
                .into_iter()
                .collect(),
            network_policy: NetworkPolicy {
                ceiling: EgressProfile::ModelApi,
            },
            credential_policy: CredentialPolicy::None,
            approval_policy: ApprovalPolicy {
                pre_approved_tasks: BTreeSet::new(),
                allow_mission_scoped_approvals: true,
            },
            budget: Budget {
                max_duration: HumanDuration::parse("2h").unwrap(),
                max_task_runs: 20,
                max_parallel_subagents: 1,
                max_subagents: 2,
                max_cpu_seconds: 1800,
                max_cache_bytes: 1 << 30,
                max_artifact_bytes: 1 << 28,
                max_egress_bytes: 1 << 20,
                max_egress_requests: 100,
            },
            expiry: created + chrono::Duration::hours(2),
            state: MissionState::Proposed,
            stop_conditions: [StopCondition::BudgetExhausted].into_iter().collect(),
            success_criteria: vec!["tests pass".to_owned()],
            cache_dir: None,
            created_at: created,
            closed_at: None,
        }
    }

    #[test]
    fn scope_write_and_read_are_subtree_scoped() {
        let scope = scope();
        assert!(scope.may_write(&path("crates/core/src/lib.rs")));
        assert!(!scope.may_write(&path("crates/policy/src/lib.rs")));
        assert!(scope.may_read(&path("docs/readme.md")));
        assert!(scope.may_read(&path("crates/core/src/lib.rs")));
        assert!(!scope.may_read(&path("secrets/key")));
    }

    #[test]
    fn derived_scope_may_not_widen_edit_or_read() {
        let parent = scope();
        let narrower = MissionScope {
            edit_paths: [path("crates/core/src")].into_iter().collect(),
            read_paths: BTreeSet::new(),
        };
        assert!(narrower.is_derivable_from(&parent).is_ok());

        let wider_edit = MissionScope {
            edit_paths: [path("crates")].into_iter().collect(),
            read_paths: BTreeSet::new(),
        };
        assert!(matches!(
            wider_edit.is_derivable_from(&parent),
            Err(ScopeViolation::EditNotInParent { .. })
        ));

        // Rule 2: a child may read what the parent may edit.
        let read_from_parent_edit = MissionScope {
            edit_paths: BTreeSet::new(),
            read_paths: [path("crates/core")].into_iter().collect(),
        };
        assert!(read_from_parent_edit.is_derivable_from(&parent).is_ok());

        let wider_read = MissionScope {
            edit_paths: BTreeSet::new(),
            read_paths: [path("infra")].into_iter().collect(),
        };
        assert!(matches!(
            wider_read.is_derivable_from(&parent),
            Err(ScopeViolation::ReadNotInParent { .. })
        ));
    }

    #[test]
    fn state_machine_follows_the_documented_diagram() {
        use MissionState::*;
        assert_eq!(
            Proposed.transition(AwaitingApproval).unwrap(),
            AwaitingApproval
        );
        assert_eq!(AwaitingApproval.transition(Active).unwrap(), Active);
        assert_eq!(Active.transition(Paused).unwrap(), Paused);
        assert_eq!(Paused.transition(Active).unwrap(), Active);
        assert_eq!(
            Active.transition(BlockedOnEscalation).unwrap(),
            BlockedOnEscalation
        );
        assert_eq!(BlockedOnEscalation.transition(Active).unwrap(), Active);
        assert_eq!(Active.transition(Completed).unwrap(), Completed);
    }

    #[test]
    fn terminal_states_are_final() {
        use MissionState::*;
        for terminal in [Completed, Denied, Revoked, Expired, Failed, Revised] {
            assert!(terminal.is_terminal());
            let err = terminal
                .transition(Active)
                .expect_err("terminal must not reopen");
            assert_eq!(err.reason, crate::error::TransitionReason::Terminal);
        }
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        use MissionState::*;
        // Skipping approval is the transition that must never be possible.
        assert!(Proposed.transition(Active).is_err());
        assert!(AwaitingApproval.transition(Completed).is_err());
        assert!(Active.transition(AwaitingApproval).is_err());
    }

    #[test]
    fn only_active_permits_work() {
        use MissionState::*;
        assert!(Active.permits_work());
        for state in [
            Proposed,
            AwaitingApproval,
            Paused,
            BlockedOnEscalation,
            Revoked,
        ] {
            assert!(!state.permits_work(), "{state} must not permit work");
        }
    }

    #[test]
    fn validation_rejects_non_human_initiator_and_human_agent() {
        let mut m = mission();
        assert!(m.validate().is_ok());
        m.initiator = ActorId::parse("agent:claude").unwrap();
        assert!(matches!(
            m.validate(),
            Err(ValidationError::NotHumanActor { .. })
        ));
        let mut m = mission();
        m.primary_actor = ActorId::parse("human:andrew").unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validation_rejects_pre_approval_outside_the_envelope() {
        let mut m = mission();
        m.approval_policy.pre_approved_tasks = [TaskType::GitPush].into_iter().collect();
        assert!(
            m.validate().is_err(),
            "a pre-approval outside allowed_tasks would decide a field after approval"
        );
    }

    #[test]
    fn validation_rejects_expiry_before_creation() {
        let mut m = mission();
        m.expiry = m.created_at;
        assert!(matches!(
            m.validate(),
            Err(ValidationError::ExpiryNotAfterIssue { .. })
        ));
    }

    #[test]
    fn serde_round_trips() {
        let m = mission();
        let json = serde_json::to_string(&m).unwrap();
        let back: Mission = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
