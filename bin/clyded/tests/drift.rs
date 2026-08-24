//! Dependency drift, against the fixtures that provide two states with an
//! identical lockfile.
//!
//! Nothing a lockfile diff can see has changed in either fixture. The detection
//! has to come from the code-execution inventory and its content hashes, and it
//! has to happen before anything runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use clyde_core::baseline::AccessDrift;
use clyde_snapshot::BundleStore;

/// Imports one state of a two-state fixture as a bundle.
fn import(harness: &support::Harness, fixture: &str, state: &str) -> clyde_store::BundleRecord {
    let root = support::fixture_root().join(fixture);
    let store = BundleStore::open(harness.dir.path().join(format!("deps-{state}"))).unwrap();
    store
        .import(&root.join(state), &root.join("Cargo.lock"))
        .expect("the bundle imports")
}

#[test]
fn a_dependency_that_gains_a_build_script_is_caught_before_it_runs() {
    let harness = support::Harness::new();
    let before = import(&harness, "dep-gains-buildscript", "before");
    let after = import(&harness, "dep-gains-buildscript", "after");

    assert_eq!(
        before.lockfile_digest, after.lockfile_digest,
        "the lockfile is identical: a lockfile diff sees nothing"
    );
    assert!(
        before.inventory.entries.is_empty(),
        "the crate executed no code at build time before"
    );

    let drift = clyde_policy::access::inventory_drift(&before.inventory, &after.inventory);
    assert_eq!(drift.len(), 1, "{drift:?}");
    match &drift[0] {
        AccessDrift::NewCodeExecCrate { crate_name, .. } => {
            assert_eq!(crate_name, "quiet-dep");
        }
        other => panic!("expected a new code-executing crate, got {other:?}"),
    }
    assert!(
        clyde_policy::access::requires_confirmation_before_build(&drift),
        "no build may run against the new bundle until a human confirms the change"
    );
    let rendered = clyde_policy::access::render_inventory_diff(&drift);
    assert!(rendered[0].starts_with('+'), "{rendered:?}");
}

#[test]
fn a_same_version_content_change_is_detected_and_reported_distinctly() {
    let harness = support::Harness::new();
    let before = import(&harness, "dep-same-version-tampered", "before");
    let after = import(&harness, "dep-same-version-tampered", "after");

    assert_eq!(before.lockfile_digest, after.lockfile_digest);
    let before_entry = before.inventory.find("ring").expect("pinned");
    let after_entry = after.inventory.find("ring").expect("pinned");
    assert_eq!(before_entry.version, after_entry.version);
    assert_ne!(
        before_entry.source_blake3, after_entry.source_blake3,
        "the content hash is what makes the same version falsifiable"
    );

    let drift = clyde_policy::access::inventory_drift(&before.inventory, &after.inventory);
    assert_eq!(drift.len(), 1);
    assert!(
        drift[0].is_tampering_signal(),
        "a same-version content change is tampering, not an upgrade"
    );
    let rendered = clyde_policy::access::render_inventory_diff(&drift);
    assert!(
        rendered[0].starts_with('!'),
        "it must be visually distinct from an upgrade: {rendered:?}"
    );
}

#[test]
fn a_build_against_an_unconfirmed_inventory_change_is_refused() {
    use clyde_core::task::{TaskOptions, TaskType};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let harness = support::Harness::with_toolchain();
        let workspace = harness.register("churn");
        let (mission, lease) =
            harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
        let baseline =
            harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
        assert!(baseline.inventory.entries.is_empty());

        // A bundle arrives whose inventory differs from the pinned one, and it
        // satisfies the workspace's lockfile.
        let lockfile = workspace.root.join("Cargo.lock");
        let parsed = clyde_snapshot::cargo::lockfile::read(&lockfile).unwrap();
        let mut record = import(&harness, "dep-gains-buildscript", "after");
        record.lockfile_digest = parsed.digest;
        harness.daemon.store.record_bundle(record.clone()).unwrap();

        let context = harness.context(&workspace, &mission, &lease);
        let error = clyded::tasks::run_task(
            &harness.daemon,
            &context,
            TaskType::RustCheck,
            clyde_core::repo_path::RepoPath::parse("crates/core").unwrap(),
            TaskOptions::RustCheck {
                package: None,
                all_targets: false,
            },
        )
        .await
        .expect_err("an unconfirmed inventory change must refuse the build");
        assert!(
            error.to_string().contains("has not been confirmed"),
            "{error}"
        );
        assert!(
            harness.recorded(&mission.id, "access.drift_detected"),
            "the drift is recorded by class"
        );

        // Once a human confirms the bundle, the build proceeds.
        harness
            .daemon
            .store
            .set_bundle_inventory_confirmed(
                &record.artifact,
                harness.operator.clone(),
                chrono::Utc::now(),
            )
            .unwrap();
        let run = clyded::tasks::run_task(
            &harness.daemon,
            &context,
            TaskType::RustCheck,
            clyde_core::repo_path::RepoPath::parse("crates/core").unwrap(),
            TaskOptions::RustCheck {
                package: None,
                all_targets: false,
            },
        )
        .await
        .expect("the build runs once the change is confirmed");
        assert_eq!(run.state, clyde_core::task::TaskRunState::Succeeded);
    });
}

#[test]
fn an_agent_cannot_confirm_an_inventory_change() {
    let harness = support::Harness::new();
    let record = import(&harness, "dep-gains-buildscript", "after");
    harness.daemon.store.record_bundle(record.clone()).unwrap();
    let error = harness
        .daemon
        .store
        .set_bundle_inventory_confirmed(
            &record.artifact,
            clyde_core::ids::ActorId::parse("agent:claude").unwrap(),
            chrono::Utc::now(),
        )
        .expect_err("an actor must not confirm the change that lets its build run");
    assert!(error.to_string().contains("not a human"));
}

#[test]
fn an_absent_crate_fails_as_missing_dependencies_not_as_a_project_error() {
    // The `missing-dep` fixture property, and the entry point to Phase 3.
    let classification = clyde_snapshot::classify(clyde_snapshot::ClassificationInput {
        exit: clyde_snapshot::ExitSummary {
            code: Some(101),
            timed_out: false,
            signal: None,
        },
        stdout: "",
        stderr: "error: no matching package named `absent-crate` found\nlocation searched: registry `crates-io`",
        egress_denied: false,
        excluded: &[],
    });
    assert_eq!(
        classification.class,
        clyde_core::task::TaskFailureClass::MissingDependencies
    );
    assert!(
        classification
            .missing_dependencies
            .contains(&"absent-crate".to_owned()),
        "the escalation has to be able to say what is missing"
    );
    assert!(!classification.class.is_users_code());
}

#[test]
fn configuration_can_pre_approve_a_low_risk_fetch_but_never_a_risky_one() {
    use clyde_policy::access::{
        LockedPackage, LockfileSummary, PackageSource, classify_lockfile_change,
    };

    let registry = |name: &str, version: &str| LockedPackage {
        name: name.to_owned(),
        version: version.to_owned(),
        source: PackageSource::Registry {
            index: "sparse+https://index.crates.io/".to_owned(),
        },
    };
    let previous = LockfileSummary {
        packages: vec![registry("serde", "1.0.0")],
    };

    // An addition from a registry already in use is the low-risk class.
    let addition = classify_lockfile_change(
        &previous,
        &LockfileSummary {
            packages: vec![registry("serde", "1.0.0"), registry("new", "0.1.0")],
        },
    );
    assert!(addition.is_pre_approvable(true, false));
    assert!(
        !addition.is_pre_approvable(false, false),
        "pre-approval is opt-in"
    );

    // A git dependency is never pre-approvable, whatever configuration says.
    let git = classify_lockfile_change(
        &previous,
        &LockfileSummary {
            packages: vec![
                registry("serde", "1.0.0"),
                LockedPackage {
                    name: "sketchy".to_owned(),
                    version: "0.1.0".to_owned(),
                    source: PackageSource::Git {
                        url: "https://example.test/x".to_owned(),
                        rev: None,
                    },
                },
            ],
        },
    );
    assert!(!git.is_pre_approvable(true, true));

    // Nor is a same-version source change.
    let tampered = classify_lockfile_change(
        &previous,
        &LockfileSummary {
            packages: vec![LockedPackage {
                name: "serde".to_owned(),
                version: "1.0.0".to_owned(),
                source: PackageSource::Registry {
                    index: "sparse+https://mirror.test/".to_owned(),
                },
            }],
        },
    );
    assert!(!tampered.is_pre_approvable(true, true));
}
