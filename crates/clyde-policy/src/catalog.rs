//! The built-in MVP task catalog (Phase 0 deliverable 6).
//!
//! The catalog is closed and encoded as a function over [`TaskType`], so every
//! task has exactly one built-in policy and adding a task is a compile-time
//! change. `.clyde/policy.toml` may only narrow these values (D14).

use clyde_core::HumanDuration;
use clyde_core::classification::{
    ApprovalRequirement, AuditLevel, CachePolicy, CredentialPolicy, EgressProfile, Environment,
    InputSpec, IsolationLevel, OutputSpec, ResourceLimits, TrustClass,
};
use clyde_core::task::{RuntimeRootKind, TaskPolicy, TaskType};

/// Wall-clock and resource ceilings by task family.
///
/// These are the *built-in* ceilings. A host, user, or repository may narrow
/// them; nothing can widen them.
fn limits(wall_clock_secs: u64, memory_gib: u64, cpu_percent: u32, tasks: u32) -> ResourceLimits {
    ResourceLimits {
        max_wall_clock: HumanDuration::from_duration(std::time::Duration::from_secs(
            wall_clock_secs,
        ))
        .unwrap_or(HumanDuration::MINIMUM),
        max_memory_bytes: memory_gib.saturating_mul(1 << 30),
        max_cpu_percent: cpu_percent,
        max_tasks: tasks,
        max_open_files: 4096,
    }
}

/// The built-in policy for a task type.
///
/// Every field is stated explicitly rather than defaulted, because a default
/// that silently widened a boundary would be invisible in review.
pub fn builtin_policy(task: TaskType) -> TaskPolicy {
    match task {
        // T0/T1 workspace activity. The agent is already inside the workspace
        // environment (D1), so these describe classes of activity for audit and
        // policy purposes rather than Clyde-launched sandboxes (D17).
        TaskType::WorkspaceRead => TaskPolicy {
            task,
            environment: Environment::Workspace,
            trust_class: TrustClass::T0,
            min_isolation: IsolationLevel::NamespaceSandbox,
            input: InputSpec::LiveWorkspace,
            outputs: vec![OutputSpec::Log],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::None,
            limits: limits(300, 2, 100, 64),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Summary,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::Workspace,
        },
        TaskType::WorkspaceEdit => TaskPolicy {
            task,
            environment: Environment::Workspace,
            trust_class: TrustClass::T1,
            min_isolation: IsolationLevel::NamespaceSandbox,
            input: InputSpec::LiveWorkspace,
            outputs: vec![OutputSpec::Log, OutputSpec::Diff],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::None,
            limits: limits(900, 2, 100, 64),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Detailed,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::Workspace,
        },
        TaskType::RepoSearch => TaskPolicy {
            task,
            environment: Environment::Workspace,
            trust_class: TrustClass::T0,
            min_isolation: IsolationLevel::NamespaceSandbox,
            input: InputSpec::LiveWorkspace,
            outputs: vec![OutputSpec::Log],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::None,
            limits: limits(300, 2, 100, 64),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Summary,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::Workspace,
        },

        // T2: project and dependency code executes. Offline by construction, and
        // a confirmed access baseline is required before either can run (D18).
        TaskType::RustCheck => TaskPolicy {
            task,
            environment: Environment::Build,
            trust_class: TrustClass::T2,
            min_isolation: IsolationLevel::NamespaceSandbox,
            input: InputSpec::Snapshot {
                closure_aware: true,
            },
            outputs: vec![OutputSpec::Log, OutputSpec::BuildOutput],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::MissionScoped {
                cargo_home: true,
                target_dir: true,
            },
            limits: limits(1800, 8, 400, 512),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Detailed,
            requires_access_baseline: true,
            runtime_root: RuntimeRootKind::Rust,
        },
        TaskType::RustTestUnit => TaskPolicy {
            task,
            environment: Environment::Build,
            trust_class: TrustClass::T2,
            min_isolation: IsolationLevel::NamespaceSandbox,
            input: InputSpec::Snapshot {
                closure_aware: true,
            },
            outputs: vec![OutputSpec::Log, OutputSpec::BuildOutput],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::MissionScoped {
                cargo_home: true,
                target_dir: true,
            },
            limits: limits(2700, 8, 400, 512),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Detailed,
            requires_access_baseline: true,
            runtime_root: RuntimeRootKind::Rust,
        },

        // T3: the only task with network reachability, and the reason Phase 2b
        // precedes Phase 3 (D9). Input is manifests only: a fetch has no reason
        // to see application code.
        TaskType::RustResolveDeps => TaskPolicy {
            task,
            environment: Environment::Build,
            trust_class: TrustClass::T3,
            min_isolation: IsolationLevel::MicroVm,
            input: InputSpec::ManifestsOnly,
            outputs: vec![
                OutputSpec::Log,
                OutputSpec::DependencyBundle,
                OutputSpec::FetchManifest,
            ],
            egress: EgressProfile::RustRegistry,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::WritesDependencyBundle,
            limits: limits(1200, 4, 200, 256),
            approval: ApprovalRequirement::HumanRequired,
            audit_level: AuditLevel::Full,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::Fetch,
        },

        // T1 in the control plane: commit creation is trusted, not an agent
        // operation, so an agent cannot plant hooks a later git run would
        // execute (D8).
        TaskType::GitCommitPrepare => TaskPolicy {
            task,
            environment: Environment::ControlPlane,
            trust_class: TrustClass::T1,
            min_isolation: IsolationLevel::InProcess,
            input: InputSpec::LiveWorkspace,
            outputs: vec![OutputSpec::CommitProposal, OutputSpec::Diff],
            egress: EgressProfile::None,
            credentials: CredentialPolicy::None,
            cache: CachePolicy::None,
            limits: limits(300, 2, 100, 32),
            approval: ApprovalRequirement::None,
            audit_level: AuditLevel::Detailed,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::None,
        },

        // T4: brokered authority, outside any sandbox.
        TaskType::GitPush => TaskPolicy {
            task,
            environment: Environment::Broker,
            trust_class: TrustClass::T4,
            min_isolation: IsolationLevel::Broker,
            input: InputSpec::CommitRef,
            outputs: vec![OutputSpec::Log],
            egress: EgressProfile::Broker,
            credentials: CredentialPolicy::BrokeredGitPush,
            cache: CachePolicy::None,
            limits: limits(600, 2, 100, 32),
            approval: ApprovalRequirement::HumanRequired,
            audit_level: AuditLevel::Full,
            requires_access_baseline: false,
            runtime_root: RuntimeRootKind::None,
        },
    }
}

/// Every built-in policy, in catalog order.
pub fn catalog() -> Vec<TaskPolicy> {
    TaskType::ALL.into_iter().map(builtin_policy).collect()
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
    fn every_task_type_has_a_valid_built_in_policy() {
        for task in TaskType::ALL {
            let policy = builtin_policy(task);
            assert_eq!(policy.task, task);
            policy
                .validate()
                .unwrap_or_else(|error| panic!("{task} policy invalid: {error}"));
            assert!(policy.digest().is_ok());
        }
        assert_eq!(catalog().len(), TaskType::ALL.len());
    }

    #[test]
    fn policy_digests_are_distinct_per_task() {
        let mut digests: Vec<String> = catalog()
            .iter()
            .map(|policy| policy.digest().map(|d| d.to_string()).unwrap_or_default())
            .collect();
        let before = digests.len();
        digests.sort();
        digests.dedup();
        assert_eq!(digests.len(), before, "two tasks share a policy digest");
    }

    #[test]
    fn egress_defaults_to_none_except_where_the_model_requires_otherwise() {
        // The network egress model states `none` is the default for every task
        // type; a profile other than `none` must be named explicitly.
        for policy in catalog() {
            match policy.task {
                TaskType::RustResolveDeps => {
                    assert_eq!(policy.egress, EgressProfile::RustRegistry)
                }
                TaskType::GitPush => assert_eq!(policy.egress, EgressProfile::Broker),
                other => assert_eq!(
                    policy.egress,
                    EgressProfile::None,
                    "{other} must have no egress"
                ),
            }
        }
    }

    #[test]
    fn compile_and_test_are_offline_and_baselined() {
        for task in [TaskType::RustCheck, TaskType::RustTestUnit] {
            let policy = builtin_policy(task);
            assert!(
                policy.egress.is_none(),
                "{task} must never reach the network"
            );
            assert!(
                policy.requires_access_baseline,
                "{task} must be refused without a confirmed baseline"
            );
            assert_eq!(policy.trust_class, TrustClass::T2);
            assert!(policy.trust_class.requires_cgroup_limits());
            assert_eq!(policy.runtime_root, RuntimeRootKind::Rust);
        }
    }

    #[test]
    fn resolve_deps_requires_a_microvm_and_human_approval() {
        let policy = builtin_policy(TaskType::RustResolveDeps);
        assert_eq!(policy.min_isolation, IsolationLevel::MicroVm);
        assert_eq!(policy.approval, ApprovalRequirement::HumanRequired);
        assert_eq!(policy.credentials, CredentialPolicy::None);
        assert_eq!(policy.input, InputSpec::ManifestsOnly);
    }

    #[test]
    fn no_task_but_push_touches_a_credential() {
        for policy in catalog() {
            match policy.task {
                TaskType::GitPush => {
                    assert_eq!(policy.credentials, CredentialPolicy::BrokeredGitPush)
                }
                other => assert_eq!(
                    policy.credentials,
                    CredentialPolicy::None,
                    "{other} must hold no credential"
                ),
            }
        }
    }

    #[test]
    fn workspace_tasks_never_use_the_rust_runtime_root() {
        // The workspace runtime root has no project build toolchain, which is
        // what makes run_task the only path to executing project code (D17).
        for task in [
            TaskType::WorkspaceRead,
            TaskType::WorkspaceEdit,
            TaskType::RepoSearch,
        ] {
            assert_eq!(
                builtin_policy(task).runtime_root,
                RuntimeRootKind::Workspace
            );
        }
    }

    #[test]
    fn build_tasks_are_snapshot_input_never_live() {
        for task in [TaskType::RustCheck, TaskType::RustTestUnit] {
            assert!(matches!(
                builtin_policy(task).input,
                InputSpec::Snapshot {
                    closure_aware: true
                }
            ));
        }
    }
}
