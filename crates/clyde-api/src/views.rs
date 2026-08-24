//! Actor-facing view types.
//!
//! Two rules, and both are properties of these types rather than of ad hoc field
//! skipping — so adding a field to a domain type cannot accidentally widen what
//! an agent can see (schema reference: wire representation):
//!
//! 1. **Responses are filtered.** An actor sees its own lease, its own tasks,
//!    and its own artifacts. It does not see other actors' tokens, other
//!    missions, host paths, or audit payloads.
//! 2. **Host paths never leak.** Paths are rendered workspace-relative or as
//!    sandbox-internal paths.

use chrono::{DateTime, Utc};
use clyde_core::artifact::Artifact;
use clyde_core::budget::{Budget, BudgetUsage};
use clyde_core::decision::PolicyReason;
use clyde_core::lease::Lease;
use clyde_core::mission::Mission;
use clyde_core::task::{TaskRun, TaskType};
use serde::{Deserialize, Serialize};

/// What `mission_status` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionView {
    pub mission: String,
    pub objective: String,
    pub state: String,
    pub edit_paths: Vec<String>,
    pub read_paths: Vec<String>,
    pub allowed_tasks: Vec<String>,
    pub egress_profile: String,
    pub expires_at: DateTime<Utc>,
    pub budget: BudgetView,
}

impl MissionView {
    /// Builds the view from a mission and the lease the actor holds.
    ///
    /// Scope comes from the *lease*, not the mission, so a sub-agent is told what
    /// it can actually do rather than what its parent could.
    pub fn new(mission: &Mission, lease: &Lease) -> Self {
        Self {
            mission: mission.id.to_string(),
            objective: mission.objective.clone(),
            state: mission.state.to_string(),
            edit_paths: lease
                .repo_scope
                .edit_paths
                .iter()
                .map(ToString::to_string)
                .collect(),
            read_paths: lease
                .repo_scope
                .read_paths
                .iter()
                .map(ToString::to_string)
                .collect(),
            allowed_tasks: lease
                .task_scope
                .iter()
                .map(|task| task.name().to_owned())
                .collect(),
            egress_profile: lease.network_scope.to_string(),
            expires_at: lease.expires_at,
            budget: BudgetView::new(&lease.budget, &lease.usage),
        }
    }
}

/// Remaining budget, in the dimensions an actor can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetView {
    pub task_runs_remaining: u32,
    pub subagents_remaining: u8,
    pub seconds_remaining: u64,
    pub egress_requests_remaining: u32,
    pub egress_bytes_remaining: u64,
}

impl BudgetView {
    pub fn new(budget: &Budget, usage: &BudgetUsage) -> Self {
        let remaining = budget.remaining(usage);
        Self {
            task_runs_remaining: remaining.task_runs,
            subagents_remaining: remaining.subagents,
            seconds_remaining: remaining.duration_seconds,
            egress_requests_remaining: remaining.egress_requests,
            egress_bytes_remaining: remaining.egress_bytes,
        }
    }
}

/// One entry from `list_capabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityView {
    pub task: String,
    /// Whether it can be run now, with no further approval.
    pub available: bool,
    /// Whether an escalation could make it available.
    pub escalation_possible: bool,
    /// What this task would do to the network.
    pub egress_profile: String,
    /// Whether a human must approve each run.
    pub requires_approval: bool,
    /// Why it is unavailable, where it is.
    pub reason: Option<String>,
    /// The narrower or escalated alternative, where one exists.
    pub alternative: Option<String>,
}

/// What `task_status` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRunView {
    pub task_run: String,
    pub task: String,
    pub state: String,
    /// The digest of the policy actually applied, so "which policy ran" is
    /// answerable from the result rather than inferred.
    pub policy_digest: String,
    pub backend: String,
    pub snapshot: Option<String>,
    pub dependency_bundle: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub classification: Option<String>,
    pub exit_code: Option<i32>,
    pub summary: Option<String>,
    pub artifacts: Vec<String>,
}

impl TaskRunView {
    pub fn new(run: &TaskRun) -> Self {
        Self {
            task_run: run.id.to_string(),
            task: run.request.task.name().to_owned(),
            state: run.state.to_string(),
            policy_digest: run.policy_digest.to_string(),
            backend: run.backend.to_string(),
            snapshot: run.snapshot.as_ref().map(ToString::to_string),
            dependency_bundle: run.dependency_bundle.as_ref().map(ToString::to_string),
            started_at: run.started_at,
            finished_at: run.finished_at,
            classification: run
                .outcome
                .as_ref()
                .map(|outcome| outcome.classification.name().to_owned()),
            exit_code: run.outcome.as_ref().and_then(|outcome| outcome.exit_code),
            summary: run.outcome.as_ref().map(|outcome| outcome.summary.clone()),
            artifacts: run.artifacts.iter().map(ToString::to_string).collect(),
        }
    }
}

/// What `list_artifacts` returns.
///
/// Deliberately without `content_ref`: artifacts are read through the API, not
/// through a mounted store, so an agent cannot read another mission's outputs by
/// walking a filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactView {
    pub artifact: String,
    pub kind: String,
    pub produced_by: Option<String>,
    /// Trust class of the environment that produced it, so a consumer can tell
    /// whether content came from untrusted execution.
    pub trust_class: String,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

impl ArtifactView {
    pub fn new(artifact: &Artifact) -> Self {
        Self {
            artifact: artifact.id.to_string(),
            kind: format!("{:?}", artifact.kind),
            produced_by: artifact.produced_by.as_ref().map(ToString::to_string),
            trust_class: artifact.trust_class.to_string(),
            size_bytes: artifact.size_bytes,
            created_at: artifact.created_at,
        }
    }
}

/// A structured denial, as an actor receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialView {
    /// What was denied.
    pub denied: String,
    /// Which constraints denied it.
    pub reasons: Vec<String>,
    /// What to do instead.
    pub alternatives: Vec<String>,
}

impl DenialView {
    pub fn new(denied: impl Into<String>, reasons: &[PolicyReason]) -> Self {
        Self {
            denied: denied.into(),
            reasons: reasons.iter().map(PolicyReason::render).collect(),
            alternatives: reasons
                .iter()
                .filter_map(PolicyReason::suggested_alternative)
                .collect(),
        }
    }
}

/// A ranged slice of a task's logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogView {
    pub task_run: String,
    pub stream: String,
    /// Byte offset this slice starts at, so a caller can page.
    pub offset: u64,
    pub content: String,
    /// Whether the log was truncated, recorded rather than silent.
    pub truncated: bool,
    /// Whether the task is still running, so a caller knows to poll again.
    pub complete: bool,
}

/// The result of an escalation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationView {
    pub approval: String,
    pub subject: String,
    /// What the human will be shown, so the agent knows what it asked for.
    pub summary: String,
    pub expires_at: DateTime<Utc>,
}

/// A sub-agent session, as returned to the requesting actor.
///
/// The token is written into the sub-agent's sandbox, never returned here: no
/// query returns a token (Phase 1 deliverable 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentView {
    pub actor: String,
    pub lease: String,
    pub edit_paths: Vec<String>,
    pub allowed_tasks: Vec<String>,
    pub expires_at: DateTime<Utc>,
}

/// Whether a task type would need an escalation from this lease.
pub fn capability_for(
    task: TaskType,
    available: bool,
    escalation_possible: bool,
    egress_profile: String,
    requires_approval: bool,
    reason: Option<&PolicyReason>,
) -> CapabilityView {
    CapabilityView {
        task: task.name().to_owned(),
        available,
        escalation_possible,
        egress_profile,
        requires_approval,
        reason: reason.map(PolicyReason::render),
        alternative: reason.and_then(PolicyReason::suggested_alternative),
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
    use std::collections::BTreeSet;

    use clyde_core::HumanDuration;
    use clyde_core::classification::{CredentialPolicy, EgressProfile, TrustClass};
    use clyde_core::ids::{self, ActorId};
    use clyde_core::lease::{AuthorityFlags, LeaseState};
    use clyde_core::mission::{
        ApprovalPolicy, MissionScope, MissionState, NetworkPolicy, StopCondition,
    };
    use clyde_core::repo_path::RepoPath;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn budget() -> Budget {
        Budget {
            max_duration: HumanDuration::parse("2h").unwrap(),
            max_task_runs: 10,
            max_parallel_subagents: 1,
            max_subagents: 2,
            max_cpu_seconds: 3600,
            max_cache_bytes: 1 << 30,
            max_artifact_bytes: 1 << 28,
            max_egress_bytes: 1 << 20,
            max_egress_requests: 50,
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
            scope: MissionScope {
                edit_paths: [path("crates/core"), path("crates/policy")]
                    .into_iter()
                    .collect(),
                read_paths: [path("docs")].into_iter().collect(),
            },
            allowed_tasks: [TaskType::RustCheck, TaskType::WorkspaceEdit]
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
            budget: budget(),
            expiry: created + chrono::Duration::hours(2),
            state: MissionState::Active,
            stop_conditions: [StopCondition::Expiry].into_iter().collect(),
            success_criteria: Vec::new(),
            cache_dir: Some(std::path::PathBuf::from("/var/lib/clyde/missions/m1")),
            created_at: created,
            closed_at: None,
        }
    }

    fn lease(mission: &Mission) -> Lease {
        let issued = Utc::now();
        Lease {
            id: ids::new::lease_id().unwrap(),
            mission: mission.id.clone(),
            parent: None,
            actor: ActorId::parse("agent:claude/1").unwrap(),
            issued_by: ActorId::parse("human:clyde").unwrap(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::hours(1),
            repo_scope: MissionScope {
                // Narrower than the mission: a sub-agent must be told what it can
                // do, not what its parent could.
                edit_paths: [path("crates/core")].into_iter().collect(),
                read_paths: BTreeSet::new(),
            },
            task_scope: [TaskType::RustCheck].into_iter().collect(),
            network_scope: EgressProfile::None,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags::NONE,
            budget: budget(),
            usage: BudgetUsage {
                task_runs: 3,
                ..BudgetUsage::default()
            },
            state: LeaseState::Active,
            purpose: "sub-agent".to_owned(),
        }
    }

    #[test]
    fn the_mission_view_reports_the_leases_scope_not_the_missions() {
        let mission = mission();
        let lease = lease(&mission);
        let view = MissionView::new(&mission, &lease);
        assert_eq!(view.edit_paths, vec!["crates/core".to_owned()]);
        assert!(
            !view.edit_paths.contains(&"crates/policy".to_owned()),
            "a sub-agent must not be told it can edit what only its parent could"
        );
        assert_eq!(view.egress_profile, "none");
        assert_eq!(view.allowed_tasks, vec!["rust.check".to_owned()]);
    }

    #[test]
    fn no_view_carries_a_host_path() {
        let mission = mission();
        let lease = lease(&mission);
        let rendered = serde_json::to_string(&MissionView::new(&mission, &lease)).unwrap();
        assert!(
            !rendered.contains("/var/lib/clyde"),
            "host paths must never reach an actor: {rendered}"
        );
    }

    #[test]
    fn the_budget_view_reports_headroom_not_ceilings() {
        let mission = mission();
        let lease = lease(&mission);
        let view = BudgetView::new(&lease.budget, &lease.usage);
        assert_eq!(view.task_runs_remaining, 7);
    }

    #[test]
    fn the_artifact_view_omits_the_content_reference() {
        let digest = clyde_core::Digest::of_bytes(b"log");
        let artifact = Artifact {
            id: Artifact::id_for(&digest).unwrap(),
            kind: clyde_core::artifact::ArtifactKind::Log,
            produced_by: None,
            mission: ids::new::mission_id().unwrap(),
            trust_class: TrustClass::T2,
            size_bytes: 3,
            blake3: digest,
            content_ref: std::path::PathBuf::from("/var/lib/clyde/blobs/abc"),
            created_at: Utc::now(),
            retain_until: None,
        };
        let rendered = serde_json::to_string(&ArtifactView::new(&artifact)).unwrap();
        assert!(!rendered.contains("/var/lib/clyde"));
        assert!(!rendered.contains("content_ref"));
        assert!(
            rendered.contains("T2"),
            "provenance must be visible: {rendered}"
        );
    }

    #[test]
    fn a_denial_view_carries_reasons_and_next_steps() {
        let reasons = vec![PolicyReason::TaskNotInLease {
            task: TaskType::GitPush,
        }];
        let view = DenialView::new("git.push", &reasons);
        assert_eq!(view.reasons.len(), 1);
        assert!(
            !view.alternatives.is_empty(),
            "a denial must offer a next step"
        );
    }

    #[test]
    fn a_subagent_view_never_contains_a_token() {
        let view = SubagentView {
            actor: "agent:claude/1".to_owned(),
            lease: "l-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            edit_paths: vec!["crates/core".to_owned()],
            allowed_tasks: vec!["rust.check".to_owned()],
            expires_at: Utc::now(),
        };
        let rendered = serde_json::to_value(&view).unwrap();
        let object = rendered.as_object().unwrap();
        assert!(!object.contains_key("token"));
        assert!(!object.contains_key("token_hash"));
    }
}
