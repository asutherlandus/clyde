//! Sandbox specification construction.
//!
//! The mount table *is* the security boundary: a workspace environment's
//! writable set is exactly its lease's edit paths (D1), and a build sandbox sees
//! project source only through an immutable snapshot. Building these tables in
//! one place means the composition table in the design has a single
//! corresponding function, and the properties can be asserted over the value
//! rather than over a running process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clyde_core::classification::{EgressProfile, IsolationLevel, ResourceLimits, TrustClass};
use clyde_core::lease::Lease;
use clyde_core::task::{RuntimeRootKind, TaskPolicy};
use clyde_sandbox::runtime_root::RuntimeRoot;
use clyde_sandbox::spec::{Mount, MountMode, MountPurpose, SandboxSpec, ScratchPolicy};

/// Where things live inside a sandbox.
pub mod inside {
    /// The project tree.
    pub const WORK: &str = "/work";
    /// Scratch, discarded at teardown.
    pub const SCRATCH: &str = "/scratch";
    /// The per-mission writable cache.
    pub const CACHE: &str = "/cache";
    /// Clyde's runtime surface: sockets and the token file.
    pub const RUN: &str = "/run/clyde";
    /// The actor API socket.
    pub const ACTOR_SOCKET: &str = "/run/clyde/clyded.sock";
    /// The capability token, mode 0400.
    pub const SESSION_TOKEN: &str = "/run/clyde/session-token";
    /// The egress proxy bridge.
    pub const EGRESS_SOCKET: &str = "/run/clyde/egress.sock";
    /// The Clyde CA certificate. Workspace environments only.
    pub const CA_CERTIFICATE: &str = "/run/clyde/clyde-ca.pem";
    /// The in-sandbox forwarder.
    pub const FORWARDER: &str = "/run/clyde/clyde-forward";
}

/// The loopback port the in-sandbox forwarder listens on.
pub const FORWARDER_PORT: u16 = 8118;

/// Inputs for a workspace-environment sandbox.
#[derive(Debug, Clone)]
pub struct WorkspaceEnvironment<'a> {
    pub id: String,
    pub lease: &'a Lease,
    /// Absolute host path of the workspace root.
    pub workspace_root: &'a Path,
    pub runtime_root: &'a RuntimeRoot,
    pub limits: ResourceLimits,
    /// Host path of the actor socket.
    pub actor_socket: PathBuf,
    /// Host path of the token file, written mode 0400.
    pub token_file: PathBuf,
    /// Host path of the bound egress socket, when the lease permits egress.
    pub egress_socket: Option<PathBuf>,
    /// Host path of the CA certificate, mounted only when egress is terminated.
    pub ca_certificate: Option<PathBuf>,
    /// Host path of the forwarder binary.
    pub forwarder: Option<PathBuf>,
    /// The agent command and arguments, from host or user configuration only.
    pub argv: Vec<String>,
    /// Environment variables the configuration allowlists.
    pub passthrough_env: BTreeMap<String, String>,
}

/// Builds the workspace-environment specification.
pub fn workspace_environment(input: &WorkspaceEnvironment<'_>) -> SandboxSpec {
    let mut mounts = Vec::new();

    // The writable surface. This *is* the edit-scope enforcement: a sub-agent's
    // narrower lease yields a narrower writable set, and nothing it attempts
    // changes that.
    for path in &input.lease.repo_scope.edit_paths {
        mounts.push(Mount {
            source: Some(path.to_host_path(input.workspace_root)),
            target: PathBuf::from(inside::WORK).join(path.as_str()),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::EditScope,
        });
    }
    for path in &input.lease.repo_scope.read_paths {
        mounts.push(Mount {
            source: Some(path.to_host_path(input.workspace_root)),
            target: PathBuf::from(inside::WORK).join(path.as_str()),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::ReadScope,
        });
    }

    // `.git` read-only: an agent can read history but cannot plant hooks or
    // repository configuration a later trusted git invocation would execute.
    let git_dir = input.workspace_root.join(".git");
    if git_dir.exists() {
        mounts.push(Mount {
            source: Some(git_dir),
            target: PathBuf::from(inside::WORK).join(".git"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::GitDirectory,
        });
    }

    // Advisory guidance and visible policy. Prose, never parsed for authority.
    for name in ["AGENTS.md", "CLAUDE.md", ".clyde/policy.toml"] {
        let host = input.workspace_root.join(name);
        if host.exists() {
            mounts.push(Mount {
                source: Some(host),
                target: PathBuf::from(inside::WORK).join(name),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::Guidance,
            });
        }
    }

    mounts.push(Mount {
        source: Some(input.actor_socket.clone()),
        target: PathBuf::from(inside::ACTOR_SOCKET),
        mode: MountMode::Socket,
        purpose: MountPurpose::ActorSocket,
    });
    mounts.push(Mount {
        source: Some(input.token_file.clone()),
        target: PathBuf::from(inside::SESSION_TOKEN),
        mode: MountMode::ReadOnly,
        purpose: MountPurpose::SessionToken,
    });

    let mut env = BTreeMap::new();
    env.insert("HOME".to_owned(), inside::SCRATCH.to_owned());
    env.insert("TMPDIR".to_owned(), "/tmp".to_owned());
    env.insert(
        "PATH".to_owned(),
        format!("{}/bin", input.runtime_root.path.display()),
    );
    env.insert(
        "CLYDE_SESSION_TOKEN_FILE".to_owned(),
        inside::SESSION_TOKEN.to_owned(),
    );
    env.insert(
        "CLYDE_ACTOR_SOCKET".to_owned(),
        inside::ACTOR_SOCKET.to_owned(),
    );
    env.insert("CLYDE_WORKSPACE".to_owned(), inside::WORK.to_owned());
    env.extend(input.passthrough_env.clone());

    if let Some(socket) = &input.egress_socket {
        mounts.push(Mount {
            source: Some(socket.clone()),
            target: PathBuf::from(inside::EGRESS_SOCKET),
            mode: MountMode::Socket,
            purpose: MountPurpose::EgressSocket,
        });
        if let Some(forwarder) = &input.forwarder {
            mounts.push(Mount {
                source: Some(forwarder.clone()),
                target: PathBuf::from(inside::FORWARDER),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::Forwarder,
            });
        }
        for (key, value) in clyde_egress::forwarder::proxy_environment(FORWARDER_PORT) {
            env.insert(key, value);
        }
        // The CA certificate goes only where TLS is terminated, which is the
        // workspace environment and nowhere else.
        if let Some(certificate) = &input.ca_certificate {
            mounts.push(Mount {
                source: Some(certificate.clone()),
                target: PathBuf::from(inside::CA_CERTIFICATE),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::CaCertificate,
            });
            for name in ["SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS", "REQUESTS_CA_BUNDLE"] {
                env.insert(name.to_owned(), inside::CA_CERTIFICATE.to_owned());
            }
        }
    }

    SandboxSpec {
        id: input.id.clone(),
        runtime_root_kind: RuntimeRootKind::Workspace,
        runtime_root: input.runtime_root.path.clone(),
        runtime_root_closure: input.runtime_root.closure.clone(),
        mounts,
        egress: input.lease.network_scope.clone(),
        limits: input.limits,
        // The workspace environment hosts a semi-trusted agent, not hostile
        // dependency code, which is why it may run on rlimits and a timeout.
        trust_class: TrustClass::T1,
        min_isolation: IsolationLevel::NamespaceSandbox,
        env,
        argv: input.argv.clone(),
        cwd: PathBuf::from(inside::WORK),
        scratch: ScratchPolicy::default(),
        stdout_path: None,
        stderr_path: None,
    }
}

/// Inputs for a build or fetch sandbox.
#[derive(Debug, Clone)]
pub struct BuildSandbox<'a> {
    pub id: String,
    pub policy: &'a TaskPolicy,
    pub runtime_root: &'a RuntimeRoot,
    /// Host path of the materialised snapshot tree, bound read-only.
    pub snapshot_tree: PathBuf,
    /// Host path of the mission's writable cache.
    pub cache_root: Option<PathBuf>,
    /// Host path of the read-only dependency bundle.
    pub dependency_bundle: Option<PathBuf>,
    /// Host path of the bound egress socket, for a network-bearing task.
    pub egress_socket: Option<PathBuf>,
    pub forwarder: Option<PathBuf>,
    pub argv: Vec<String>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

/// Builds a build or fetch sandbox specification.
pub fn build_sandbox(input: &BuildSandbox<'_>) -> SandboxSpec {
    let mut mounts = vec![Mount {
        // Always read-only: hardlink materialisation shares inodes with the
        // content store, so a writable bind would corrupt it.
        source: Some(input.snapshot_tree.clone()),
        target: PathBuf::from(inside::WORK),
        mode: MountMode::ReadOnly,
        purpose: MountPurpose::Snapshot,
    }];

    let mut env = BTreeMap::new();
    env.insert(
        "PATH".to_owned(),
        format!("{}/bin", input.runtime_root.path.display()),
    );
    env.insert("HOME".to_owned(), inside::SCRATCH.to_owned());
    env.insert("TMPDIR".to_owned(), "/tmp".to_owned());
    // Offline by construction, not by convention: the sandbox has no route out
    // and cargo is told not to look for one, so a missing dependency fails
    // rather than triggering a fetch.
    if input.policy.egress.is_none() {
        env.insert("CARGO_NET_OFFLINE".to_owned(), "true".to_owned());
    }
    env.insert("CARGO_TERM_COLOR".to_owned(), "never".to_owned());
    // Build scripts inherit this and it keeps their output deterministic.
    env.insert("LC_ALL".to_owned(), "C".to_owned());
    env.insert("RUST_BACKTRACE".to_owned(), "1".to_owned());

    if let Some(cache) = &input.cache_root {
        mounts.push(Mount {
            source: Some(cache.join("cargo-target")),
            target: PathBuf::from(inside::CACHE).join("target"),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::MissionCache,
        });
        mounts.push(Mount {
            source: Some(cache.join("cargo-home")),
            target: PathBuf::from(inside::CACHE).join("cargo-home"),
            mode: MountMode::ReadWrite,
            purpose: MountPurpose::MissionCache,
        });
        env.insert(
            "CARGO_TARGET_DIR".to_owned(),
            format!("{}/target", inside::CACHE),
        );
        env.insert(
            "CARGO_HOME".to_owned(),
            format!("{}/cargo-home", inside::CACHE),
        );
    }

    if let Some(bundle) = &input.dependency_bundle {
        mounts.push(Mount {
            source: Some(bundle.clone()),
            target: PathBuf::from("/deps"),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::DependencyBundle,
        });
    }

    if let Some(socket) = &input.egress_socket {
        mounts.push(Mount {
            source: Some(socket.clone()),
            target: PathBuf::from(inside::EGRESS_SOCKET),
            mode: MountMode::Socket,
            purpose: MountPurpose::EgressSocket,
        });
        if let Some(forwarder) = &input.forwarder {
            mounts.push(Mount {
                source: Some(forwarder.clone()),
                target: PathBuf::from(inside::FORWARDER),
                mode: MountMode::ReadOnly,
                purpose: MountPurpose::Forwarder,
            });
        }
        for (key, value) in clyde_egress::forwarder::proxy_environment(FORWARDER_PORT) {
            env.insert(key, value);
        }
    }

    SandboxSpec {
        id: input.id.clone(),
        runtime_root_kind: input.policy.runtime_root,
        runtime_root: input.runtime_root.path.clone(),
        runtime_root_closure: input.runtime_root.closure.clone(),
        mounts,
        egress: input.policy.egress.clone(),
        limits: input.policy.limits,
        trust_class: input.policy.trust_class,
        min_isolation: input.policy.min_isolation,
        env,
        argv: input.argv.clone(),
        cwd: PathBuf::from(inside::WORK),
        scratch: ScratchPolicy::default(),
        stdout_path: Some(input.stdout_path.clone()),
        stderr_path: Some(input.stderr_path.clone()),
    }
}

/// Whether a profile needs a bound proxy socket.
pub fn needs_egress_socket(profile: &EgressProfile) -> bool {
    !profile.is_none() && !matches!(profile, EgressProfile::Broker)
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

    use chrono::Utc;
    use clyde_core::HumanDuration;
    use clyde_core::budget::{Budget, BudgetUsage};
    use clyde_core::classification::CredentialPolicy;
    use clyde_core::ids::{self, ActorId};
    use clyde_core::lease::{AuthorityFlags, LeaseState};
    use clyde_core::mission::MissionScope;
    use clyde_core::repo_path::RepoPath;
    use clyde_core::task::TaskType;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn limits() -> ResourceLimits {
        ResourceLimits {
            max_wall_clock: HumanDuration::parse("1h").unwrap(),
            max_memory_bytes: 4 << 30,
            max_cpu_percent: 200,
            max_tasks: 256,
            max_open_files: 4096,
        }
    }

    fn lease(egress: EgressProfile) -> Lease {
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
            task_scope: [TaskType::RustCheck].into_iter().collect(),
            network_scope: egress,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags {
                may_edit: true,
                may_request_tasks: true,
                may_spawn_subagents: false,
                may_request_publish: false,
            },
            budget: Budget {
                max_duration: HumanDuration::parse("1h").unwrap(),
                max_task_runs: 10,
                max_parallel_subagents: 1,
                max_subagents: 1,
                max_cpu_seconds: 600,
                max_cache_bytes: 1 << 30,
                max_artifact_bytes: 1 << 28,
                max_egress_bytes: 1 << 20,
                max_egress_requests: 100,
            },
            usage: BudgetUsage::default(),
            state: LeaseState::Active,
            purpose: "primary".to_owned(),
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        workspace: PathBuf,
        runtime_root: RuntimeRoot,
        token: PathBuf,
        socket: PathBuf,
        ca: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(workspace.join("crates/core")).unwrap();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "guidance\n").unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, "x").unwrap();
        let socket = dir.path().join("clyded.sock");
        std::fs::write(&socket, "x").unwrap();
        let ca = dir.path().join("clyde-ca.pem");
        std::fs::write(&ca, "cert").unwrap();
        Fixture {
            _dir: dir,
            workspace,
            runtime_root: RuntimeRoot {
                kind: RuntimeRootKind::Workspace,
                path: PathBuf::from("/nix/store/workspace-root"),
                closure: vec![PathBuf::from("/nix/store/workspace-root")],
                binaries: vec!["sh".to_owned()],
            },
            token,
            socket,
            ca,
        }
    }

    fn workspace_spec(fixture: &Fixture, lease: &Lease, egress: bool) -> SandboxSpec {
        workspace_environment(&WorkspaceEnvironment {
            id: "sb-ws".to_owned(),
            lease,
            workspace_root: &fixture.workspace,
            runtime_root: &fixture.runtime_root,
            limits: limits(),
            actor_socket: fixture.socket.clone(),
            token_file: fixture.token.clone(),
            egress_socket: egress.then(|| fixture._dir.path().join("egress.sock")),
            ca_certificate: egress.then(|| fixture.ca.clone()),
            forwarder: egress.then(|| PathBuf::from("/usr/libexec/clyde-forward")),
            argv: vec!["/nix/store/workspace-root/bin/sh".to_owned()],
            passthrough_env: BTreeMap::from([("TERM".to_owned(), "xterm".to_owned())]),
        })
    }

    #[test]
    fn the_writable_set_is_exactly_the_leases_edit_paths() {
        let fixture = fixture();
        let lease = lease(EgressProfile::None);
        let spec = workspace_spec(&fixture, &lease, false);
        let writable: Vec<&PathBuf> = spec
            .mounts
            .iter()
            .filter(|mount| mount.purpose == MountPurpose::EditScope)
            .map(|mount| &mount.target)
            .collect();
        assert_eq!(writable, vec![&PathBuf::from("/work/crates/core")]);
        let read_only: Vec<&PathBuf> = spec
            .mounts
            .iter()
            .filter(|mount| mount.purpose == MountPurpose::ReadScope)
            .map(|mount| &mount.target)
            .collect();
        assert_eq!(read_only, vec![&PathBuf::from("/work/docs")]);
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn a_narrower_lease_yields_a_narrower_writable_set() {
        // Sub-agent narrowing is real: it is the mount table, not a check.
        let fixture = fixture();
        let mut lease = lease(EgressProfile::None);
        lease.repo_scope.edit_paths = BTreeSet::new();
        let spec = workspace_spec(&fixture, &lease, false);
        assert!(
            !spec.has_purpose(MountPurpose::EditScope),
            "a lease with no edit paths must have no writable project mount"
        );
    }

    #[test]
    fn git_is_mounted_read_only_in_the_workspace_environment() {
        let fixture = fixture();
        let lease = lease(EgressProfile::None);
        let spec = workspace_spec(&fixture, &lease, false);
        let git = spec
            .mounts
            .iter()
            .find(|mount| mount.purpose == MountPurpose::GitDirectory)
            .expect("the agent can read history");
        assert_eq!(
            git.mode,
            MountMode::ReadOnly,
            "a writable .git would let an agent plant a pre-push hook"
        );
    }

    #[test]
    fn no_workspace_mount_reaches_a_credential_directory() {
        let fixture = fixture();
        let lease = lease(EgressProfile::ModelApi);
        let spec = workspace_spec(&fixture, &lease, true);
        assert!(
            clyde_sandbox::bubblewrap::credential_shaped_mounts(&spec).is_empty(),
            "no host home, ssh, gnupg, or container socket may be reachable"
        );
        assert!(!spec.touches_host_path(Path::new("/root")));
    }

    #[test]
    fn the_ca_certificate_and_proxy_environment_appear_only_with_egress() {
        let fixture = fixture();
        let lease = lease(EgressProfile::ModelApi);
        let with = workspace_spec(&fixture, &lease, true);
        assert!(with.has_purpose(MountPurpose::CaCertificate));
        assert_eq!(
            with.env.get("SSL_CERT_FILE").map(String::as_str),
            Some(inside::CA_CERTIFICATE)
        );
        assert!(with.env.contains_key("https_proxy"));
        assert_eq!(with.validate(), Ok(()));

        let lease = self::lease(EgressProfile::None);
        let without = workspace_spec(&fixture, &lease, false);
        assert!(!without.has_purpose(MountPurpose::CaCertificate));
        assert!(!without.env.contains_key("https_proxy"));
        assert_eq!(without.validate(), Ok(()));
    }

    #[test]
    fn the_token_file_is_mounted_read_only_and_is_not_in_the_environment() {
        let fixture = fixture();
        let lease = lease(EgressProfile::None);
        let spec = workspace_spec(&fixture, &lease, false);
        let token = spec
            .mounts
            .iter()
            .find(|mount| mount.purpose == MountPurpose::SessionToken)
            .expect("the token is delivered by file");
        assert_eq!(token.mode, MountMode::ReadOnly);
        for value in spec.env.values() {
            assert!(
                value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()),
                "a token must never appear in the environment"
            );
        }
        assert!(
            spec.argv.iter().all(|arg| !arg.contains("token=")),
            "a token must never appear in argv"
        );
    }

    fn build_spec(egress: bool) -> SandboxSpec {
        let mut policy = clyde_policy::builtin_policy(TaskType::RustCheck);
        if egress {
            policy = clyde_policy::builtin_policy(TaskType::RustResolveDeps);
        }
        let runtime_root = RuntimeRoot {
            kind: policy.runtime_root,
            path: PathBuf::from("/nix/store/rust-root"),
            closure: vec![PathBuf::from("/nix/store/rust-root")],
            binaries: vec!["cargo".to_owned()],
        };
        build_sandbox(&BuildSandbox {
            id: "sb-build".to_owned(),
            policy: &policy,
            runtime_root: &runtime_root,
            snapshot_tree: PathBuf::from("/var/lib/clyde/snapshots/trees/abc"),
            cache_root: Some(PathBuf::from("/var/lib/clyde/missions/m1")),
            dependency_bundle: Some(PathBuf::from("/var/lib/clyde/deps/bundle")),
            egress_socket: egress.then(|| PathBuf::from("/run/clyde/egress-1.sock")),
            forwarder: egress.then(|| PathBuf::from("/usr/libexec/clyde-forward")),
            argv: vec![
                "/nix/store/rust-root/bin/cargo".to_owned(),
                "check".to_owned(),
            ],
            stdout_path: PathBuf::from("/var/lib/clyde/logs/tasks/t1/stdout"),
            stderr_path: PathBuf::from("/var/lib/clyde/logs/tasks/t1/stderr"),
        })
    }

    #[test]
    fn a_build_sandbox_sees_source_only_through_a_read_only_snapshot() {
        let spec = build_spec(false);
        let snapshot = spec
            .mounts
            .iter()
            .find(|mount| mount.purpose == MountPurpose::Snapshot)
            .expect("a snapshot mount");
        assert_eq!(snapshot.mode, MountMode::ReadOnly);
        assert!(
            clyde_sandbox::bubblewrap::live_workspace_mounts(&spec).is_empty(),
            "project code executes only against snapshots, never the live tree"
        );
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn a_build_sandbox_is_offline_and_receives_no_ca_certificate() {
        let spec = build_spec(false);
        assert!(spec.egress.is_none());
        assert!(!spec.has_purpose(MountPurpose::EgressSocket));
        assert!(!spec.has_purpose(MountPurpose::CaCertificate));
        assert_eq!(
            spec.env.get("CARGO_NET_OFFLINE").map(String::as_str),
            Some("true")
        );
        assert!(!spec.env.contains_key("https_proxy"));
        // A sandbox that does not trust the CA cannot be transparently
        // intercepted, which is what keeps the carve-out from spreading.
        assert!(!spec.env.contains_key("SSL_CERT_FILE"));
    }

    #[test]
    fn the_dependency_bundle_is_read_only_and_the_cache_is_writable() {
        let spec = build_spec(false);
        let bundle = spec
            .mounts
            .iter()
            .find(|mount| mount.purpose == MountPurpose::DependencyBundle)
            .unwrap();
        assert_eq!(bundle.mode, MountMode::ReadOnly);
        let writable: Vec<&PathBuf> = spec
            .mounts
            .iter()
            .filter(|mount| mount.purpose == MountPurpose::MissionCache)
            .map(|mount| &mount.target)
            .collect();
        assert_eq!(writable.len(), 2);
        assert_eq!(
            spec.env.get("CARGO_TARGET_DIR").map(String::as_str),
            Some("/cache/target")
        );
    }

    #[test]
    fn a_fetch_sandbox_gets_a_socket_and_the_proxy_environment_but_no_ca() {
        let spec = build_spec(true);
        assert!(spec.has_purpose(MountPurpose::EgressSocket));
        assert!(spec.has_purpose(MountPurpose::Forwarder));
        assert!(spec.env.contains_key("https_proxy"));
        assert!(
            !spec.has_purpose(MountPurpose::CaCertificate),
            "intercepting dependency traffic would undermine the content-hash story"
        );
        assert!(!spec.env.contains_key("CARGO_NET_OFFLINE"));
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn build_sandboxes_write_their_logs_to_host_files() {
        let spec = build_spec(false);
        assert!(spec.stdout_path.is_some());
        assert!(spec.stderr_path.is_some());
    }

    #[test]
    fn the_broker_profile_needs_no_sandbox_socket() {
        assert!(!needs_egress_socket(&EgressProfile::None));
        assert!(!needs_egress_socket(&EgressProfile::Broker));
        assert!(needs_egress_socket(&EgressProfile::ModelApi));
        assert!(needs_egress_socket(&EgressProfile::RustRegistry));
    }
}
