//! The end-to-end delegation flow: propose, approve, lease, admit, execute,
//! classify, review.
//!
//! This runs on the no-isolation test backend, because this host cannot create
//! unprivileged user namespaces. What it asserts is the *pipeline* — admission,
//! snapshot, execution, classification, artifacts, audit — never the isolation
//! boundary, which is asserted structurally over sandbox specifications in
//! `security.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use clyde_core::repo_path::RepoPath;
use clyde_core::task::{TaskFailureClass, TaskOptions, TaskRunState, TaskType};

fn path(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

#[tokio::test]
async fn a_mission_runs_check_end_to_end_and_is_reviewable() {
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
    let context = harness.context(&workspace, &mission, &lease);

    let run = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustCheck,
        path("crates/core"),
        TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
    )
    .await
    .expect("the task runs");

    assert_eq!(
        run.state,
        TaskRunState::Succeeded,
        "outcome: {:?}",
        run.outcome
    );
    let outcome = run.outcome.expect("an outcome");
    assert_eq!(outcome.classification, TaskFailureClass::Success);
    assert!(run.snapshot.is_some(), "a build runs against a snapshot");
    assert!(
        !run.policy_digest.as_str().is_empty(),
        "the policy that actually applied is recorded on the run"
    );

    // Budget was charged at admission, before execution.
    let charged = harness.daemon.store.get_lease(&lease.id).unwrap();
    assert_eq!(charged.usage.task_runs, 1);

    // The audit trail reconstructs the whole flow.
    for kind in [
        "mission.proposed",
        "mission.approved",
        "lease.issued",
        "mission.activated",
        "task.requested",
        "task.admitted",
        "snapshot.created",
        "task.started",
        "task.finished",
    ] {
        assert!(harness.recorded(&mission.id, kind), "missing {kind}");
    }
    // Baseline events are workspace-scoped rather than mission-scoped: a
    // baseline outlives any one mission, and keying it to a mission would make
    // it look like a per-mission decision.
    let workspace_events = harness
        .daemon
        .store
        .list_audit(&clyde_store::AuditFilter {
            workspace: Some(workspace.id.clone()),
            ..Default::default()
        })
        .unwrap();
    for kind in ["baseline.proposed", "baseline.confirmed"] {
        assert!(
            workspace_events
                .iter()
                .any(|event| event.kind.name() == kind),
            "missing {kind}"
        );
    }
    harness.daemon.store.verify_audit().unwrap();

    let review = clyded::review::build(&harness.daemon, &mission.id)
        .await
        .expect("a review");
    assert!(review.audit_intact);
    assert_eq!(review.tasks.len(), 1);
    assert_eq!(review.tasks[0].task, "rust.check");
    assert!(review.budget_consumed.contains("1 of"));
}

#[tokio::test]
async fn a_second_run_reuses_the_warm_mission_cache() {
    // One cold build per mission is accepted, and the cache's warmth is
    // observable so the cold run can be explained rather than guessed at.
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
    let context = harness.context(&workspace, &mission, &lease);

    let cache =
        clyde_snapshot::MissionCache::existing(&harness.daemon.paths.missions(), &mission.id)
            .expect("the cache is created at activation");
    assert!(!cache.is_warm(), "the first build in a mission is cold");

    for _ in 0..2 {
        let run = clyded::tasks::run_task(
            &harness.daemon,
            &context,
            TaskType::RustCheck,
            path("crates/core"),
            TaskOptions::RustCheck {
                package: None,
                all_targets: false,
            },
        )
        .await
        .expect("the task runs");
        assert_eq!(run.state, TaskRunState::Succeeded, "{:?}", run.outcome);
    }
    assert!(cache.is_warm(), "the second run reuses a warm cache");
}

#[tokio::test]
async fn a_task_with_no_confirmed_baseline_is_refused_and_the_proposal_is_offered() {
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let context = harness.context(&workspace, &mission, &lease);

    let error = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustCheck,
        path("crates/core"),
        TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
    )
    .await
    .expect_err("no baseline means no run");
    assert!(
        error.to_string().contains("no confirmed access baseline"),
        "{error}"
    );

    // The denial computed the proposal on the way out, so the operator's next
    // step is one command rather than a search.
    let proposal = harness
        .daemon
        .store
        .get_baseline_proposal(&clyde_core::baseline::BaselineKey {
            workspace: workspace.id.clone(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
        })
        .unwrap();
    assert!(proposal.is_some(), "the static proposal is offered");
    assert!(harness.recorded(&mission.id, "task.denied"));
}

#[tokio::test]
async fn a_task_outside_the_mission_envelope_is_denied_with_a_next_step() {
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let context = harness.context(&workspace, &mission, &lease);

    let error = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustTestUnit,
        path("crates/core"),
        TaskOptions::RustTestUnit {
            package: None,
            filter: None,
        },
    )
    .await
    .expect_err("rust.test.unit is not in this envelope");
    let reasons = error.reasons();
    assert!(!reasons.is_empty());
    assert!(
        reasons
            .iter()
            .any(|reason| reason.suggested_alternative().is_some()),
        "a denial must offer a next step"
    );
}

#[tokio::test]
async fn revoking_a_mission_stops_further_work_immediately() {
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
    let context = harness.context(&workspace, &mission, &lease);

    // A session bound to the lease works before revocation.
    let token = clyded::missions::bind_session(&harness.daemon, &lease).unwrap();
    assert!(
        harness
            .daemon
            .store
            .resolve_token(&token.hash(), chrono::Utc::now())
            .unwrap()
            .is_some()
    );

    clyded::missions::revoke(&harness.daemon, &mission.id, &harness.operator).unwrap();

    assert!(
        harness
            .daemon
            .store
            .resolve_token(&token.hash(), chrono::Utc::now())
            .unwrap()
            .is_none(),
        "revocation must stop the actor immediately, not at the next connection"
    );
    let error = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustCheck,
        path("crates/core"),
        TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
    )
    .await
    .expect_err("a revoked mission admits no work");
    assert!(error.to_string().contains("not active"), "{error}");
}

#[tokio::test]
async fn a_sub_agent_lease_is_narrower_and_cannot_nest() {
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);

    let session = clyde_store::ResolvedSession {
        session: clyde_core::session::ActorSession {
            actor: lease.actor.clone(),
            lease: lease.id.clone(),
            token_hash: clyde_core::session::SessionToken::generate()
                .unwrap()
                .hash(),
            issued_at: lease.issued_at,
            expires_at: lease.expires_at,
            revoked_at: None,
            sandbox: None,
        },
        lease: lease.clone(),
        mission: mission.clone(),
    };

    let result = clyded::subagents::request(
        &harness.daemon,
        &session,
        &serde_json::json!({
            "purpose": "narrow work",
            "edit_paths": ["crates/core/src"],
        }),
    )
    .await
    .expect("a narrower sub-agent derives");
    assert!(!result.is_error);

    let leases = harness.daemon.store.list_leases(&mission.id).unwrap();
    let derived = leases
        .iter()
        .find(|candidate| candidate.parent.is_some())
        .expect("a derived lease");
    assert_eq!(
        derived
            .repo_scope
            .edit_paths
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["crates/core/src".to_owned()]
    );
    assert!(
        !derived.authority.may_spawn_subagents,
        "one level of derivation only"
    );
    assert!(!derived.authority.may_request_publish);

    // A request outside the parent's scope is a structured denial.
    let error = clyded::subagents::request(
        &harness.daemon,
        &session,
        &serde_json::json!({
            "purpose": "wider work",
            "edit_paths": ["crates"],
        }),
    )
    .await
    .expect_err("a sub-agent cannot widen");
    assert!(!error.reasons().is_empty());
}
