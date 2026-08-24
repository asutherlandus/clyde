//! Lease derivation.
//!
//! All eight derivation rules from the schema reference, as one pure function
//! over a parent lease and a request. This is the highest-value unit-test target
//! in the codebase, and the tests below start from the denial cases.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use clyde_core::budget::Budget;
use clyde_core::classification::{CredentialPolicy, EgressProfile};
use clyde_core::decision::PolicyReason;
use clyde_core::ids::{ActorId, LeaseId};
use clyde_core::lease::{AuthorityFlags, Lease, LeaseState};
use clyde_core::mission::{MissionScope, ScopeViolation};
use clyde_core::task::TaskType;

use crate::egress::{EgressComparison, is_no_wider_than};

/// A request to derive a child lease.
///
/// Every field the child would hold is stated explicitly: derivation never
/// inherits a value implicitly, because an inherited value is one a reviewer of
/// the sub-agent's authority would have to go and look up.
#[derive(Debug, Clone)]
pub struct LeaseDerivationRequest {
    pub id: LeaseId,
    pub actor: ActorId,
    pub issued_by: ActorId,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub repo_scope: MissionScope,
    pub task_scope: BTreeSet<TaskType>,
    pub network_scope: EgressProfile,
    pub credential_scope: CredentialPolicy,
    pub authority: AuthorityFlags,
    pub budget: Budget,
    pub purpose: String,
}

/// Derives a child lease from `parent`, or explains why it cannot be derived.
///
/// The rules are checked in the documented order so a denial names the first,
/// most specific violation rather than a generic "not permitted".
pub fn derive_lease(
    parent: &Lease,
    request: LeaseDerivationRequest,
) -> Result<Lease, PolicyReason> {
    // A lease that cannot admit work cannot be a parent, checked before the
    // scope rules so that an expired parent is reported as expired rather than
    // as a scope violation.
    if parent.state != LeaseState::Active {
        return Err(match parent.state {
            LeaseState::Expired => PolicyReason::LeaseExpired,
            _ => PolicyReason::LeaseNotActive,
        });
    }
    if parent.is_expired_at(request.issued_at) {
        return Err(PolicyReason::LeaseExpired);
    }

    // Rules 1 and 2: repo scope.
    request
        .repo_scope
        .is_derivable_from(&parent.repo_scope)
        .map_err(|violation| match violation {
            ScopeViolation::EditNotInParent { path } => {
                PolicyReason::NotWritableUnderLease { path }
            }
            ScopeViolation::ReadNotInParent { path } => PolicyReason::OutOfLeaseScope { path },
        })?;

    // Rule 3: task scope.
    if let Some(task) = request
        .task_scope
        .iter()
        .find(|task| !parent.task_scope.contains(task))
    {
        return Err(PolicyReason::TaskNotInLease { task: *task });
    }

    // Rule 4: network scope, via the partial order. Incomparable is a denial
    // with its own diagnostic, because the remedy differs.
    match is_no_wider_than(&request.network_scope, &parent.network_scope) {
        EgressComparison::NoWider => {}
        EgressComparison::Wider => {
            return Err(PolicyReason::EgressWiderThanLease {
                requested: request.network_scope.to_string(),
                ceiling: parent.network_scope.to_string(),
            });
        }
        EgressComparison::Incomparable => {
            return Err(PolicyReason::EgressProfilesIncomparable {
                requested: request.network_scope.to_string(),
                ceiling: parent.network_scope.to_string(),
            });
        }
    }

    // Rule 5: credential scope.
    if !request
        .credential_scope
        .is_no_wider_than(parent.credential_scope)
    {
        return Err(PolicyReason::CredentialsWiderThanLease);
    }

    // Rule 6: authority flags may only narrow, and a derived lease may never
    // spawn further sub-agents (one level of derivation in the MVP).
    if let Some(flag) = request.authority.is_no_wider_than(parent.authority) {
        return Err(PolicyReason::AuthorityFlagNotHeld {
            flag: flag.to_owned(),
        });
    }
    if request.authority.may_spawn_subagents {
        return Err(PolicyReason::SubagentDerivationDepthExceeded);
    }
    if parent.parent.is_some() {
        return Err(PolicyReason::SubagentDerivationDepthExceeded);
    }

    // Rule 7: expiry.
    if request.expires_at > parent.expires_at {
        return Err(PolicyReason::LeaseExpired);
    }

    // Rule 8: budget is within the parent's *remaining* budget, not its ceiling,
    // so a parent that has already spent cannot lend what it no longer has.
    let remaining = parent.budget.remaining(&parent.usage);
    if let Some(dimension) = budget_exceeds_remaining(&request.budget, &remaining) {
        return Err(PolicyReason::BudgetWiderThanParent { dimension });
    }

    Ok(Lease {
        id: request.id,
        mission: parent.mission.clone(),
        parent: Some(parent.id.clone()),
        actor: request.actor,
        issued_by: request.issued_by,
        issued_at: request.issued_at,
        expires_at: request.expires_at,
        repo_scope: request.repo_scope,
        task_scope: request.task_scope,
        network_scope: request.network_scope,
        credential_scope: request.credential_scope,
        authority: request.authority,
        budget: request.budget,
        usage: clyde_core::budget::BudgetUsage::default(),
        state: LeaseState::Issued,
        purpose: request.purpose,
    })
}

/// Which dimension of `requested` exceeds `remaining`, if any.
fn budget_exceeds_remaining(
    requested: &Budget,
    remaining: &clyde_core::budget::BudgetRemaining,
) -> Option<clyde_core::budget::BudgetDimension> {
    use clyde_core::budget::BudgetDimension as D;
    [
        (
            requested.max_duration.as_secs() > remaining.duration_seconds,
            D::Duration,
        ),
        (requested.max_task_runs > remaining.task_runs, D::TaskRuns),
        (
            requested.max_parallel_subagents > remaining.parallel_subagents,
            D::ParallelSubagents,
        ),
        (requested.max_subagents > remaining.subagents, D::Subagents),
        (
            requested.max_cpu_seconds > remaining.cpu_seconds,
            D::CpuSeconds,
        ),
        (
            requested.max_cache_bytes > remaining.cache_bytes,
            D::CacheBytes,
        ),
        (
            requested.max_artifact_bytes > remaining.artifact_bytes,
            D::ArtifactBytes,
        ),
        (
            requested.max_egress_bytes > remaining.egress_bytes,
            D::EgressBytes,
        ),
        (
            requested.max_egress_requests > remaining.egress_requests,
            D::EgressRequests,
        ),
    ]
    .into_iter()
    .find_map(|(exceeded, dimension)| exceeded.then_some(dimension))
}

/// What a `request_subagent` call asks for, before derivation checks it.
#[derive(Debug, Clone)]
pub struct SubagentAsk {
    pub id: LeaseId,
    pub actor: ActorId,
    pub issued_by: ActorId,
    pub now: DateTime<Utc>,
    pub scope: MissionScope,
    pub tasks: BTreeSet<TaskType>,
    pub budget: Budget,
    pub purpose: String,
}

/// Builds a derivation request that narrows the parent in exactly one dimension:
/// the repository scope. Everything else is inherited at or below the parent.
///
/// This is the shape a `request_subagent` call takes in practice, and having it
/// here keeps the "inherit safely" logic in one tested place rather than in the
/// daemon.
pub fn narrowed_request(parent: &Lease, ask: SubagentAsk) -> LeaseDerivationRequest {
    let SubagentAsk {
        id,
        actor,
        issued_by,
        now,
        scope,
        tasks,
        budget,
        purpose,
    } = ask;
    let remaining = parent.budget.remaining(&parent.usage);
    LeaseDerivationRequest {
        id,
        actor,
        issued_by,
        issued_at: now,
        expires_at: parent.expires_at,
        repo_scope: scope,
        task_scope: tasks.intersection(&parent.task_scope).copied().collect(),
        network_scope: parent.network_scope.clone(),
        credential_scope: parent.credential_scope,
        authority: AuthorityFlags {
            may_edit: parent.authority.may_edit,
            may_request_tasks: parent.authority.may_request_tasks,
            // Never inherited: one level of derivation only.
            may_spawn_subagents: false,
            // Publishing authority is not inherited by a sub-agent: a narrower
            // actor should not gain the widest capability by default.
            may_request_publish: false,
        },
        budget: budget.narrowed_to(Budget {
            max_duration: clyde_core::HumanDuration::from_duration(std::time::Duration::from_secs(
                remaining.duration_seconds.max(1),
            ))
            .unwrap_or(clyde_core::HumanDuration::MINIMUM),
            max_task_runs: remaining.task_runs,
            max_parallel_subagents: remaining.parallel_subagents,
            max_subagents: remaining.subagents,
            max_cpu_seconds: remaining.cpu_seconds,
            max_cache_bytes: remaining.cache_bytes,
            max_artifact_bytes: remaining.artifact_bytes,
            max_egress_bytes: remaining.egress_bytes,
            max_egress_requests: remaining.egress_requests,
        }),
        purpose,
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
    use clyde_core::HumanDuration;
    use clyde_core::budget::{BudgetCost, BudgetUsage};
    use clyde_core::classification::HostName;
    use clyde_core::ids;
    use clyde_core::repo_path::RepoPath;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn budget(task_runs: u32) -> Budget {
        Budget {
            max_duration: HumanDuration::parse("1h").unwrap(),
            max_task_runs: task_runs,
            max_parallel_subagents: 2,
            max_subagents: 2,
            max_cpu_seconds: 600,
            max_cache_bytes: 1 << 30,
            max_artifact_bytes: 1 << 28,
            max_egress_bytes: 1 << 20,
            max_egress_requests: 100,
        }
    }

    fn parent() -> Lease {
        let issued = Utc::now();
        Lease {
            id: ids::new::lease_id().unwrap(),
            mission: ids::new::mission_id().unwrap(),
            parent: None,
            actor: ActorId::parse("agent:claude").unwrap(),
            issued_by: ActorId::parse("human:clyde").unwrap(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::hours(1),
            repo_scope: MissionScope {
                edit_paths: [path("crates/core")].into_iter().collect(),
                read_paths: [path("docs")].into_iter().collect(),
            },
            task_scope: [TaskType::RustCheck, TaskType::WorkspaceEdit]
                .into_iter()
                .collect(),
            network_scope: EgressProfile::ModelApi,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags {
                may_edit: true,
                may_request_tasks: true,
                may_spawn_subagents: true,
                may_request_publish: true,
            },
            budget: budget(10),
            usage: BudgetUsage::default(),
            state: LeaseState::Active,
            purpose: "primary".to_owned(),
        }
    }

    fn request(parent: &Lease) -> LeaseDerivationRequest {
        LeaseDerivationRequest {
            id: ids::new::lease_id().unwrap(),
            actor: ActorId::parse("agent:claude/1").unwrap(),
            issued_by: ActorId::parse("human:clyde").unwrap(),
            issued_at: parent.issued_at,
            expires_at: parent.expires_at,
            repo_scope: MissionScope {
                edit_paths: [path("crates/core/src")].into_iter().collect(),
                read_paths: BTreeSet::new(),
            },
            task_scope: [TaskType::RustCheck].into_iter().collect(),
            network_scope: EgressProfile::ModelApi,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags {
                may_edit: true,
                may_request_tasks: true,
                may_spawn_subagents: false,
                may_request_publish: false,
            },
            budget: budget(3),
            purpose: "sub-agent".to_owned(),
        }
    }

    #[test]
    fn a_valid_narrowing_derives() {
        let parent = parent();
        let derived = derive_lease(&parent, request(&parent)).expect("narrowing must derive");
        assert_eq!(derived.parent, Some(parent.id.clone()));
        assert_eq!(derived.mission, parent.mission);
        assert_eq!(derived.state, LeaseState::Issued);
        assert_eq!(derived.usage, BudgetUsage::default());
    }

    #[test]
    fn rule_1_edit_paths_must_be_inside_the_parent() {
        let parent = parent();
        let mut request = request(&parent);
        request.repo_scope.edit_paths = [path("crates")].into_iter().collect();
        assert!(matches!(
            derive_lease(&parent, request),
            Err(PolicyReason::NotWritableUnderLease { .. })
        ));
    }

    #[test]
    fn rule_2_read_paths_may_come_from_the_parents_edit_paths() {
        let parent = parent();
        let mut request = request(&parent);
        request.repo_scope.edit_paths = BTreeSet::new();
        request.repo_scope.read_paths = [path("crates/core/src")].into_iter().collect();
        assert!(derive_lease(&parent, request).is_ok());

        let mut outside = self::request(&parent);
        outside.repo_scope.read_paths = [path("infra")].into_iter().collect();
        assert!(matches!(
            derive_lease(&parent, outside),
            Err(PolicyReason::OutOfLeaseScope { .. })
        ));
    }

    #[test]
    fn rule_3_task_scope_must_be_a_subset() {
        let parent = parent();
        let mut request = request(&parent);
        request.task_scope = [TaskType::GitPush].into_iter().collect();
        assert!(matches!(
            derive_lease(&parent, request),
            Err(PolicyReason::TaskNotInLease {
                task: TaskType::GitPush
            })
        ));
    }

    #[test]
    fn rule_4_network_scope_may_not_widen_and_incomparable_is_denied() {
        let parent = parent();
        let mut wider = request(&parent);
        wider.network_scope = EgressProfile::Custom {
            hosts: [HostName::parse("evil.test").unwrap()]
                .into_iter()
                .collect(),
        };
        assert!(matches!(
            derive_lease(&parent, wider),
            Err(PolicyReason::EgressWiderThanLease { .. })
        ));

        let mut sideways = request(&parent);
        sideways.network_scope = EgressProfile::RustRegistry;
        assert!(
            matches!(
                derive_lease(&parent, sideways),
                Err(PolicyReason::EgressProfilesIncomparable { .. })
            ),
            "model-api cannot derive rust-registry; that needs an escalation"
        );

        let mut narrower = request(&parent);
        narrower.network_scope = EgressProfile::None;
        assert!(derive_lease(&parent, narrower).is_ok());
    }

    #[test]
    fn rule_5_credential_scope_may_not_widen() {
        let parent = parent();
        let mut request = request(&parent);
        request.credential_scope = CredentialPolicy::BrokeredGitPush;
        assert!(matches!(
            derive_lease(&parent, request),
            Err(PolicyReason::CredentialsWiderThanLease)
        ));
    }

    #[test]
    fn rule_6_flags_may_not_widen_and_subagents_cannot_nest() {
        let mut parent = parent();
        parent.authority.may_request_publish = false;
        let mut request = request(&parent);
        request.authority.may_request_publish = true;
        assert!(matches!(
            derive_lease(&parent, request),
            Err(PolicyReason::AuthorityFlagNotHeld { .. })
        ));

        let parent = self::parent();
        let mut nesting = self::request(&parent);
        nesting.authority.may_spawn_subagents = true;
        assert!(matches!(
            derive_lease(&parent, nesting),
            Err(PolicyReason::SubagentDerivationDepthExceeded)
        ));

        // A lease that is itself derived cannot be a parent.
        let derived = derive_lease(&parent, self::request(&parent)).unwrap();
        let mut active_child = derived;
        active_child.state = LeaseState::Active;
        active_child.authority.may_spawn_subagents = true;
        assert!(matches!(
            derive_lease(&active_child, self::request(&parent)),
            Err(PolicyReason::SubagentDerivationDepthExceeded)
        ));
    }

    #[test]
    fn rule_7_expiry_may_not_exceed_the_parents() {
        let parent = parent();
        let mut request = request(&parent);
        request.expires_at = parent.expires_at + chrono::Duration::minutes(1);
        assert!(matches!(
            derive_lease(&parent, request),
            Err(PolicyReason::LeaseExpired)
        ));
    }

    #[test]
    fn rule_8_budget_is_bounded_by_remaining_not_by_the_ceiling() {
        let mut parent = parent();
        // The parent has spent 8 of its 10 task runs.
        parent.usage = BudgetUsage {
            task_runs: 8,
            ..BudgetUsage::default()
        };
        let mut request = request(&parent);
        request.budget = budget(3);
        assert!(
            matches!(
                derive_lease(&parent, request),
                Err(PolicyReason::BudgetWiderThanParent {
                    dimension: clyde_core::budget::BudgetDimension::TaskRuns
                })
            ),
            "a parent cannot lend budget it has already spent"
        );

        let mut within = self::request(&parent);
        within.budget = budget(2);
        assert!(derive_lease(&parent, within).is_ok());
    }

    #[test]
    fn an_inactive_or_expired_parent_cannot_derive() {
        let mut parent = parent();
        parent.state = LeaseState::Revoked;
        assert!(matches!(
            derive_lease(&parent, self::request(&parent)),
            Err(PolicyReason::LeaseNotActive)
        ));

        let mut expired = self::parent();
        expired.state = LeaseState::Expired;
        assert!(matches!(
            derive_lease(&expired, self::request(&expired)),
            Err(PolicyReason::LeaseExpired)
        ));

        let mut stale = self::parent();
        let mut request = self::request(&stale);
        stale.expires_at = stale.issued_at - chrono::Duration::seconds(1);
        request.expires_at = stale.expires_at;
        assert!(matches!(
            derive_lease(&stale, request),
            Err(PolicyReason::LeaseExpired)
        ));
    }

    #[test]
    fn narrowed_request_never_inherits_spawn_or_publish() {
        let parent = parent();
        let request = narrowed_request(
            &parent,
            SubagentAsk {
                id: ids::new::lease_id().unwrap(),
                actor: ActorId::parse("agent:claude/1").unwrap(),
                issued_by: ActorId::parse("human:clyde").unwrap(),
                now: parent.issued_at,
                scope: MissionScope {
                    edit_paths: [path("crates/core/src")].into_iter().collect(),
                    read_paths: BTreeSet::new(),
                },
                tasks: [TaskType::RustCheck, TaskType::GitPush]
                    .into_iter()
                    .collect(),
                budget: budget(2),
                purpose: "sub".to_owned(),
            },
        );
        assert!(!request.authority.may_spawn_subagents);
        assert!(!request.authority.may_request_publish);
        // Tasks are intersected with the parent's, so an over-broad ask narrows
        // rather than failing.
        assert_eq!(
            request.task_scope,
            [TaskType::RustCheck].into_iter().collect::<BTreeSet<_>>()
        );
        assert!(derive_lease(&parent, request).is_ok());
    }

    #[test]
    fn narrowed_request_respects_spent_parent_budget() {
        let mut parent = parent();
        parent.usage = BudgetUsage::default().saturating_add(&BudgetCost {
            task_runs: 9,
            ..BudgetCost::default()
        });
        let request = narrowed_request(
            &parent,
            SubagentAsk {
                id: ids::new::lease_id().unwrap(),
                actor: ActorId::parse("agent:claude/1").unwrap(),
                issued_by: ActorId::parse("human:clyde").unwrap(),
                now: parent.issued_at,
                scope: MissionScope {
                    edit_paths: [path("crates/core/src")].into_iter().collect(),
                    read_paths: BTreeSet::new(),
                },
                tasks: [TaskType::RustCheck].into_iter().collect(),
                budget: budget(10),
                purpose: "sub".to_owned(),
            },
        );
        assert_eq!(request.budget.max_task_runs, 1);
        assert!(derive_lease(&parent, request).is_ok());
    }
}
