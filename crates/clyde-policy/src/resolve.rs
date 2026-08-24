//! Task policy resolution and action admission.
//!
//! Two entry points:
//!
//! - [`resolve_task_policy`] answers "what policy governs this task here", which
//!   is a question about the catalog and configuration only.
//! - [`validate_action`] answers "may this actor do this now", which additionally
//!   consults the mission, the lease, the budget, the host, and any approval.
//!
//! Both are pure. `validate_action` never returns `Err`: it always produces a
//! decision, because a decision is recorded whether it allows or denies (Phase 1
//! deliverable 3).

use chrono::{DateTime, Utc};
use clyde_core::budget::BudgetCost;
use clyde_core::classification::{
    ApprovalRequirement, CachePolicy, Environment, IsolationLevel, ResourceLimits,
};
use clyde_core::decision::{PolicyOutcome, PolicyReason};
use clyde_core::lease::{Lease, LeaseState};
use clyde_core::mission::Mission;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::{TaskPolicy, TaskType};

use crate::budget::charge_budget;
use crate::config::Config;
use crate::egress::{EgressComparison, is_no_wider_than};

/// A policy resolved against configuration, with the narrowings that were
/// applied recorded for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPolicy {
    pub policy: TaskPolicy,
    /// Human-readable notes about what configuration narrowed, so the operator
    /// can see why a policy differs from the built-in.
    pub notes: Vec<String>,
}

/// Resolves the policy for `task` at `path`.
///
/// Configuration may only narrow the built-in policy; the merge that enforces
/// that already happened when the configuration was loaded, so this function
/// applies an already-validated override.
pub fn resolve_task_policy(
    task: TaskType,
    _path: &RepoPath,
    _mission: &Mission,
    _lease: &Lease,
    config: &Config,
) -> Result<ResolvedPolicy, PolicyReason> {
    let mut policy = crate::catalog::builtin_policy(task);
    let mut notes = Vec::new();

    // Environment-wide limits narrow every task in that environment.
    let environment_limits = match policy.environment {
        Environment::Workspace | Environment::ControlPlane => config.limits.workspace,
        Environment::Build => config.limits.build,
        Environment::Broker => config.limits.build,
    };
    let narrowed = policy.limits.narrowed_to(environment_limits);
    if narrowed != policy.limits {
        notes.push("resource limits narrowed by configuration".to_owned());
        policy.limits = narrowed;
    }

    if let Some(task_override) = config.task_override(task) {
        if !task_override.enabled {
            return Err(PolicyReason::PhaseGated {
                capability: task.name().to_owned(),
            });
        }
        if let Some(egress) = &task_override.egress
            && *egress != policy.egress
        {
            notes.push(format!(
                "egress profile narrowed by configuration to {}",
                egress.name()
            ));
            policy.egress = egress.clone();
        }
        if let Some(approval) = task_override.approval
            && approval > policy.approval
        {
            notes.push(format!("approval requirement raised to {approval}"));
            policy.approval = approval;
        }
        if let Some(limits) = task_override.limits {
            let narrowed = narrow_limits(policy.limits, limits);
            if narrowed != policy.limits {
                notes.push("resource limits narrowed by task configuration".to_owned());
                policy.limits = narrowed;
            }
        }
    }

    Ok(ResolvedPolicy { policy, notes })
}

fn narrow_limits(current: ResourceLimits, requested: ResourceLimits) -> ResourceLimits {
    current.narrowed_to(requested)
}

/// What the host can actually provide, as a value so admission stays pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCapabilities {
    /// The strongest isolation level available, or `None` if no sandbox backend
    /// is usable at all.
    pub strongest_isolation: Option<IsolationLevel>,
    /// Whether cgroup v2 delegation is available. Mandatory for T2 and above
    /// (D22); its absence refuses build tasks rather than degrading them.
    pub cgroup_delegation: bool,
}

impl HostCapabilities {
    /// A host that can run everything, for tests and for the control plane.
    pub fn full() -> Self {
        Self {
            strongest_isolation: Some(IsolationLevel::MicroVm),
            cgroup_delegation: true,
        }
    }
}

/// Whether a matching approval already exists for this exact request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    /// No approval has been sought.
    None,
    /// A matching, unexpired, unconsumed approval exists.
    Present,
    /// An approval exists but does not match, has expired, or was consumed.
    StaleOrMismatched,
    /// A human denied this request.
    Denied,
}

/// Everything admission needs, as one value.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionInput<'a> {
    pub task: TaskType,
    pub path: &'a RepoPath,
    pub mission: &'a Mission,
    pub lease: &'a Lease,
    pub config: &'a Config,
    pub now: DateTime<Utc>,
    pub host: HostCapabilities,
    /// Whether a confirmed access baseline exists for this task and target.
    pub baseline_confirmed: bool,
    pub approval: ApprovalState,
}

/// The admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub outcome: PolicyOutcome,
    pub policy: Option<TaskPolicy>,
    pub reasons: Vec<PolicyReason>,
    pub alternatives: Vec<String>,
    /// The budget cost that must be charged before execution, present only when
    /// the outcome permits work.
    pub cost: Option<BudgetCost>,
    pub notes: Vec<String>,
}

impl Admission {
    fn denied(reasons: Vec<PolicyReason>) -> Self {
        let alternatives = reasons
            .iter()
            .filter_map(PolicyReason::suggested_alternative)
            .collect();
        Self {
            outcome: PolicyOutcome::Denied,
            policy: None,
            reasons,
            alternatives,
            cost: None,
            notes: Vec::new(),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self.outcome, PolicyOutcome::Allowed)
    }

    pub fn needs_approval(&self) -> bool {
        matches!(self.outcome, PolicyOutcome::AllowedWithApproval)
    }

    /// The first denial reason, for a one-line message.
    pub fn primary_reason(&self) -> Option<&PolicyReason> {
        self.reasons.first()
    }
}

/// Admits or denies an action.
///
/// Checks run in a deliberate order: identity and lifecycle first, then scope,
/// then policy ceilings, then host capability, then budget, then approval. That
/// ordering means the reported reason is the most fundamental one, rather than a
/// downstream symptom of it.
pub fn validate_action(input: AdmissionInput<'_>) -> Admission {
    let mut reasons = Vec::new();

    // Lifecycle.
    if !input.mission.state.permits_work() {
        reasons.push(PolicyReason::MissionNotActive);
    }
    if input.mission.is_expired_at(input.now) {
        reasons.push(PolicyReason::MissionNotActive);
    }
    match input.lease.state {
        LeaseState::Active => {}
        LeaseState::Expired => reasons.push(PolicyReason::LeaseExpired),
        _ => reasons.push(PolicyReason::LeaseNotActive),
    }
    if input.lease.is_expired_at(input.now) {
        reasons.push(PolicyReason::LeaseExpired);
    }
    if !reasons.is_empty() {
        return Admission::denied(reasons);
    }

    // Task membership: the mission envelope first, then the lease, because a
    // task outside the envelope cannot be fixed by a different lease.
    if !input.mission.allowed_tasks.contains(&input.task) {
        reasons.push(PolicyReason::TaskNotInMission { task: input.task });
    }
    if !input.lease.allows_task(input.task) {
        reasons.push(PolicyReason::TaskNotInLease { task: input.task });
    }
    if !reasons.is_empty() {
        return Admission::denied(reasons);
    }

    // Authority flags.
    if !input.lease.authority.may_request_tasks {
        reasons.push(PolicyReason::AuthorityFlagNotHeld {
            flag: "may_request_tasks".to_owned(),
        });
    }
    if input.task == TaskType::GitPush && !input.lease.authority.may_request_publish {
        reasons.push(PolicyReason::AuthorityFlagNotHeld {
            flag: "may_request_publish".to_owned(),
        });
    }
    if input.task == TaskType::WorkspaceEdit && !input.lease.authority.may_edit {
        reasons.push(PolicyReason::AuthorityFlagNotHeld {
            flag: "may_edit".to_owned(),
        });
    }
    if !reasons.is_empty() {
        return Admission::denied(reasons);
    }

    // Path scope. Which check applies depends on whether the task writes.
    if let Some(reason) = check_path_scope(input.task, input.path, input.lease) {
        return Admission::denied(vec![reason]);
    }

    // Resolve the policy.
    let resolved = match resolve_task_policy(
        input.task,
        input.path,
        input.mission,
        input.lease,
        input.config,
    ) {
        Ok(resolved) => resolved,
        Err(reason) => return Admission::denied(vec![reason]),
    };
    let policy = resolved.policy;

    // Egress ceiling. The lease's network scope is the ceiling; a policy that
    // wants more needs an escalation, not a derivation.
    match is_no_wider_than(&policy.egress, &input.lease.network_scope) {
        EgressComparison::NoWider => {}
        EgressComparison::Wider => reasons.push(PolicyReason::EgressWiderThanLease {
            requested: policy.egress.to_string(),
            ceiling: input.lease.network_scope.to_string(),
        }),
        EgressComparison::Incomparable => {
            reasons.push(PolicyReason::EgressProfilesIncomparable {
                requested: policy.egress.to_string(),
                ceiling: input.lease.network_scope.to_string(),
            });
        }
    }

    // Credential ceiling.
    if !policy
        .credentials
        .is_no_wider_than(input.lease.credential_scope)
    {
        reasons.push(PolicyReason::CredentialsWiderThanLease);
    }
    if !reasons.is_empty() {
        return Admission::denied(reasons);
    }

    // Host capability. A task is refused rather than run at a weaker boundary.
    match input.host.strongest_isolation {
        Some(available) if available >= policy.min_isolation => {}
        _ => {
            return Admission::denied(vec![PolicyReason::IsolationUnavailable {
                required: policy.min_isolation,
            }]);
        }
    }
    if policy.trust_class.requires_cgroup_limits() && !input.host.cgroup_delegation {
        return Admission::denied(vec![PolicyReason::CgroupLimitsUnavailable]);
    }

    // Access baseline. A task with no confirmed baseline for its target is
    // refused; there is no implicit wide-scope first run (D18).
    if policy.requires_access_baseline && !input.baseline_confirmed {
        return Admission::denied(vec![PolicyReason::AccessBaselineMissing {
            target: input.path.clone(),
        }]);
    }

    // Budget, charged at admission.
    let cost = admission_cost(input.task, &policy);
    if let Err(reason) = charge_budget(&input.lease.budget, &input.lease.usage, &cost) {
        return Admission::denied(vec![reason]);
    }

    // Approval. Mission pre-approval satisfies a `HumanRequired` policy only
    // where the human recorded it in the envelope that was approved.
    let pre_approved = input
        .mission
        .approval_policy
        .pre_approved_tasks
        .contains(&input.task);
    let outcome = match (policy.approval, input.approval, pre_approved) {
        (_, ApprovalState::Denied, _) => {
            return Admission::denied(vec![PolicyReason::ApprovalMissingOrStale]);
        }
        (ApprovalRequirement::None, _, _) => PolicyOutcome::Allowed,
        (_, ApprovalState::Present, _) => PolicyOutcome::Allowed,
        (ApprovalRequirement::PolicyGated, _, true) => PolicyOutcome::Allowed,
        (ApprovalRequirement::PolicyGated, _, false)
        | (ApprovalRequirement::HumanRequired, _, _) => PolicyOutcome::AllowedWithApproval,
    };

    let mut alternatives = Vec::new();
    if matches!(outcome, PolicyOutcome::AllowedWithApproval) {
        alternatives.push(format!(
            "a human must approve {} on the admin channel before it runs",
            input.task
        ));
    }

    Admission {
        outcome,
        policy: Some(policy),
        reasons: Vec::new(),
        alternatives,
        cost: Some(cost),
        notes: resolved.notes,
    }
}

/// Whether the requested path is inside the lease's scope, at the authority the
/// task needs.
fn check_path_scope(task: TaskType, path: &RepoPath, lease: &Lease) -> Option<PolicyReason> {
    match task {
        // Writes: must be inside the lease's edit paths.
        TaskType::WorkspaceEdit => (!lease.repo_scope.may_write(path))
            .then(|| PolicyReason::NotWritableUnderLease { path: path.clone() }),
        // Reads: edit paths are readable by construction.
        TaskType::WorkspaceRead
        | TaskType::RepoSearch
        | TaskType::RustCheck
        | TaskType::RustTestUnit
        | TaskType::GitCommitPrepare => (!lease.repo_scope.may_read(path))
            .then(|| PolicyReason::OutOfLeaseScope { path: path.clone() }),
        // Neither addresses a repository subtree: dependency resolution sees
        // manifests only, and a push addresses a commit.
        TaskType::RustResolveDeps | TaskType::GitPush => None,
    }
}

/// What a task costs at admission.
///
/// Cache bytes are charged when the cache is created rather than here, and CPU
/// seconds are charged from the observed run, so admission charges the one
/// dimension that must not be lost to a crash: the task run itself.
fn admission_cost(task: TaskType, policy: &TaskPolicy) -> BudgetCost {
    let mut cost = BudgetCost::one_task_run();
    if matches!(policy.cache, CachePolicy::WritesDependencyBundle) {
        // A fetch is the only task that consumes egress budget at admission,
        // since its whole purpose is network use.
        cost.egress_requests = 1;
    }
    let _ = task;
    cost
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
    use clyde_core::budget::{Budget, BudgetUsage};
    use clyde_core::classification::{CredentialPolicy, EgressProfile};
    use clyde_core::ids::{self, ActorId};
    use clyde_core::lease::AuthorityFlags;
    use clyde_core::mission::{
        ApprovalPolicy, MissionScope, MissionState, NetworkPolicy, StopCondition,
    };

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
            max_egress_requests: 10,
        }
    }

    fn mission(tasks: &[TaskType]) -> Mission {
        let created = Utc::now();
        Mission {
            id: ids::new::mission_id().unwrap(),
            workspace: ids::new::workspace_id().unwrap(),
            objective: "work".to_owned(),
            initiator: ActorId::parse("human:andrew").unwrap(),
            primary_actor: ActorId::parse("agent:claude").unwrap(),
            scope: MissionScope {
                edit_paths: [path("crates/core")].into_iter().collect(),
                read_paths: [path("docs")].into_iter().collect(),
            },
            allowed_tasks: tasks.iter().copied().collect(),
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
            stop_conditions: [StopCondition::BudgetExhausted].into_iter().collect(),
            success_criteria: Vec::new(),
            cache_dir: None,
            created_at: created,
            closed_at: None,
        }
    }

    fn lease(mission: &Mission, tasks: &[TaskType]) -> Lease {
        let issued = Utc::now();
        Lease {
            id: ids::new::lease_id().unwrap(),
            mission: mission.id.clone(),
            parent: None,
            actor: ActorId::parse("agent:claude").unwrap(),
            issued_by: ActorId::parse("human:clyde").unwrap(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::hours(1),
            repo_scope: mission.scope.clone(),
            task_scope: tasks.iter().copied().collect(),
            network_scope: EgressProfile::ModelApi,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags {
                may_edit: true,
                may_request_tasks: true,
                may_spawn_subagents: true,
                may_request_publish: true,
            },
            budget: budget(),
            usage: BudgetUsage::default(),
            state: LeaseState::Active,
            purpose: "primary".to_owned(),
        }
    }

    struct Fixture {
        mission: Mission,
        lease: Lease,
        config: Config,
    }

    fn fixture(tasks: &[TaskType]) -> Fixture {
        let mission = mission(tasks);
        let lease = lease(&mission, tasks);
        Fixture {
            mission,
            lease,
            config: Config::defaults(),
        }
    }

    fn input<'a>(fixture: &'a Fixture, task: TaskType, path: &'a RepoPath) -> AdmissionInput<'a> {
        AdmissionInput {
            task,
            path,
            mission: &fixture.mission,
            lease: &fixture.lease,
            config: &fixture.config,
            now: Utc::now(),
            host: HostCapabilities::full(),
            baseline_confirmed: true,
            approval: ApprovalState::None,
        }
    }

    #[test]
    fn an_in_scope_check_with_a_baseline_is_allowed() {
        let fixture = fixture(&[TaskType::RustCheck]);
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustCheck, &target));
        assert!(admission.is_allowed(), "{:?}", admission.reasons);
        assert_eq!(admission.cost.map(|cost| cost.task_runs), Some(1));
        assert!(admission.policy.is_some());
    }

    #[test]
    fn a_task_outside_the_mission_envelope_is_denied_before_the_lease_is_consulted() {
        let fixture = fixture(&[TaskType::RustCheck]);
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::GitPush, &target));
        assert_eq!(admission.outcome, PolicyOutcome::Denied);
        assert!(admission.reasons.contains(&PolicyReason::TaskNotInMission {
            task: TaskType::GitPush
        }));
        assert!(
            !admission.alternatives.is_empty(),
            "a denial must offer a next step"
        );
    }

    #[test]
    fn an_expired_lease_denies_before_any_scope_check() {
        let mut fixture = fixture(&[TaskType::RustCheck]);
        fixture.lease.expires_at = Utc::now() - chrono::Duration::minutes(1);
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustCheck, &target));
        assert_eq!(
            admission.primary_reason(),
            Some(&PolicyReason::LeaseExpired)
        );
    }

    #[test]
    fn an_inactive_mission_denies() {
        let mut fixture = fixture(&[TaskType::RustCheck]);
        fixture.mission.state = MissionState::Revoked;
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustCheck, &target));
        assert_eq!(
            admission.primary_reason(),
            Some(&PolicyReason::MissionNotActive)
        );
    }

    #[test]
    fn writing_outside_the_edit_scope_is_denied() {
        let fixture = fixture(&[TaskType::WorkspaceEdit]);
        let outside = path("docs/readme.md");
        let admission = validate_action(input(&fixture, TaskType::WorkspaceEdit, &outside));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::NotWritableUnderLease { .. })
        ));
        // Reading the same path is fine, because it is in the read scope.
        let fixture = self::fixture(&[TaskType::WorkspaceRead]);
        let admission = validate_action(input(&fixture, TaskType::WorkspaceRead, &outside));
        assert!(admission.is_allowed());
    }

    #[test]
    fn reading_outside_every_scope_is_denied() {
        let fixture = fixture(&[TaskType::RepoSearch]);
        let outside = path("infra/secrets");
        let admission = validate_action(input(&fixture, TaskType::RepoSearch, &outside));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::OutOfLeaseScope { .. })
        ));
    }

    #[test]
    fn a_missing_baseline_refuses_a_build_rather_than_running_it_wide() {
        let fixture = fixture(&[TaskType::RustCheck]);
        let target = path("crates/core");
        let mut input = input(&fixture, TaskType::RustCheck, &target);
        input.baseline_confirmed = false;
        let admission = validate_action(input);
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::AccessBaselineMissing { .. })
        ));
        assert!(
            admission
                .alternatives
                .iter()
                .any(|text| text.contains("clyde access propose")),
            "the denial must say how to get a baseline"
        );
    }

    #[test]
    fn a_build_task_is_refused_without_cgroup_delegation() {
        let fixture = fixture(&[TaskType::RustCheck]);
        let target = path("crates/core");
        let mut input = input(&fixture, TaskType::RustCheck, &target);
        input.host = HostCapabilities {
            strongest_isolation: Some(IsolationLevel::NamespaceSandbox),
            cgroup_delegation: false,
        };
        let admission = validate_action(input);
        assert_eq!(
            admission.primary_reason(),
            Some(&PolicyReason::CgroupLimitsUnavailable),
            "there is no rlimits-only fallback for T2 (D22)"
        );
    }

    #[test]
    fn a_workspace_task_still_runs_without_cgroup_delegation() {
        let fixture = fixture(&[TaskType::WorkspaceEdit]);
        let target = path("crates/core/src");
        let mut input = input(&fixture, TaskType::WorkspaceEdit, &target);
        input.host = HostCapabilities {
            strongest_isolation: Some(IsolationLevel::NamespaceSandbox),
            cgroup_delegation: false,
        };
        assert!(validate_action(input).is_allowed());
    }

    #[test]
    fn a_microvm_task_is_refused_on_a_namespace_only_host() {
        let mut fixture = fixture(&[TaskType::RustResolveDeps]);
        fixture.lease.network_scope = EgressProfile::RustRegistry;
        fixture.mission.network_policy.ceiling = EgressProfile::RustRegistry;
        let target = RepoPath::root();
        let mut input = input(&fixture, TaskType::RustResolveDeps, &target);
        input.host = HostCapabilities {
            strongest_isolation: Some(IsolationLevel::NamespaceSandbox),
            cgroup_delegation: true,
        };
        let admission = validate_action(input);
        assert_eq!(
            admission.primary_reason(),
            Some(&PolicyReason::IsolationUnavailable {
                required: IsolationLevel::MicroVm
            }),
            "it must be refused, never silently downgraded"
        );
    }

    #[test]
    fn a_fetch_needs_an_egress_profile_the_lease_permits() {
        // The lease holds model-api; rust-registry is incomparable with it, so
        // the fetch needs an escalation rather than a derivation.
        let fixture = fixture(&[TaskType::RustResolveDeps]);
        let target = RepoPath::root();
        let admission = validate_action(input(&fixture, TaskType::RustResolveDeps, &target));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::EgressProfilesIncomparable { .. })
        ));
    }

    #[test]
    fn a_fetch_with_the_right_lease_needs_human_approval() {
        let mut fixture = fixture(&[TaskType::RustResolveDeps]);
        fixture.lease.network_scope = EgressProfile::RustRegistry;
        let target = RepoPath::root();
        let admission = validate_action(input(&fixture, TaskType::RustResolveDeps, &target));
        assert!(admission.needs_approval(), "{:?}", admission.reasons);
        assert!(!admission.alternatives.is_empty());
    }

    #[test]
    fn a_present_approval_turns_approval_required_into_allowed() {
        let mut fixture = fixture(&[TaskType::RustResolveDeps]);
        fixture.lease.network_scope = EgressProfile::RustRegistry;
        let target = RepoPath::root();
        let mut input = input(&fixture, TaskType::RustResolveDeps, &target);
        input.approval = ApprovalState::Present;
        assert!(validate_action(input).is_allowed());
    }

    #[test]
    fn a_denied_approval_is_a_denial_not_a_re_prompt() {
        let mut fixture = fixture(&[TaskType::RustResolveDeps]);
        fixture.lease.network_scope = EgressProfile::RustRegistry;
        let target = RepoPath::root();
        let mut input = input(&fixture, TaskType::RustResolveDeps, &target);
        input.approval = ApprovalState::Denied;
        let admission = validate_action(input);
        assert_eq!(admission.outcome, PolicyOutcome::Denied);
    }

    #[test]
    fn push_requires_the_publish_flag_and_the_credential_scope() {
        let mut fixture = fixture(&[TaskType::GitPush]);
        fixture.lease.authority.may_request_publish = false;
        let target = RepoPath::root();
        let admission = validate_action(input(&fixture, TaskType::GitPush, &target));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::AuthorityFlagNotHeld { .. })
        ));

        let mut fixture = self::fixture(&[TaskType::GitPush]);
        fixture.lease.network_scope = EgressProfile::Broker;
        let admission = validate_action(input(&fixture, TaskType::GitPush, &target));
        assert_eq!(
            admission.primary_reason(),
            Some(&PolicyReason::CredentialsWiderThanLease),
            "a lease with no credential scope cannot push"
        );

        let mut fixture = self::fixture(&[TaskType::GitPush]);
        fixture.lease.network_scope = EgressProfile::Broker;
        fixture.lease.credential_scope = CredentialPolicy::BrokeredGitPush;
        let admission = validate_action(input(&fixture, TaskType::GitPush, &target));
        assert!(admission.needs_approval(), "{:?}", admission.reasons);
    }

    #[test]
    fn budget_exhaustion_denies_at_admission() {
        let mut fixture = fixture(&[TaskType::RustCheck]);
        fixture.lease.usage = BudgetUsage {
            task_runs: 10,
            ..BudgetUsage::default()
        };
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustCheck, &target));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::BudgetExhausted { .. })
        ));
    }

    #[test]
    fn configuration_narrowing_is_visible_in_the_notes() {
        let mut fixture = fixture(&[TaskType::RustCheck]);
        let (config, _) = crate::config::apply_layer(
            fixture.config.clone(),
            "[limits.build]\nmax_memory_bytes = 1073741824\n",
            crate::config::ConfigSource::Repository,
        )
        .expect("narrowing applies");
        fixture.config = config;
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustCheck, &target));
        assert!(admission.is_allowed());
        assert!(
            admission.notes.iter().any(|note| note.contains("narrowed")),
            "notes: {:?}",
            admission.notes
        );
        assert_eq!(
            admission
                .policy
                .map(|policy| policy.limits.max_memory_bytes),
            Some(1 << 30)
        );
    }

    #[test]
    fn a_disabled_task_is_reported_as_unavailable() {
        let mut fixture = fixture(&[TaskType::RustTestUnit]);
        let (config, _) = crate::config::apply_layer(
            fixture.config.clone(),
            "[tasks.\"rust.test.unit\"]\nenabled = false\n",
            crate::config::ConfigSource::Repository,
        )
        .expect("disabling applies");
        fixture.config = config;
        let target = path("crates/core");
        let admission = validate_action(input(&fixture, TaskType::RustTestUnit, &target));
        assert!(matches!(
            admission.primary_reason(),
            Some(PolicyReason::PhaseGated { .. })
        ));
    }

    #[test]
    fn resolution_is_independent_of_lease_and_mission() {
        // resolve_task_policy answers a catalog-and-configuration question only;
        // the mission and lease are consulted by validate_action.
        let fixture = fixture(&[TaskType::RustCheck]);
        let resolved = resolve_task_policy(
            TaskType::RustCheck,
            &path("crates/core"),
            &fixture.mission,
            &fixture.lease,
            &fixture.config,
        )
        .expect("resolution");
        assert_eq!(resolved.policy.task, TaskType::RustCheck);
        assert!(resolved.policy.egress.is_none());
    }
}
