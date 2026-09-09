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

#[tokio::test]
async fn an_operator_runs_the_same_task_as_an_actor_and_the_record_says_who() {
    // D25's central claim: the surfaces differ in who authenticates, and the
    // admission path is one path. This runs the *same* request both ways and
    // compares what admission decided.
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    let options = || TaskOptions::RustCheck {
        package: None,
        all_targets: false,
    };

    let by_operator = clyded::tasks::run_task(
        &harness.daemon,
        &harness.operator_context(&workspace, &mission, &lease),
        TaskType::RustCheck,
        path("crates/core"),
        options(),
    )
    .await
    .expect("the operator runs the task");

    let by_actor = clyded::tasks::run_task(
        &harness.daemon,
        &harness.context(&workspace, &mission, &lease),
        TaskType::RustCheck,
        path("crates/core"),
        options(),
    )
    .await
    .expect("the actor runs the same task");

    // Same policy, same outcome. If these ever diverge, one surface has become
    // privileged relative to the other, which is the thing D25 forbids.
    assert_eq!(
        by_operator.policy_digest, by_actor.policy_digest,
        "the same request must resolve to the same policy on both surfaces"
    );
    assert_eq!(by_operator.state, by_actor.state);
    assert_eq!(
        by_operator.request.digest().expect("a digest"),
        by_actor.request.digest().expect("a digest"),
        "the principal must not enter the request digest, or an approval would \
         stop being usable across the two surfaces"
    );

    // But the record distinguishes them.
    assert!(
        by_operator.request.principal.is_operator(),
        "an admin-socket request is recorded as an operator: {:?}",
        by_operator.request.principal
    );
    assert!(
        !by_actor.request.principal.is_operator(),
        "a token-bearing request is recorded as a session: {:?}",
        by_actor.request.principal
    );
    // The lease's actor is the same for both; the principal is what tells them
    // apart, which is precisely why the field had to exist.
    assert_eq!(by_operator.request.actor, by_actor.request.actor);
}

#[tokio::test]
async fn an_operator_task_is_bound_by_the_lease_like_any_other() {
    // The operator surface is not a way around the envelope. A task outside the
    // mission's allowed set is denied for a human exactly as it is for an agent.
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    let denied = clyded::tasks::run_task(
        &harness.daemon,
        &harness.operator_context(&workspace, &mission, &lease),
        TaskType::RustTestUnit,
        path("crates/core"),
        TaskOptions::RustTestUnit {
            package: None,
            filter: None,
        },
    )
    .await;

    assert!(
        denied.is_err(),
        "rust.test.unit is outside this mission, and being the operator does not change that"
    );
}

#[tokio::test]
async fn an_operator_run_charges_the_same_budget() {
    // Budget is charged at admission regardless of surface, so an operator
    // cannot drive an unmetered loop alongside a metered one.
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    clyded::tasks::run_task(
        &harness.daemon,
        &harness.operator_context(&workspace, &mission, &lease),
        TaskType::RustCheck,
        path("crates/core"),
        TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
    )
    .await
    .expect("the operator runs the task");

    let charged = harness
        .daemon
        .store
        .get_lease(&lease.id)
        .expect("the lease is still there");
    assert_eq!(
        charged.usage.task_runs, 1,
        "an operator-driven run consumes the lease's budget like any other"
    );
}

#[tokio::test]
async fn a_task_run_records_the_posture_it_ran_under() {
    // Recorded rather than reconstructed later, because posture is a property of
    // the moment: a deployment that gains the warden halfway through a mission
    // must not make the earlier work look as though it were enforced (D26).
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    let run = clyded::tasks::run_task(
        &harness.daemon,
        &harness.operator_context(&workspace, &mission, &lease),
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
        run.posture, harness.daemon.posture,
        "the run carries the posture the daemon derived, not a default"
    );
    // The test harness hosts no agent, so this deployment cannot be enforcing —
    // whatever else is true of the machine the suite is running on.
    assert!(
        !run.posture.is_enforcing(),
        "a builder-only deployment is advisory: {}",
        run.posture
    );
    assert!(
        run.posture
            .reasons()
            .contains(&clyde_core::posture::BypassReason::NoHostedActor),
        "and it must name that specific bypass: {}",
        run.posture
    );

    // Mission review states it too, so review does not have to assume today's
    // posture applied to yesterday's work.
    let review = clyded::review::build(&harness.daemon, &mission.id)
        .await
        .expect("a review");
    assert!(
        review.postures.contains(&"advisory".to_owned()),
        "review states the posture the work happened under: {:?}",
        review.postures
    );
}

#[tokio::test]
async fn posture_cannot_change_an_admission_decision() {
    // D26's hard constraint, and the same one R10 places on enclosure detection:
    // observing a bypass may change what is *reported* and never what is
    // *permitted*. A posture that could widen admission would be a security
    // control that weakens the system by noticing something.
    let harness = support::Harness::with_toolchain();
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    let options = || TaskOptions::RustCheck {
        package: None,
        all_targets: false,
    };
    let context = harness.operator_context(&workspace, &mission, &lease);

    let first = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustCheck,
        path("crates/core"),
        options(),
    )
    .await
    .expect("the task runs");

    // A task the mission does not allow stays denied, and one it does allow
    // stays admitted, under the same posture. Posture is an output of the run,
    // never an input to it.
    let denied = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustTestUnit,
        path("crates/core"),
        TaskOptions::RustTestUnit {
            package: None,
            filter: None,
        },
    )
    .await;
    assert!(denied.is_err(), "the envelope decides, not the posture");

    let second = clyded::tasks::run_task(
        &harness.daemon,
        &context,
        TaskType::RustCheck,
        path("crates/core"),
        options(),
    )
    .await
    .expect("the task runs again");

    assert_eq!(
        first.policy_digest, second.policy_digest,
        "the resolved policy is identical across runs under one posture"
    );
    assert_eq!(first.posture, second.posture);
}

/// Renewal, which had no coverage at all until three bugs in it were found by
/// hand: an expired lease renewed into a lease that was already expired, the
/// mission's own expiry never moved, and a revoked mission could be renewed back
/// into an active one.
mod renewal {
    use chrono::{Duration, Utc};
    use clyde_core::lease::LeaseState;
    use clyde_core::mission::MissionState;
    use clyde_core::task::TaskType;

    use super::support;

    /// Moves a mission and its lease into the past, as an untouched mission
    /// becomes after a weekend.
    fn expire(harness: &support::Harness, mission: &clyde_core::mission::Mission) {
        let past = Utc::now() - Duration::days(2);
        harness
            .daemon
            .store
            .set_mission_expiry(&mission.id, past)
            .unwrap();
        // A lease is immutable once stored, so the expired one is installed the
        // way the daemon would install any replacement: superseding the live
        // lease. Issued before it expired, because a lease that expires before
        // it was issued is not a state the daemon can reach.
        let leases = harness.daemon.store.list_leases(&mission.id).unwrap();
        for lease in leases.into_iter().filter(|lease| lease.parent.is_none()) {
            let expired = clyde_core::lease::Lease {
                id: clyde_core::ids::new::lease_id().unwrap(),
                issued_at: past - Duration::days(1),
                expires_at: past,
                purpose: "expired for the test".to_owned(),
                ..lease.clone()
            };
            harness
                .daemon
                .store
                .renew_lease(clyde_store::LeaseRenewal {
                    superseded: lease.id.clone(),
                    replacement: expired,
                })
                .unwrap();
        }
    }

    #[test]
    fn renewing_an_expired_lease_produces_one_that_is_valid_from_now() {
        let harness = support::Harness::new();
        let workspace = harness.register("single-crate");
        let (mission, _lease) = harness.mission(&workspace.id, &["src"], &[TaskType::RustCheck]);
        expire(&harness, &mission);

        let renewed = clyded::missions::renew(&harness.daemon, &mission.id, "1h", None)
            .expect("an expired lease is exactly what renewal is for");

        assert!(
            renewed.expires_at > Utc::now(),
            "a renewal that lands in the past is a lease born expired: {}",
            renewed.expires_at
        );
        assert_eq!(renewed.state, LeaseState::Active);
    }

    #[test]
    fn the_missions_own_expiry_moves_with_the_lease() {
        let harness = support::Harness::new();
        let workspace = harness.register("single-crate");
        let (mission, _lease) = harness.mission(&workspace.id, &["src"], &[TaskType::RustCheck]);
        expire(&harness, &mission);

        let renewed = clyded::missions::renew(&harness.daemon, &mission.id, "2h", None).unwrap();

        let stored = harness.daemon.store.get_mission(&mission.id).unwrap();
        assert_eq!(
            stored.expiry, renewed.expires_at,
            "a mission that expires before its lease refuses work the lease permits"
        );
    }

    #[test]
    fn renewing_an_unexpired_lease_extends_from_its_expiry_not_from_now() {
        let harness = support::Harness::new();
        let workspace = harness.register("single-crate");
        let (mission, lease) = harness.mission(&workspace.id, &["src"], &[TaskType::RustCheck]);

        let renewed = clyded::missions::renew(&harness.daemon, &mission.id, "1h", None).unwrap();

        assert!(
            renewed.expires_at > lease.expires_at,
            "an unexpired lease must not lose its remaining time to a renewal"
        );
        assert_eq!(renewed.expires_at, lease.expires_at + Duration::hours(1));
    }

    #[test]
    fn a_revoked_mission_cannot_be_renewed_back_into_an_active_one() {
        let harness = support::Harness::new();
        let workspace = harness.register("single-crate");
        let (mission, _lease) = harness.mission(&workspace.id, &["src"], &[TaskType::RustCheck]);
        clyded::missions::revoke(&harness.daemon, &mission.id, &harness.operator).unwrap();

        let error = clyded::missions::renew(&harness.daemon, &mission.id, "1h", None)
            .expect_err("revocation is final");

        assert!(
            error.to_string().contains("cannot be renewed"),
            "revoking a mission must not be undoable by renewing its lease: {error}"
        );
        let stored = harness.daemon.store.get_mission(&mission.id).unwrap();
        assert_eq!(stored.state, MissionState::Revoked);
    }

    #[test]
    fn usage_carries_over_and_extra_runs_are_added() {
        let harness = support::Harness::new();
        let workspace = harness.register("single-crate");
        let (mission, lease) = harness.mission(&workspace.id, &["src"], &[TaskType::RustCheck]);

        let renewed = clyded::missions::renew(&harness.daemon, &mission.id, "1h", Some(5)).unwrap();

        assert_eq!(renewed.budget.max_task_runs, lease.budget.max_task_runs + 5);
        assert_eq!(
            renewed.usage, lease.usage,
            "a renewal extends a lease; it does not refund what has been spent"
        );
    }
}
