//! Security properties asserted over the sandbox specifications the system can
//! actually generate.
//!
//! These are structural rather than behavioural on purpose. This host cannot
//! create unprivileged user namespaces, so a test that started a sandbox would
//! assert nothing; a test over the specification asserts the property that the
//! sandbox would enforce, and does so on every host.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clyde_core::classification::{EgressProfile, TrustClass};
use clyde_core::task::{RuntimeRootKind, TaskType};
use clyde_sandbox::bubblewrap::{credential_shaped_mounts, live_workspace_mounts};
use clyde_sandbox::runtime_root::RuntimeRoot;
use clyde_sandbox::spec::{MountMode, MountPurpose, SandboxSpec};
use clyded::sandboxes;

fn runtime_root(kind: RuntimeRootKind) -> RuntimeRoot {
    RuntimeRoot {
        kind,
        path: PathBuf::from("/nix/store/runtime-root"),
        closure: vec![PathBuf::from("/nix/store/runtime-root")],
        binaries: vec!["sh".to_owned(), "grep".to_owned()],
    }
}

/// Every specification the system can generate, for the "no spec anywhere"
/// assertions.
fn every_spec(harness: &support::Harness) -> Vec<SandboxSpec> {
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(
        &workspace.id,
        &["crates/core"],
        &[
            TaskType::RustCheck,
            TaskType::RustTestUnit,
            TaskType::WorkspaceEdit,
        ],
    );
    let config = harness.config(&workspace);
    let mut specs = Vec::new();

    // The workspace environment, with and without egress.
    for egress in [EgressProfile::None, EgressProfile::ModelApi] {
        let mut lease = lease.clone();
        lease.network_scope = egress.clone();
        let egress_socket = sandboxes::needs_egress_socket(&egress)
            .then(|| PathBuf::from("/run/clyde/egress.sock"));
        specs.push(sandboxes::workspace_environment(
            &sandboxes::WorkspaceEnvironment {
                id: "ws".to_owned(),
                lease: &lease,
                workspace_root: &workspace.root,
                runtime_root: &runtime_root(RuntimeRootKind::Workspace),
                limits: config.limits.workspace,
                actor_socket: PathBuf::from("/run/clyde/clyded.sock"),
                token_file: PathBuf::from("/run/clyde/token"),
                egress_socket: egress_socket.clone(),
                ca_certificate: egress
                    .terminates_tls()
                    .then(|| PathBuf::from("/var/lib/clyde/ca/clyde-ca.pem")),
                forwarder: egress_socket.map(|_| PathBuf::from("/nix/store/clyde-forward")),
                argv: vec!["/nix/store/runtime-root/bin/sh".to_owned()],
                passthrough_env: BTreeMap::new(),
            },
        ));
    }

    // Every build and fetch task.
    for task in [
        TaskType::RustCheck,
        TaskType::RustTestUnit,
        TaskType::RustResolveDeps,
    ] {
        let policy = clyde_policy::builtin_policy(task);
        let egress_socket = sandboxes::needs_egress_socket(&policy.egress)
            .then(|| PathBuf::from("/run/clyde/egress.sock"));
        specs.push(sandboxes::build_sandbox(&sandboxes::BuildSandbox {
            id: task.name().to_owned(),
            policy: &policy,
            runtime_root: &runtime_root(policy.runtime_root),
            snapshot_tree: PathBuf::from("/var/lib/clyde/snapshots/trees/abc"),
            cache_root: Some(PathBuf::from("/var/lib/clyde/missions/m1")),
            dependency_bundle: Some(PathBuf::from("/var/lib/clyde/deps/bundle")),
            egress_socket: egress_socket.clone(),
            forwarder: egress_socket.map(|_| PathBuf::from("/nix/store/clyde-forward")),
            argv: vec!["/nix/store/runtime-root/bin/cargo".to_owned()],
            stdout_path: PathBuf::from("/var/lib/clyde/logs/out"),
            stderr_path: PathBuf::from("/var/lib/clyde/logs/err"),
        }));
    }
    let _ = mission;
    specs
}

#[test]
fn no_specification_the_system_can_generate_carries_a_credential() {
    // The Phase 4 property, asserted over every spec rather than over one.
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        assert!(
            credential_shaped_mounts(&spec).is_empty(),
            "{} reaches a credential path: {:?}",
            spec.id,
            credential_shaped_mounts(&spec)
        );
        for forbidden in [
            "/root",
            "/run/docker.sock",
            "/var/run/docker.sock",
            "/run/podman",
        ] {
            assert!(
                !spec.touches_host_path(Path::new(forbidden)),
                "{} reaches {forbidden}",
                spec.id
            );
        }
        // No environment variable carries credential *material*. The token
        // *path* is a different thing and is expected: the token is delivered by
        // file precisely so that its content is never in the environment.
        assert!(
            !spec.env.contains_key("SSH_AUTH_SOCK"),
            "{} can reach an ssh agent",
            spec.id
        );
        for (key, value) in &spec.env {
            assert!(
                !value.contains("BEGIN") && !value.contains("Bearer "),
                "{} has credential-shaped content in {key}",
                spec.id
            );
            if key.ends_with("_FILE") {
                assert!(
                    value.starts_with('/'),
                    "{} has {key} holding something that is not a path",
                    spec.id
                );
            }
        }
    }
}

#[test]
fn no_build_or_fetch_sandbox_receives_the_ca_certificate() {
    // A sandbox that does not trust the CA cannot be transparently intercepted,
    // even by Clyde. That is what keeps the model-api carve-out from spreading.
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        if spec.trust_class >= TrustClass::T2 {
            assert!(
                !spec.has_purpose(MountPurpose::CaCertificate),
                "{} receives the CA certificate",
                spec.id
            );
            assert!(!spec.env.contains_key("SSL_CERT_FILE"), "{}", spec.id);
        }
    }
}

#[test]
fn no_build_sandbox_receives_git_or_the_live_workspace() {
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        if spec.trust_class < TrustClass::T2 {
            continue;
        }
        assert!(
            !spec.has_purpose(MountPurpose::GitDirectory),
            "{} receives .git, which is never available to a build task",
            spec.id
        );
        assert!(
            live_workspace_mounts(&spec).is_empty(),
            "{} binds the live workspace rather than a snapshot",
            spec.id
        );
        // And the spec's own validation refuses it, so this cannot regress by
        // someone adding a mount.
        assert_eq!(spec.validate(), Ok(()));
    }
}

#[test]
fn an_offline_task_has_no_egress_socket_and_a_fetch_has_one() {
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        if spec.egress.is_none() {
            assert!(
                !spec.has_purpose(MountPurpose::EgressSocket),
                "{} has an egress socket under profile none",
                spec.id
            );
        } else {
            assert!(
                spec.has_purpose(MountPurpose::EgressSocket),
                "{} permits egress but has no socket",
                spec.id
            );
        }
    }
}

#[test]
fn snapshots_and_bundles_are_read_only_and_only_the_cache_is_writable() {
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        for mount in &spec.mounts {
            match mount.purpose {
                MountPurpose::Snapshot | MountPurpose::DependencyBundle => assert_eq!(
                    mount.mode,
                    MountMode::ReadOnly,
                    "{} binds {:?} writable",
                    spec.id,
                    mount.purpose
                ),
                _ => {}
            }
        }
        if spec.trust_class >= TrustClass::T2 {
            let writable: Vec<MountPurpose> = spec
                .mounts
                .iter()
                .filter(|mount| matches!(mount.mode, MountMode::ReadWrite))
                .map(|mount| mount.purpose)
                .collect();
            assert!(
                writable
                    .iter()
                    .all(|purpose| *purpose == MountPurpose::MissionCache),
                "{} has writable mounts other than the mission cache: {writable:?}",
                spec.id
            );
        }
    }
}

#[test]
fn a_token_never_appears_in_an_argv_or_an_environment() {
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        for value in spec.env.values().chain(spec.argv.iter()) {
            assert!(
                !(value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())),
                "{} carries something token-shaped",
                spec.id
            );
        }
    }
}

#[test]
fn the_workspace_environments_writable_set_is_exactly_the_leases_edit_paths() {
    let harness = support::Harness::new();
    let specs = every_spec(&harness);
    let workspace_spec = specs
        .iter()
        .find(|spec| spec.runtime_root_kind == RuntimeRootKind::Workspace)
        .expect("a workspace environment");
    let writable: Vec<&PathBuf> = workspace_spec
        .mounts
        .iter()
        .filter(|mount| mount.purpose == MountPurpose::EditScope)
        .map(|mount| &mount.target)
        .collect();
    assert_eq!(writable, vec![&PathBuf::from("/work/crates/core")]);
}

#[test]
fn the_admin_socket_is_never_in_any_specification() {
    // An agent cannot approve anything, and the mechanism is that the socket is
    // not there to reach.
    let harness = support::Harness::new();
    for spec in every_spec(&harness) {
        for mount in &spec.mounts {
            let target = mount.target.to_string_lossy();
            assert!(
                !target.contains("clyded-admin"),
                "{} can reach the admin socket",
                spec.id
            );
            assert!(
                !target.contains("brokerd"),
                "{} can reach the broker socket",
                spec.id
            );
            if let Some(source) = &mount.source {
                let source = source.to_string_lossy();
                assert!(!source.contains("clyded-admin"), "{}", spec.id);
                assert!(!source.contains("brokerd"), "{}", spec.id);
                assert!(
                    !clyde_egress::ca::is_ca_key_path(Path::new(source.as_ref())),
                    "{} can read the CA private key",
                    spec.id
                );
            }
        }
    }
}

#[test]
fn a_workspace_runtime_root_containing_a_toolchain_is_refused() {
    // The assertion the flake makes over the derivation, made again here so a
    // hand-configured root cannot quietly reintroduce a toolchain.
    let assertion = clyde_sandbox::assert_workspace_root(&[
        "sh".to_owned(),
        "grep".to_owned(),
        "sed".to_owned(),
        "find".to_owned(),
        "jq".to_owned(),
        "diff".to_owned(),
        "cargo".to_owned(),
    ]);
    assert!(!assertion.holds());
    assert!(assertion.render().contains("without asking Clyde"));
}
