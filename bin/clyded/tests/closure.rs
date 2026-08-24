//! Build-closure computation over the four project layouts that actually
//! differ.
//!
//! Getting this wrong is the most likely cause of "Clyde can't build my
//! project", which is why each layout is a fixture with a stated property rather
//! than an inline string.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use clyde_core::repo_path::RepoPath;
use clyde_snapshot::compute_closure;

fn path(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

#[test]
fn a_single_crate_project_has_its_own_manifest_as_the_root() {
    let root = support::fixture_root().join("single-crate");
    let closure = compute_closure(&root, &RepoPath::root()).unwrap();
    assert_eq!(closure.root_manifest, Some(path("Cargo.toml")));
    assert!(closure.config_files.contains(&path("Cargo.lock")));
    assert!(
        closure.member_manifests.is_empty(),
        "there is no workspace, so there are no members: {:?}",
        closure.member_manifests
    );
    assert!(closure.source_subtrees.contains(&RepoPath::root()));
}

#[test]
fn a_virtual_workspace_pulls_in_every_member_manifest_but_not_their_sources() {
    let root = support::fixture_root().join("virtual-workspace");
    let closure = compute_closure(&root, &path("crates/alpha")).unwrap();

    assert_eq!(closure.root_manifest, Some(path("Cargo.toml")));
    assert!(
        closure
            .member_manifests
            .contains(&path("crates/beta/Cargo.toml")),
        "cargo fails if a listed member's manifest is unreadable: {:?}",
        closure.member_manifests
    );
    assert!(
        !closure.source_subtrees.contains(&path("crates/beta")),
        "the other member's source is not needed and must not be admitted"
    );
    assert!(closure.config_files.contains(&path(".cargo/config.toml")));
    assert!(closure.config_files.contains(&path("rust-toolchain.toml")));
    assert!(closure.config_files.contains(&path("Cargo.lock")));
}

#[test]
fn workspace_inherited_dependencies_are_followed() {
    let root = support::fixture_root().join("inherited-deps");
    let closure = compute_closure(&root, &path("crates/app")).unwrap();
    assert!(
        closure
            .member_manifests
            .contains(&path("crates/shared/Cargo.toml")),
        "the path lives in the root's [workspace.dependencies], not the member's: {:?}",
        closure.member_manifests
    );
}

#[test]
fn nested_path_dependencies_are_followed_transitively() {
    let root = support::fixture_root().join("nested-path-deps");
    let closure = compute_closure(&root, &path("a")).unwrap();
    for crate_path in ["a", "b", "c"] {
        assert!(
            closure.source_subtrees.contains(&path(crate_path)),
            "{crate_path} needs full source: {:?}",
            closure.source_subtrees
        );
    }
    let dependents: Vec<&str> = closure
        .path_dependencies
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(dependents.contains(&"a"));
    assert!(dependents.contains(&"b"));
}

#[test]
fn a_closure_over_a_workspace_never_grants_the_repository_root() {
    // The proposal turns a closure into a baseline; granting the root to reach a
    // manifest would widen the read set far past what the build needs.
    let root = support::fixture_root().join("virtual-workspace");
    let closure = compute_closure(&root, &path("crates/alpha")).unwrap();
    let proposal = clyde_snapshot::propose_from_closure(
        clyde_core::ids::new::workspace_id().unwrap(),
        clyde_core::task::TaskType::RustCheck,
        path("crates/alpha"),
        &clyde_core::mission::MissionScope {
            edit_paths: [path("crates/alpha")].into_iter().collect(),
            read_paths: Default::default(),
        },
        &closure,
    );
    assert!(
        !proposal.paths.iter().any(|entry| entry.path().is_root()),
        "the repository root must never be granted: {:?}",
        proposal.paths
    );
    // The manifests it needs are still admitted, individually.
    let confirmed = proposal
        .confirm(
            clyde_core::ids::ActorId::parse("human:andrew").unwrap(),
            chrono::Utc::now(),
        )
        .unwrap();
    assert!(confirmed.admits(&path("Cargo.toml")));
    assert!(confirmed.admits(&path("crates/beta/Cargo.toml")));
    assert!(
        !confirmed.admits(&path("crates/beta/src/lib.rs")),
        "manifests only, not the other member's sources"
    );
}
