//! The operator API, on the admin socket.
//!
//! Operations available **only** here: mission create, approve, deny, revoke,
//! renew, workspace register, access-baseline confirmation, dependency bundle
//! import, and any read of another actor's data (Phase 1 deliverable 1).
//!
//! Approvals are only accepted on this socket. There is no actor-facing
//! operation that can approve anything, which is what makes agent
//! self-approval structurally impossible rather than policy-prohibited (D2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Method names. Constants rather than string literals at call sites, so a typo
/// is a compile error on one side and a `method not found` on the other.
pub mod methods {
    pub const WORKSPACE_REGISTER: &str = "workspace.register";
    pub const WORKSPACE_LIST: &str = "workspace.list";

    pub const MISSION_CREATE: &str = "mission.create";
    pub const MISSION_STATUS: &str = "mission.status";
    pub const MISSION_LIST: &str = "mission.list";
    pub const MISSION_APPROVE: &str = "mission.approve";
    pub const MISSION_DENY: &str = "mission.deny";
    pub const MISSION_REVOKE: &str = "mission.revoke";
    pub const MISSION_RENEW: &str = "mission.renew";
    pub const MISSION_CLOSE: &str = "mission.close";
    pub const MISSION_REVIEW: &str = "mission.review";

    pub const APPROVALS_LIST: &str = "approvals.list";
    pub const APPROVALS_DECIDE: &str = "approvals.decide";

    pub const AUDIT_SHOW: &str = "audit.show";
    pub const AUDIT_VERIFY: &str = "audit.verify";

    pub const ACCESS_SHOW: &str = "access.show";
    pub const ACCESS_PROPOSE: &str = "access.propose";
    pub const ACCESS_LEARN: &str = "access.learn";
    pub const ACCESS_REVIEW: &str = "access.review";
    pub const ACCESS_CONFIRM: &str = "access.confirm";
    pub const ACCESS_RESET: &str = "access.reset";

    pub const DEPS_IMPORT: &str = "deps.import";
    pub const DEPS_LIST: &str = "deps.list";
    pub const DEPS_CONFIRM_INVENTORY: &str = "deps.confirm-inventory";

    /// The operator task surface (D25). `run` is here as well as on the actor
    /// surface: a human driving the pipeline is admitted against the same lease,
    /// policy, budget, and baseline, and is recorded as the acting principal.
    pub const TASK_RUN: &str = "task.run";
    pub const TASK_LIST: &str = "task.list";
    pub const TASK_STATUS: &str = "task.status";
    pub const TASK_LOGS: &str = "task.logs";

    pub const DOCTOR: &str = "doctor";

    /// Every admin method, for the CLI's own routing and for tests that assert
    /// the surfaces do not overlap.
    pub const ALL: [&str; 29] = [
        WORKSPACE_REGISTER,
        WORKSPACE_LIST,
        MISSION_CREATE,
        MISSION_STATUS,
        MISSION_LIST,
        MISSION_APPROVE,
        MISSION_DENY,
        MISSION_REVOKE,
        MISSION_RENEW,
        MISSION_CLOSE,
        MISSION_REVIEW,
        APPROVALS_LIST,
        APPROVALS_DECIDE,
        AUDIT_SHOW,
        AUDIT_VERIFY,
        ACCESS_SHOW,
        ACCESS_PROPOSE,
        ACCESS_LEARN,
        ACCESS_REVIEW,
        ACCESS_CONFIRM,
        ACCESS_RESET,
        DEPS_IMPORT,
        DEPS_LIST,
        DEPS_CONFIRM_INVENTORY,
        TASK_RUN,
        TASK_LIST,
        TASK_STATUS,
        TASK_LOGS,
        DOCTOR,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterWorkspace {
    /// Absolute host path.
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub workspace: String,
    pub root: String,
    pub vcs: String,
    pub active_mission: Option<String>,
}

/// Mission creation.
///
/// The proposal takes the human's objective plus configuration defaults and
/// produces a concrete envelope. The envelope is shown for approval exactly as
/// it will be issued — no field is decided after approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMission {
    pub workspace: String,
    pub objective: String,
    #[serde(default)]
    pub edit_paths: Vec<String>,
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<String>,
    /// Duration string, such as "2h". Clamped to the configured maximum.
    #[serde(default)]
    pub expires_in: Option<String>,
    #[serde(default)]
    pub max_task_runs: Option<u32>,
    /// Whether the agent may reach the model API.
    #[serde(default = "default_true")]
    pub model_api: bool,
    /// Whether to start the agent process once the mission is approved.
    #[serde(default = "default_true")]
    pub start_agent: bool,
}

fn default_true() -> bool {
    true
}

/// The envelope a human approves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionEnvelope {
    pub mission: String,
    pub workspace: String,
    pub objective: String,
    pub state: String,
    pub edit_paths: Vec<String>,
    pub read_paths: Vec<String>,
    pub allowed_tasks: Vec<String>,
    pub egress_profile: String,
    pub credential_policy: String,
    pub expires_at: DateTime<Utc>,
    pub max_task_runs: u32,
    pub max_subagents: u8,
    /// Tasks that will require a human decision each time they run.
    pub approval_required_tasks: Vec<String>,
    /// Access baselines in force for this workspace, summarised — because a
    /// baseline lives only in Clyde state and is otherwise invisible.
    pub baselines_in_force: Vec<String>,
    /// Caveats the approval UX must state rather than imply.
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionRef {
    pub mission: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewMission {
    pub mission: String,
    /// Additional duration, such as "1h".
    pub extend_by: String,
    #[serde(default)]
    pub additional_task_runs: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideApproval {
    pub approval: String,
    /// `approve_once`, `approve_for_mission`, or `deny`.
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// A pending approval, as the operator sees it.
///
/// Everything the human needs to decide is here, including the caveats: an
/// approval prompt that omits what it cannot promise invites reflexive approval,
/// which is the failure mode the whole design exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalView {
    pub approval: String,
    pub mission: String,
    pub actor: String,
    pub subject: String,
    pub summary: String,
    pub reason: String,
    /// Why the previous attempt failed, where this arose from a denial.
    pub prior_failure: Option<String>,
    pub alternatives: Vec<String>,
    /// The exact host allowlist this would permit, if any.
    pub egress_hosts: Vec<String>,
    pub egress_profile: String,
    pub credentials: String,
    pub outputs: Vec<String>,
    /// Lockfile change classification, for a dependency fetch.
    pub lockfile_change: Option<String>,
    /// Code-execution inventory diff, which is the higher-signal companion.
    pub inventory_diff: Vec<String>,
    /// Task runs that passed against this tree, as push evidence.
    pub task_evidence: Vec<String>,
    pub caveats: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub request_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuditQuery {
    pub mission: Option<String>,
    pub workspace: Option<String>,
    pub limit: Option<usize>,
    /// Only high-signal events: approvals, drift, denied egress, brokered work.
    pub high_signal_only: bool,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            mission: None,
            workspace: None,
            limit: Some(100),
            high_signal_only: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub mission: Option<String>,
    pub actor: Option<String>,
    pub detail: String,
    pub high_signal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTarget {
    pub workspace: String,
    pub task: String,
    pub target: String,
}

/// A baseline or proposal, rendered for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessBaselineView {
    pub workspace: String,
    pub task: String,
    pub target: String,
    pub origin: String,
    pub confirmed: bool,
    pub confirmed_by: Option<String>,
    /// Subtree grants: first-party code, not drift-sensitive.
    pub grants: Vec<String>,
    /// Pins: everything outside the grants, each with a reason.
    pub pins: Vec<AccessPinView>,
    pub inventory_entries: Vec<String>,
    pub rationale: Vec<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPinView {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBundle {
    /// Host path to a cargo cache or vendor directory.
    pub source: String,
    /// Host path to the `Cargo.lock` it satisfies.
    pub lockfile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleView {
    pub artifact: String,
    pub lockfile_digest: String,
    pub crate_count: u32,
    pub code_executing_crates: usize,
    pub registries: Vec<String>,
    pub inventory_confirmed: bool,
    pub created_at: DateTime<Utc>,
}

/// The end-to-end review surface (Phase 4 deliverable 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionReview {
    pub mission: String,
    pub objective: String,
    pub state: String,
    pub files_changed: Vec<String>,
    pub diff_stat: String,
    pub tasks: Vec<TaskSummary>,
    pub escalations: Vec<String>,
    pub approvals: Vec<String>,
    pub egress: Vec<String>,
    pub brokered_operations: Vec<String>,
    pub budget_consumed: String,
    /// The postures the mission's work actually happened under, distinct, in the
    /// order first seen (D26).
    ///
    /// A list rather than a single value because posture is a property of the
    /// moment: a deployment that gains the warden mid-mission must not make the
    /// earlier work look as though it were enforced.
    pub postures: Vec<String>,
    pub closing_diff: Option<String>,
    /// Whether the audit chain for this mission verifies.
    pub audit_intact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_run: String,
    pub task: String,
    pub state: String,
    pub classification: Option<String>,
    pub policy_digest: String,
    /// Who asked (D25) and what was in force when they did (D26), so review
    /// answers both without inference.
    pub principal: String,
    pub posture: String,
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

    #[test]
    fn admin_method_names_are_unique_and_complete() {
        let mut names = methods::ALL.to_vec();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate admin method name");
    }

    #[test]
    fn no_admin_method_is_also_an_actor_tool() {
        // The separation is the point: an actor-facing operation that could
        // approve something would make self-approval possible.
        for method in methods::ALL {
            assert!(
                !crate::mcp::is_known_tool(method),
                "{method} appears on both surfaces"
            );
        }
        assert!(
            methods::ALL.iter().any(|method| method.contains("approve")),
            "approvals must exist on the admin surface"
        );
    }

    #[test]
    fn request_types_reject_unknown_fields() {
        let error = serde_json::from_value::<CreateMission>(serde_json::json!({
            "workspace": "w-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "objective": "x",
            "unexpected": true
        }));
        assert!(
            error.is_err(),
            "a typo in an admin request must be an error"
        );
    }

    #[test]
    fn mission_creation_defaults_are_conservative_but_usable() {
        let created: CreateMission = serde_json::from_value(serde_json::json!({
            "workspace": "w-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "objective": "tidy"
        }))
        .unwrap();
        assert!(created.edit_paths.is_empty());
        assert!(created.tasks.is_empty());
        assert!(
            created.model_api,
            "an agent with no model access cannot work"
        );
        assert!(created.start_agent);
    }

    #[test]
    fn the_audit_query_defaults_to_a_bounded_window() {
        let query = AuditQuery::default();
        assert_eq!(query.limit, Some(100));
        assert!(!query.high_signal_only);
    }

    #[test]
    fn an_approval_view_has_somewhere_to_state_its_caveats() {
        let view = ApprovalView {
            approval: "ap-1".to_owned(),
            mission: "m-1".to_owned(),
            actor: "agent:claude".to_owned(),
            subject: "task_escalation".to_owned(),
            summary: "fetch dependencies".to_owned(),
            reason: "rust.check failed with MissingDependencies".to_owned(),
            prior_failure: Some("serde is not in the dependency bundle".to_owned()),
            alternatives: vec![],
            egress_hosts: vec!["static.crates.io".to_owned()],
            egress_profile: "rust-registry".to_owned(),
            credentials: "none".to_owned(),
            outputs: vec!["dependency bundle".to_owned()],
            lockfile_change: Some("4 additions, 1 version change".to_owned()),
            inventory_diff: vec!["+ serde_derive_internals 0.29.1".to_owned()],
            task_evidence: vec![],
            caveats: vec!["allowlisting is by destination host, not by content".to_owned()],
            expires_at: Utc::now(),
            request_digest: "ab".repeat(32),
        };
        assert!(!view.caveats.is_empty());
        assert!(
            view.prior_failure.is_some(),
            "a prompt that does not say what went wrong invites reflexive approval"
        );
    }
}
