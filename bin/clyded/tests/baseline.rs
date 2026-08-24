//! Access-baseline behaviour against the fixtures.
//!
//! The properties here are the ones that decide whether the control is usable at
//! all: first-party editing must be prompt-free, and a grant must not admit a
//! secret.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::collections::BTreeSet;

use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;
use clyde_snapshot::{ContentStore, SnapshotRequest};

fn path(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

/// Builds a snapshot from a confirmed baseline, as the task pipeline does.
fn snapshot(
    harness: &support::Harness,
    workspace: &clyde_core::workspace::Workspace,
    mission: &clyde_core::mission::Mission,
    baseline: &clyde_core::baseline::AccessBaseline,
) -> clyde_snapshot::BuiltSnapshot {
    let store = ContentStore::open(harness.dir.path().join("content")).unwrap();
    let request = SnapshotRequest::from_baseline(
        workspace.id.clone(),
        mission.id.clone(),
        workspace.root.clone(),
        baseline,
        BTreeSet::new(),
    );
    clyde_snapshot::build(&store, &request).unwrap()
}

#[test]
fn editing_inside_a_grant_produces_no_drift_across_a_full_loop() {
    // The `churn` fixture property, and the one that decides whether the whole
    // control gets clicked through in practice.
    let harness = support::Harness::new();
    let workspace = harness.register("churn");
    let (mission, _) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let baseline =
        harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");

    let first = snapshot(&harness, &workspace, &mission, &baseline);
    assert!(
        first
            .snapshot
            .manifest
            .contains(&path("crates/core/src/original.rs"))
    );

    // A full edit loop: add a module, add a test, rename a file, delete a file.
    let core = workspace.root.join("crates/core");
    std::fs::write(core.join("src/added.rs"), "pub fn added() -> u32 { 2 }\n").unwrap();
    std::fs::create_dir_all(core.join("src/nested")).unwrap();
    std::fs::write(core.join("src/nested/deep.rs"), "pub fn deep() {}\n").unwrap();
    std::fs::write(core.join("tests/added_test.rs"), "#[test] fn t() {}\n").unwrap();
    std::fs::rename(core.join("src/original.rs"), core.join("src/renamed.rs")).unwrap();
    std::fs::remove_file(core.join("tests/it.rs")).unwrap();

    let second = snapshot(&harness, &workspace, &mission, &baseline);

    // Everything new is admitted, with no amendment and no prompt.
    for added in [
        "crates/core/src/added.rs",
        "crates/core/src/nested/deep.rs",
        "crates/core/tests/added_test.rs",
        "crates/core/src/renamed.rs",
    ] {
        assert!(
            second.snapshot.manifest.contains(&path(added)),
            "{added} must be admitted without an amendment"
        );
    }
    assert!(
        !second
            .snapshot
            .manifest
            .contains(&path("crates/core/tests/it.rs"))
    );

    // Zero drift: every read the loop produced is inside the grant.
    let reads: Vec<RepoPath> = second
        .snapshot
        .manifest
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let drift = clyde_policy::access::path_drift(&baseline, &reads);
    assert!(drift.is_empty(), "editing produced drift: {drift:?}");

    // And no drift event was ever recorded.
    assert!(
        !harness.recorded(&mission.id, "access.drift_detected"),
        "first-party editing must be prompt-free by construction"
    );
}

#[test]
fn a_grant_does_not_admit_a_secret_shaped_file() {
    // The `secret-shaped-files` fixture property.
    let harness = support::Harness::new();
    let workspace = harness.register("secret-shaped-files");
    let (mission, _) = harness.mission(&workspace.id, &["backend/auth"], &[TaskType::RustCheck]);
    let baseline =
        harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "backend/auth");

    // The baseline itself admits the path, because grants are subtrees...
    assert!(baseline.admits(&path("backend/auth/.env.local")));

    // ...and materialisation still refuses it, because absolute exclusions are
    // applied before grants. That is the enforcement, and it is what a test must
    // assert.
    let built = snapshot(&harness, &workspace, &mission, &baseline);
    for secret in [
        "backend/auth/.env.local",
        "backend/auth/server.pem",
        "backend/auth/id_rsa",
    ] {
        assert!(
            !built.snapshot.manifest.contains(&path(secret)),
            "{secret} must not be materialised"
        );
        assert!(
            !built.tree.join(secret).exists(),
            "{secret} must not exist in the snapshot tree"
        );
    }
    assert!(
        built
            .snapshot
            .manifest
            .contains(&path("backend/auth/src/lib.rs"))
    );
}

#[test]
fn a_read_static_analysis_cannot_see_is_named_when_it_fails() {
    // The `include-str-outside` fixture property. The static closure does not
    // include `docs/schema.sql`, so it is not materialised; the failure is then
    // rendered as a named path rather than a raw cargo error.
    let harness = support::Harness::new();
    let workspace = harness.register("include-str-outside");
    let (mission, _) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let baseline =
        harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
    assert!(
        !baseline.admits(&path("docs/schema.sql")),
        "static analysis cannot see an include_str! target"
    );

    let built = snapshot(&harness, &workspace, &mission, &baseline);
    assert!(!built.snapshot.manifest.contains(&path("docs/schema.sql")));

    // The classifier turns the resulting ENOENT into a named escalation.
    let excluded = vec![(
        path("docs/schema.sql"),
        "outside the confirmed baseline".to_owned(),
    )];
    let classification = clyde_snapshot::classify(clyde_snapshot::ClassificationInput {
        exit: clyde_snapshot::ExitSummary {
            code: Some(101),
            timed_out: false,
            signal: None,
        },
        stdout: "",
        stderr: "error: couldn't read docs/schema.sql: No such file or directory (os error 2)",
        egress_denied: false,
        excluded: &excluded,
    });
    assert_eq!(
        classification.class,
        clyde_core::task::TaskFailureClass::AccessBaselineDrift
    );
    assert!(classification.summary.contains("docs/schema.sql"));
}

#[test]
fn learn_mode_records_the_read_static_analysis_missed() {
    // Learn mode is what turns that failure into a confirmable pin.
    let harness = support::Harness::new();
    let workspace = harness.register("include-str-outside");
    let (mission, _) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let key = clyde_core::baseline::BaselineKey {
        workspace: workspace.id.clone(),
        task: TaskType::RustCheck,
        target: path("crates/core"),
    };

    clyded::access::record_learn_initiated(&harness.daemon, &key, &harness.operator, &mission.id);
    let proposal = clyded::access::propose_from_learn(
        &harness.daemon,
        &key,
        &mission.scope,
        &[path("docs/schema.sql")],
    )
    .unwrap();

    let confirmed = proposal
        .confirm(harness.operator.clone(), chrono::Utc::now())
        .unwrap();
    assert!(confirmed.admits(&path("docs/schema.sql")));
    assert_eq!(
        confirmed.origin,
        clyde_core::baseline::BaselineOrigin::Learned
    );

    // A learn run is a wide-scope execution of exactly the code being
    // constrained, so it is recorded distinctly and a reviewer can find it.
    assert!(
        harness.recorded(&mission.id, "baseline.learn_mode_initiated"),
        "learn mode must be marked distinctly in the audit log"
    );
}

#[test]
fn a_baseline_confirmed_by_a_non_human_is_refused() {
    let harness = support::Harness::new();
    let workspace = harness.register("churn");
    let (mission, _) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let target = path("crates/core");
    clyded::access::propose(
        &harness.daemon,
        &workspace.id,
        TaskType::RustCheck,
        &target,
        &mission.scope,
    )
    .unwrap();
    let error = clyded::access::confirm(
        &harness.daemon,
        &clyde_core::baseline::BaselineKey {
            workspace: workspace.id.clone(),
            task: TaskType::RustCheck,
            target,
        },
        &clyde_core::ids::ActorId::parse("agent:claude").unwrap(),
    )
    .expect_err("an actor must never confirm its own baseline");
    assert!(error.to_string().contains("only a human"));
}
