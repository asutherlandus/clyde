//! Store conformance tests.
//!
//! Both implementations are exercised through the same suite, so a behavioural
//! difference between the in-memory store the tests use and the SQLite store the
//! daemon uses is a test failure rather than a surprise in production.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::Utc;
use clyde_core::HumanDuration;

use clyde_core::approval::{ApprovalDecision, ApprovalRequest, ApprovalSubject, Decision};
use clyde_core::artifact::{Artifact, ArtifactKind};
use clyde_core::audit::{AuditEventDraft, AuditEventKind, ChainViolation};
use clyde_core::baseline::{
    AccessBaseline, BaselineKey, BaselineOrigin, BaselinePathEntry, CodeExecInventory, GrantSource,
};
use clyde_core::broker::{BrokerOpState, BrokeredKind, BrokeredOperation};
use clyde_core::budget::{Budget, BudgetCost, BudgetUsage};
use clyde_core::classification::{BackendKind, CredentialPolicy, EgressProfile, TrustClass};
use clyde_core::digest::Digest;
use clyde_core::ids::{self, ActorId};
use clyde_core::lease::{AuthorityFlags, Lease, LeaseState};
use clyde_core::mission::{
    ApprovalPolicy, Mission, MissionScope, MissionState, NetworkPolicy, StopCondition,
};
use clyde_core::repo_path::RepoPath;
use clyde_core::session::{ActorSession, SessionToken};
use clyde_core::task::{
    TaskFailureClass, TaskOptions, TaskOutcome, TaskRequest, TaskRun, TaskRunState, TaskType,
};
use clyde_core::workspace::{VcsKind, Workspace};

use crate::error::StoreError;
use crate::store::Store;
use crate::types::{AuditFilter, LeaseRenewal, MissionCloseout};
use crate::{MemoryStore, SqliteStore};

fn path(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

fn workspace(root: &str) -> Workspace {
    Workspace {
        id: ids::new::workspace_id().unwrap(),
        root: PathBuf::from(root),
        vcs: VcsKind::Git {
            default_remote: Some("origin".to_owned()),
            default_branch: Some("main".to_owned()),
        },
        registered_at: Utc::now(),
        policy_digest: None,
    }
}

fn budget(task_runs: u32) -> Budget {
    Budget {
        max_duration: HumanDuration::parse("2h").unwrap(),
        max_task_runs: task_runs,
        max_parallel_subagents: 2,
        max_subagents: 2,
        max_cpu_seconds: 3600,
        max_cache_bytes: 1 << 30,
        max_artifact_bytes: 1 << 28,
        max_egress_bytes: 1 << 20,
        max_egress_requests: 20,
    }
}

fn mission(workspace: &Workspace) -> Mission {
    let created = Utc::now();
    Mission {
        id: ids::new::mission_id().unwrap(),
        workspace: workspace.id.clone(),
        objective: "tidy the core crate".to_owned(),
        initiator: ActorId::parse("human:andrew").unwrap(),
        primary_actor: ActorId::parse("agent:claude").unwrap(),
        scope: MissionScope {
            edit_paths: [path("crates/core")].into_iter().collect(),
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
        budget: budget(10),
        expiry: created + chrono::Duration::hours(2),
        state: MissionState::Proposed,
        stop_conditions: [StopCondition::BudgetExhausted].into_iter().collect(),
        success_criteria: vec!["tests pass".to_owned()],
        cache_dir: None,
        created_at: created,
        closed_at: None,
    }
}

fn lease(mission: &Mission, parent: Option<&Lease>) -> Lease {
    let issued = Utc::now();
    Lease {
        id: ids::new::lease_id().unwrap(),
        mission: mission.id.clone(),
        parent: parent.map(|lease| lease.id.clone()),
        actor: match parent {
            Some(_) => ActorId::parse("agent:claude/1").unwrap(),
            None => ActorId::parse("agent:claude").unwrap(),
        },
        issued_by: ActorId::parse("human:clyde").unwrap(),
        issued_at: issued,
        expires_at: issued + chrono::Duration::hours(1),
        repo_scope: mission.scope.clone(),
        task_scope: mission.allowed_tasks.clone(),
        network_scope: EgressProfile::ModelApi,
        credential_scope: CredentialPolicy::None,
        authority: AuthorityFlags {
            may_edit: true,
            may_request_tasks: true,
            may_spawn_subagents: parent.is_none(),
            may_request_publish: false,
        },
        budget: budget(10),
        usage: BudgetUsage::default(),
        state: LeaseState::Active,
        purpose: "test lease".to_owned(),
    }
}

fn session(lease: &Lease, token: &SessionToken) -> ActorSession {
    ActorSession {
        actor: lease.actor.clone(),
        lease: lease.id.clone(),
        token_hash: token.hash(),
        issued_at: lease.issued_at,
        expires_at: lease.expires_at,
        revoked_at: None,
        sandbox: None,
    }
}

fn task_run(lease: &Lease) -> TaskRun {
    let id = ids::new::task_run_id().unwrap();
    let request = TaskRequest {
        id: id.clone(),
        lease: lease.id.clone(),
        actor: lease.actor.clone(),
        principal: clyde_core::actor::Principal::Session {
            session_actor: lease.actor.clone(),
            hosted: true,
        },
        task: TaskType::RustCheck,
        path: path("crates/core"),
        options: TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
        requested_at: Utc::now(),
    };
    TaskRun {
        id,
        request,
        policy_digest: Digest::of_bytes(b"policy"),
        posture: clyde_core::posture::Posture::Advisory {
            reasons: vec![clyde_core::posture::BypassReason::NoHostedActor],
        },
        snapshot: None,
        dependency_bundle: None,
        backend: BackendKind::Bubblewrap,
        state: TaskRunState::Requested,
        started_at: None,
        finished_at: None,
        outcome: None,
        artifacts: Vec::new(),
    }
}

fn approval_request(mission: &Mission, lease: &Lease, digest: Digest) -> ApprovalRequest {
    let now = Utc::now();
    ApprovalRequest {
        id: ids::new::approval_id().unwrap(),
        mission: mission.id.clone(),
        lease: lease.id.clone(),
        actor: lease.actor.clone(),
        subject: ApprovalSubject::BrokeredOperation {
            summary: "push".to_owned(),
        },
        request_digest: digest,
        reason: "needs review".to_owned(),
        alternatives: Vec::new(),
        created_at: now,
        expires_at: now + chrono::Duration::minutes(15),
    }
}

fn decision(request: &ApprovalRequest, kind: Decision) -> ApprovalDecision {
    ApprovalDecision {
        request: request.id.clone(),
        decided_by: ActorId::parse("human:andrew").unwrap(),
        decision: kind,
        decided_at: Utc::now(),
        note: None,
        consumed_at: None,
    }
}

/// Runs `body` against both store implementations.
fn each_store(body: impl Fn(&dyn Store, &'static str)) {
    let memory = MemoryStore::new();
    body(&memory, "memory");
    let dir = tempfile::tempdir().expect("temp dir");
    let sqlite = SqliteStore::open(dir.path().join("db.sqlite")).expect("open sqlite");
    body(&sqlite, "sqlite");
}

#[test]
fn a_workspace_round_trips_and_is_findable_by_root() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        assert_eq!(
            store.get_workspace(&workspace.id).unwrap(),
            workspace,
            "{label}"
        );
        assert_eq!(
            store
                .find_workspace_by_root(&PathBuf::from("/srv/project"))
                .unwrap()
                .map(|found| found.id),
            Some(workspace.id.clone()),
            "{label}"
        );
        assert!(
            store
                .find_workspace_by_root(&PathBuf::from("/srv/other"))
                .unwrap()
                .is_none()
        );
        assert_eq!(store.list_workspaces().unwrap().len(), 1, "{label}");
    });
}

#[test]
fn a_relative_workspace_root_is_rejected() {
    each_store(|store, label| {
        let mut workspace = workspace("relative/path");
        workspace.root = PathBuf::from("relative/path");
        assert!(
            store.register_workspace(workspace).is_err(),
            "{label}: a relative root must be rejected"
        );
    });
}

#[test]
fn only_one_non_terminal_mission_per_workspace() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let first = mission(&workspace);
        store.create_mission(first.clone()).unwrap();

        let second = mission(&workspace);
        let error = store
            .create_mission(second.clone())
            .expect_err("D16: one active mission per workspace");
        match error {
            StoreError::MissionAlreadyActive { existing, .. } => {
                assert_eq!(existing, first.id, "{label}")
            }
            other => panic!("{label}: unexpected error {other:?}"),
        }

        // Once the first reaches a terminal state, a new mission is permitted.
        store
            .transition_mission(&first.id, MissionState::Denied)
            .unwrap();
        store.create_mission(second).unwrap();
    });
}

#[test]
fn a_mission_for_an_unregistered_workspace_is_rejected() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        assert!(
            store.create_mission(mission(&workspace)).is_err(),
            "{label}"
        );
    });
}

#[test]
fn mission_transitions_are_checked_by_the_state_machine() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mission = mission(&workspace);
        store.create_mission(mission.clone()).unwrap();

        // Skipping approval must not be possible, even through the store.
        assert!(
            store
                .transition_mission(&mission.id, MissionState::Active)
                .is_err(),
            "{label}: proposed -> active must be rejected"
        );
        store
            .transition_mission(&mission.id, MissionState::AwaitingApproval)
            .unwrap();
        let active = store
            .transition_mission(&mission.id, MissionState::Active)
            .unwrap();
        assert_eq!(active.state, MissionState::Active);
    });
}

#[test]
fn closeout_revokes_leases_sessions_and_freezes_broker_work_together() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();

        let parent = lease(&mission, None);
        store.insert_lease(parent.clone()).unwrap();
        let child = lease(&mission, Some(&parent));
        store.insert_lease(child.clone()).unwrap();

        let token = SessionToken::generate().unwrap();
        store.bind_session(session(&parent, &token)).unwrap();
        let child_token = SessionToken::generate().unwrap();
        store.bind_session(session(&child, &child_token)).unwrap();

        let approval = approval_request(&mission, &parent, Digest::of_bytes(b"push"));
        store.insert_approval_request(approval.clone()).unwrap();
        let op = BrokeredOperation {
            id: ids::new::broker_op_id().unwrap(),
            mission: mission.id.clone(),
            lease: parent.id.clone(),
            approval: approval.id.clone(),
            kind: BrokeredKind::GitPush {
                remote: "origin".to_owned(),
                refspec: "refs/heads/feature".to_owned(),
                commit: "a".repeat(40),
            },
            state: BrokerOpState::Requested,
            result_summary: None,
            requested_at: Utc::now(),
            finished_at: None,
        };
        store.insert_broker_op(op.clone()).unwrap();

        let now = Utc::now();
        let closed = store
            .close_mission(MissionCloseout {
                mission: mission.id.clone(),
                final_state: MissionState::Completed,
                closed_at: now,
                closing_diff: None,
                summary: "done".to_owned(),
            })
            .unwrap();

        assert_eq!(closed.state, MissionState::Completed, "{label}");
        assert_eq!(closed.closed_at, Some(now));
        for id in [&parent.id, &child.id] {
            assert_eq!(
                store.get_lease(id).unwrap().state,
                LeaseState::Revoked,
                "{label}: every lease must be revoked"
            );
        }
        assert!(
            store.resolve_token(&token.hash(), now).unwrap().is_none(),
            "{label}: sessions must be revoked with the mission"
        );
        assert_eq!(
            store.get_broker_op(&op.id).unwrap().state,
            BrokerOpState::Frozen,
            "{label}: in-flight brokered work freezes rather than cancelling silently"
        );
    });
}

#[test]
fn revoking_a_lease_fans_out_to_derived_leases_and_sessions() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let parent = lease(&mission, None);
        store.insert_lease(parent.clone()).unwrap();
        let child = lease(&mission, Some(&parent));
        store.insert_lease(child.clone()).unwrap();
        let child_token = SessionToken::generate().unwrap();
        store.bind_session(session(&child, &child_token)).unwrap();

        let now = Utc::now();
        let revoked = store.revoke_lease_tree(&parent.id, now).unwrap();
        assert!(revoked.contains(&parent.id), "{label}");
        assert!(revoked.contains(&child.id), "{label}: sub-agent fan-out");
        assert_eq!(
            store.get_lease(&child.id).unwrap().state,
            LeaseState::Revoked
        );
        assert!(
            store
                .resolve_token(&child_token.hash(), now)
                .unwrap()
                .is_none(),
            "{label}: a sub-agent's token stops working immediately"
        );
    });
}

#[test]
fn budget_charging_is_atomic_and_exhausts_the_lease() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let mut lease = lease(&mission, None);
        lease.budget = budget(2);
        store.insert_lease(lease.clone()).unwrap();

        let first = store
            .charge_lease(&lease.id, &BudgetCost::one_task_run())
            .unwrap();
        assert_eq!(first.task_runs, 1, "{label}");
        let second = store
            .charge_lease(&lease.id, &BudgetCost::one_task_run())
            .unwrap();
        assert_eq!(second.task_runs, 2);
        // The second charge spent the last run, so the lease is exhausted and
        // blocks new work without terminating.
        assert_eq!(
            store.get_lease(&lease.id).unwrap().state,
            LeaseState::Exhausted,
            "{label}"
        );
        assert!(
            store
                .charge_lease(&lease.id, &BudgetCost::one_task_run())
                .is_err(),
            "{label}: charging past the ceiling must fail"
        );
        assert_eq!(
            store.get_lease(&lease.id).unwrap().usage.task_runs,
            2,
            "{label}: a refused charge must not be recorded"
        );
    });
}

#[test]
fn renewal_supersedes_rather_than_mutating_expiry() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let original = lease(&mission, None);
        store.insert_lease(original.clone()).unwrap();

        let mut replacement = lease(&mission, None);
        replacement.expires_at = original.expires_at + chrono::Duration::hours(1);
        let renewed = store
            .renew_lease(LeaseRenewal {
                superseded: original.id.clone(),
                replacement: replacement.clone(),
            })
            .unwrap();

        assert_eq!(renewed.id, replacement.id, "{label}");
        assert_eq!(
            store.get_lease(&original.id).unwrap().state,
            LeaseState::Superseded,
            "{label}: the old lease is superseded, not mutated"
        );
        assert_eq!(
            store.get_lease(&original.id).unwrap().expires_at,
            original.expires_at
        );
    });
}

#[test]
fn token_resolution_treats_unknown_expired_and_revoked_identically() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let token = SessionToken::generate().unwrap();
        store.bind_session(session(&lease, &token)).unwrap();

        let now = lease.issued_at;
        let resolved = store.resolve_token(&token.hash(), now).unwrap();
        let resolved = resolved.expect("a valid token resolves");
        assert_eq!(resolved.lease.id, lease.id, "{label}");
        assert_eq!(resolved.mission.id, mission.id);

        // Unknown.
        let other = SessionToken::generate().unwrap();
        assert!(store.resolve_token(&other.hash(), now).unwrap().is_none());
        // Expired.
        assert!(
            store
                .resolve_token(
                    &token.hash(),
                    lease.expires_at + chrono::Duration::seconds(1)
                )
                .unwrap()
                .is_none()
        );
        // Revoked.
        store.revoke_sessions_for_lease(&lease.id, now).unwrap();
        assert!(store.resolve_token(&token.hash(), now).unwrap().is_none());
    });
}

#[test]
fn a_session_may_not_outlive_its_lease() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let token = SessionToken::generate().unwrap();
        let mut session = session(&lease, &token);
        session.expires_at = lease.expires_at + chrono::Duration::hours(1);
        assert!(
            store.bind_session(session).is_err(),
            "{label}: token expiry is derived from lease expiry"
        );
    });
}

#[test]
fn task_runs_transition_and_complete() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let run = task_run(&lease);
        store.insert_task_run(run.clone()).unwrap();

        store
            .transition_task_run(&run.id, TaskRunState::Admitted)
            .unwrap();
        store
            .transition_task_run(&run.id, TaskRunState::Preparing)
            .unwrap();
        store
            .transition_task_run(&run.id, TaskRunState::Running)
            .unwrap();
        let finished = store
            .complete_task_run(
                &run.id,
                TaskRunState::Succeeded,
                TaskOutcome::new(Some(0), TaskFailureClass::Success, "ok"),
                Utc::now(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(finished.state, TaskRunState::Succeeded, "{label}");
        assert!(finished.finished_at.is_some());
        assert_eq!(
            store.list_task_runs(&mission.id).unwrap().len(),
            1,
            "{label}"
        );
        assert_eq!(
            store.passing_task_evidence(&mission.id).unwrap(),
            vec![(TaskType::RustCheck, run.id.clone())],
            "{label}"
        );
        // A terminal run cannot be reopened.
        assert!(
            store
                .transition_task_run(&run.id, TaskRunState::Running)
                .is_err()
        );
    });
}

#[test]
fn approve_once_is_single_use_and_a_mismatched_digest_never_authorises() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();

        let digest = Digest::of_bytes(b"push origin feature abc");
        let request = approval_request(&mission, &lease, digest.clone());
        store.insert_approval_request(request.clone()).unwrap();
        assert_eq!(
            store.list_pending_approvals(Utc::now()).unwrap().len(),
            1,
            "{label}"
        );

        store
            .record_approval_decision(decision(&request, Decision::ApproveOnce))
            .unwrap();
        assert!(
            store.list_pending_approvals(Utc::now()).unwrap().is_empty(),
            "{label}: a decided approval is no longer pending"
        );

        let found = store
            .find_authorising_approval(&mission.id, &digest, Utc::now())
            .unwrap();
        assert!(found.is_some(), "{label}");
        let tampered = Digest::of_bytes(b"push origin main abc");
        assert!(
            store
                .find_authorising_approval(&mission.id, &tampered, Utc::now())
                .unwrap()
                .is_none(),
            "{label}: an altered request must not match"
        );

        store.consume_approval(&request.id, Utc::now()).unwrap();
        assert!(
            matches!(
                store.consume_approval(&request.id, Utc::now()),
                Err(StoreError::ApprovalAlreadyConsumed(_))
            ),
            "{label}: ApproveOnce is single-use"
        );
        assert!(
            store
                .find_authorising_approval(&mission.id, &digest, Utc::now())
                .unwrap()
                .is_none(),
            "{label}: a consumed approval no longer authorises"
        );
    });
}

#[test]
fn a_non_human_decision_is_refused_by_the_store() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let request = approval_request(&mission, &lease, Digest::of_bytes(b"x"));
        store.insert_approval_request(request.clone()).unwrap();
        let mut decision = decision(&request, Decision::ApproveOnce);
        decision.decided_by = ActorId::parse("agent:claude").unwrap();
        assert!(
            store.record_approval_decision(decision).is_err(),
            "{label}: an agent must not be able to approve anything"
        );
    });
}

#[test]
fn mission_scoped_approvals_are_not_consumed() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let digest = Digest::of_bytes(b"x");
        let request = approval_request(&mission, &lease, digest.clone());
        store.insert_approval_request(request.clone()).unwrap();
        store
            .record_approval_decision(decision(&request, Decision::ApproveForMission))
            .unwrap();
        store.consume_approval(&request.id, Utc::now()).unwrap();
        store.consume_approval(&request.id, Utc::now()).unwrap();
        assert!(
            store
                .find_authorising_approval(&mission.id, &digest, Utc::now())
                .unwrap()
                .is_some(),
            "{label}: a mission-scoped approval stays usable"
        );
    });
}

#[test]
fn baselines_are_keyed_by_workspace_task_and_target() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let baseline = AccessBaseline {
            workspace: workspace.id.clone(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths: vec![BaselinePathEntry::SubtreeGrant {
                path: path("crates/core"),
                source: GrantSource::MissionEditScope,
            }],
            inventory: CodeExecInventory::default(),
            origin: BaselineOrigin::StaticClosure,
            confirmed_by: ActorId::parse("human:andrew").unwrap(),
            confirmed_at: Utc::now(),
        };
        store.put_baseline(baseline.clone()).unwrap();
        let key = baseline.key();
        assert_eq!(
            store.get_baseline(&key).unwrap(),
            Some(baseline.clone()),
            "{label}"
        );

        // A different task type is a different baseline.
        let other_key = BaselineKey {
            workspace: workspace.id.clone(),
            task: TaskType::RustTestUnit,
            target: path("crates/core"),
        };
        assert!(store.get_baseline(&other_key).unwrap().is_none(), "{label}");
        assert_eq!(store.list_baselines(&workspace.id).unwrap().len(), 1);
        assert!(store.delete_baseline(&key).unwrap());
        assert!(!store.delete_baseline(&key).unwrap());
    });
}

#[test]
fn an_unconfirmed_baseline_cannot_be_stored_as_confirmed() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let baseline = AccessBaseline {
            workspace: workspace.id.clone(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths: vec![BaselinePathEntry::SubtreeGrant {
                path: path("crates/core"),
                source: GrantSource::MissionEditScope,
            }],
            inventory: CodeExecInventory::default(),
            origin: BaselineOrigin::Learned,
            confirmed_by: ActorId::parse("agent:claude").unwrap(),
            confirmed_at: Utc::now(),
        };
        assert!(
            store.put_baseline(baseline).is_err(),
            "{label}: only a human can confirm a baseline"
        );
    });
}

#[test]
fn the_audit_log_is_append_only_and_chained() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        for index in 0..5 {
            store
                .append_audit(
                    AuditEventDraft::new(AuditEventKind::MissionProposed)
                        .workspace(workspace.id.clone())
                        .payload(serde_json::json!({"n": index})),
                )
                .unwrap();
        }
        let events = store.list_audit(&AuditFilter::default()).unwrap();
        assert_eq!(events.len(), 5, "{label}");
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[4].seq, 5);
        assert_eq!(
            store.audit_head().unwrap().map(|head| head.seq),
            Some(5),
            "{label}"
        );
        store.verify_audit().unwrap();
    });
}

#[test]
fn audit_filters_select_by_mission_and_kind() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mission = mission(&workspace);
        store.create_mission(mission.clone()).unwrap();
        store
            .append_audit(
                AuditEventDraft::new(AuditEventKind::MissionProposed).mission(mission.id.clone()),
            )
            .unwrap();
        store
            .append_audit(AuditEventDraft::new(AuditEventKind::DaemonStarted))
            .unwrap();
        let mission_events = store
            .list_audit(&AuditFilter::for_mission(mission.id.clone()))
            .unwrap();
        assert_eq!(mission_events.len(), 1, "{label}");
        let by_kind = store
            .list_audit(&AuditFilter {
                kinds: vec!["daemon.started"],
                ..AuditFilter::default()
            })
            .unwrap();
        assert_eq!(by_kind.len(), 1, "{label}");
    });
}

#[test]
fn truncating_the_audit_log_is_detected() {
    // The head is recorded separately, which is what makes tail truncation
    // visible; without it the remaining prefix verifies on its own.
    let store = MemoryStore::new();
    let workspace = workspace("/srv/project");
    store.register_workspace(workspace.clone()).unwrap();
    for _ in 0..4 {
        store
            .append_audit(AuditEventDraft::new(AuditEventKind::DaemonStarted))
            .unwrap();
    }
    store.verify_audit().unwrap();
    store.truncate_audit_for_test(2).unwrap();
    let error = store
        .verify_audit()
        .expect_err("truncation must be detected");
    match error {
        StoreError::AuditChain(ChainViolation::TruncatedTail { expected, found }) => {
            assert_eq!((expected, found), (4, 2));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn sqlite_truncation_is_detected_too() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("db.sqlite");
    let store = SqliteStore::open(&db).expect("open");
    for _ in 0..4 {
        store
            .append_audit(AuditEventDraft::new(AuditEventKind::DaemonStarted))
            .unwrap();
    }
    store.verify_audit().unwrap();
    store.delete_audit_rows_for_test(2).unwrap();
    assert!(
        store.verify_audit().is_err(),
        "removing rows must be detectable even with direct database access"
    );
}

#[test]
fn artifacts_and_egress_attempts_are_listed_per_mission() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        let mut mission = mission(&workspace);
        mission.state = MissionState::Active;
        store.create_mission(mission.clone()).unwrap();
        let lease = lease(&mission, None);
        store.insert_lease(lease.clone()).unwrap();
        let run = task_run(&lease);
        store.insert_task_run(run.clone()).unwrap();

        let digest = Digest::of_bytes(b"log");
        let artifact = Artifact {
            id: Artifact::id_for(&digest).unwrap(),
            kind: ArtifactKind::Log,
            produced_by: Some(run.id.clone()),
            mission: mission.id.clone(),
            trust_class: TrustClass::T2,
            size_bytes: 3,
            blake3: digest,
            content_ref: PathBuf::from("/var/lib/clyde/blobs/log"),
            created_at: Utc::now(),
            retain_until: None,
        };
        store.insert_artifact(artifact.clone()).unwrap();
        assert_eq!(
            store.list_artifacts(&mission.id).unwrap().len(),
            1,
            "{label}"
        );
        assert_eq!(store.get_artifact(&artifact.id).unwrap(), artifact);

        let attempt = clyde_core::egress::EgressAttempt {
            task_run: Some(run.id.clone()),
            at: Utc::now(),
            profile: "none".to_owned(),
            host: clyde_core::classification::HostName::parse("evil.test").unwrap(),
            port: 443,
            decision: clyde_core::egress::EgressDecision::Denied,
            denial_reason: Some(clyde_core::egress::EgressDenialReason::ProfileForbidsEgress),
            bytes_in: 0,
            bytes_out: 0,
            request_path: None,
            status: None,
            duration_ms: 1,
        };
        store.insert_egress_attempt(attempt.clone()).unwrap();
        assert_eq!(
            store.list_egress_attempts(&run.id).unwrap().len(),
            1,
            "{label}"
        );
        assert_eq!(
            store
                .list_mission_egress_attempts(&mission.id)
                .unwrap()
                .len(),
            1,
            "{label}"
        );
    });
}

#[test]
fn bundle_inventory_confirmation_requires_a_human() {
    each_store(|store, label| {
        let digest = Digest::of_bytes(b"bundle");
        let artifact = Artifact::id_for(&digest).unwrap();
        store
            .record_bundle(crate::BundleRecord {
                artifact: artifact.clone(),
                lockfile_digest: Digest::of_bytes(b"lock"),
                lockfile: Default::default(),
                content_ref: PathBuf::from("/var/lib/clyde/deps/x"),
                crate_count: 12,
                inventory: CodeExecInventory::default(),
                created_at: Utc::now(),
                registries: vec!["index.crates.io".to_owned()],
            })
            .unwrap();
        assert!(
            store
                .find_bundle_for_lockfile(&Digest::of_bytes(b"lock"))
                .unwrap()
                .is_some(),
            "{label}"
        );
        assert!(!store.is_bundle_inventory_confirmed(&artifact).unwrap());
        assert!(
            store
                .set_bundle_inventory_confirmed(
                    &artifact,
                    ActorId::parse("agent:claude").unwrap(),
                    Utc::now()
                )
                .is_err(),
            "{label}: an agent cannot confirm an inventory change"
        );
        store
            .set_bundle_inventory_confirmed(
                &artifact,
                ActorId::parse("human:andrew").unwrap(),
                Utc::now(),
            )
            .unwrap();
        assert!(
            store.is_bundle_inventory_confirmed(&artifact).unwrap(),
            "{label}"
        );
    });
}

#[test]
fn config_loads_are_recorded_with_their_digest() {
    each_store(|store, label| {
        let workspace = workspace("/srv/project");
        store.register_workspace(workspace.clone()).unwrap();
        store
            .record_config_load(crate::ConfigLoad {
                workspace: Some(workspace.id.clone()),
                source: "repository".to_owned(),
                digest: Digest::of_bytes(b"policy.toml"),
                rejected_keys: vec!["agent.command".to_owned()],
                loaded_at: Utc::now(),
            })
            .unwrap();
        let loads = store.list_config_loads(Some(&workspace.id)).unwrap();
        assert_eq!(loads.len(), 1, "{label}");
        assert_eq!(loads[0].rejected_keys, vec!["agent.command".to_owned()]);
    });
}

#[test]
fn a_sqlite_store_reopens_with_its_data() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("db.sqlite");
    let workspace = workspace("/srv/project");
    let mission_id: clyde_core::MissionId;
    {
        let store = SqliteStore::open(&db).expect("open");
        store.register_workspace(workspace.clone()).unwrap();
        let mission = mission(&workspace);
        mission_id = mission.id.clone();
        store.create_mission(mission).unwrap();
        store
            .append_audit(AuditEventDraft::new(AuditEventKind::DaemonStarted))
            .unwrap();
    }
    let store = SqliteStore::open(&db).expect("reopen");
    assert_eq!(store.get_mission(&mission_id).unwrap().id, mission_id);
    assert_eq!(store.audit_head().unwrap().map(|head| head.seq), Some(1));
    store.verify_audit().unwrap();

    // The chain continues from the persisted head rather than restarting.
    let next = store
        .append_audit(AuditEventDraft::new(AuditEventKind::DaemonStopped))
        .unwrap();
    assert_eq!(next.seq, 2);
    store.verify_audit().unwrap();
}

#[test]
fn migrations_are_idempotent_across_opens() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("db.sqlite");
    for _ in 0..3 {
        let store = SqliteStore::open(&db).expect("open");
        assert!(store.schema_version().unwrap() > 0);
    }
}
