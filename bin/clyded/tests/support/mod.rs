//! Shared fixtures for the integration tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clyde_api::admin::CreateMission;
use clyde_core::ids::{ActorId, MissionId, WorkspaceId};
use clyde_core::lease::Lease;
use clyde_core::mission::Mission;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;
use clyde_core::workspace::{VcsKind, Workspace};
use clyde_policy::config::Config;
use clyde_sandbox::BackendRegistry;
use clyde_sandbox::test_backend::TestBackend;
use clyde_store::{MemoryStore, Store};
use clyded::daemon::{Daemon, DaemonOptions};

/// The repository's fixture directory.
pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("the fixture directory must exist")
}

/// Copies a fixture into a temporary directory, so a test can modify it.
pub fn copy_fixture(name: &str, into: &Path) -> PathBuf {
    let source = fixture_root().join(name);
    let target = into.join(name);
    copy_tree(&source, &target);
    target
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap().filter_map(Result::ok) {
        let child = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &child);
        } else {
            std::fs::copy(entry.path(), &child).unwrap();
        }
    }
}

/// A daemon with an in-memory store and the no-isolation test backend.
///
/// The backend is injected explicitly: the daemon's own registry construction
/// never adds it, so nothing here can happen by accident in a shipped binary.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub daemon: Arc<Daemon>,
    pub operator: ActorId,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_backends(
            BackendRegistry::new().with(Arc::new(TestBackend::new())),
            false,
        )
    }

    /// A harness whose `rust` runtime root points at the host toolchain, so an
    /// integration test can run a real `cargo check` through the pipeline.
    pub fn with_toolchain() -> Self {
        Self::with_backends(
            BackendRegistry::new().with(Arc::new(TestBackend::new())),
            true,
        )
    }

    pub fn with_backends(backends: BackendRegistry, toolchain: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Runtime roots arrive through the real configuration path rather than
        // through a test-only injection point, so the test exercises what a host
        // configuration would do.
        let host_config = dir.path().join("host.toml");
        let mut host = String::new();
        if toolchain {
            let root = toolchain_runtime_root(dir.path());
            let manifests = toolchain_manifests(dir.path(), &root);
            host.push_str(&format!(
                "[sandbox]\nruntime_root_rust = \"{}\"\nruntime_root_manifests = \"{}\"\n\n",
                root.display(),
                manifests.display()
            ));
        }
        // Push configuration is host configuration: a repository cannot widen
        // it, and without it every push is refused before a human is asked.
        host.push_str("[push]\nremotes = [\"origin\"]\nbranch_patterns = [\"feature/*\"]\n");
        std::fs::write(&host_config, host).unwrap();
        let options = DaemonOptions {
            state_root: dir.path().join("state"),
            config_paths: clyded::config_load::ConfigPaths {
                host: host_config,
                user: None,
            },
            backends: Some(backends),
            // A synthetic report, because this host cannot provide the boundary.
            // The rules that consult it are tested directly against both values
            // in clyde-policy; what these tests exercise is the pipeline.
            host_report: Some(capable_host()),
        };
        let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
        let daemon = Arc::new(Daemon::new(options, store).expect("the daemon builds"));
        Self {
            dir,
            daemon,
            operator: ActorId::parse("human:andrew").unwrap(),
        }
    }

    /// Registers a copied fixture as a workspace.
    pub fn register(&self, fixture: &str) -> Workspace {
        let root = copy_fixture(fixture, self.dir.path());
        let workspace = Workspace {
            id: clyde_core::ids::new::workspace_id().unwrap(),
            root,
            vcs: VcsKind::None,
            registered_at: chrono::Utc::now(),
            policy_digest: None,
        };
        self.daemon
            .store
            .register_workspace(workspace.clone())
            .unwrap();
        workspace
    }

    /// Proposes and approves a mission over the given paths.
    pub fn mission(
        &self,
        workspace: &WorkspaceId,
        edit_paths: &[&str],
        tasks: &[TaskType],
    ) -> (Mission, Lease) {
        let request = CreateMission {
            workspace: workspace.to_string(),
            objective: "integration test".to_owned(),
            edit_paths: edit_paths.iter().map(|path| (*path).to_owned()).collect(),
            read_paths: Vec::new(),
            tasks: tasks.iter().map(|task| task.name().to_owned()).collect(),
            expires_in: Some("1h".to_owned()),
            max_task_runs: Some(20),
            model_api: false,
            start_agent: false,
        };
        let (mission, _) = clyded::missions::propose(&self.daemon, &request, &self.operator)
            .expect("the mission proposes");
        clyded::missions::approve(&self.daemon, &mission.id, &self.operator)
            .expect("the mission approves")
    }

    /// The configuration in force for a workspace.
    pub fn config(&self, workspace: &Workspace) -> Config {
        self.daemon.config_for(&workspace.root).unwrap()
    }

    /// Confirms a static baseline for a target, as a human would.
    pub fn confirm_baseline(
        &self,
        workspace: &Workspace,
        mission: &Mission,
        task: TaskType,
        target: &str,
    ) -> clyde_core::baseline::AccessBaseline {
        let target = RepoPath::parse(target).unwrap();
        clyded::access::propose(&self.daemon, &workspace.id, task, &target, &mission.scope)
            .expect("a proposal");
        clyded::access::confirm(
            &self.daemon,
            &clyde_core::baseline::BaselineKey {
                workspace: workspace.id.clone(),
                task,
                target,
            },
            &self.operator,
        )
        .expect("a human confirms it")
    }

    /// A task context for running work, as a hosted actor would.
    pub fn context(
        &self,
        workspace: &Workspace,
        mission: &Mission,
        lease: &Lease,
    ) -> clyded::tasks::TaskContext {
        self.context_as(
            workspace,
            mission,
            lease,
            clyde_core::actor::Principal::Session {
                session_actor: lease.actor.clone(),
                hosted: true,
            },
        )
    }

    /// The same context as a human operator on the admin socket would produce
    /// (D25). Only the principal differs, which is the property worth testing.
    pub fn operator_context(
        &self,
        workspace: &Workspace,
        mission: &Mission,
        lease: &Lease,
    ) -> clyded::tasks::TaskContext {
        self.context_as(
            workspace,
            mission,
            lease,
            clyde_core::actor::Principal::Operator { uid: 1000 },
        )
    }

    fn context_as(
        &self,
        workspace: &Workspace,
        mission: &Mission,
        lease: &Lease,
        principal: clyde_core::actor::Principal,
    ) -> clyded::tasks::TaskContext {
        clyded::tasks::TaskContext {
            mission: mission.clone(),
            lease: lease.clone(),
            workspace: workspace.clone(),
            config: self.config(workspace),
            isolation_floor: None,
            principal,
        }
    }

    /// The audit events recorded so far.
    pub fn audit(&self, mission: &MissionId) -> Vec<clyde_core::audit::AuditEvent> {
        self.daemon
            .store
            .list_audit(&clyde_store::AuditFilter::for_mission(mission.clone()))
            .unwrap()
    }

    /// Whether an event of this kind was recorded for the mission.
    pub fn recorded(&self, mission: &MissionId, kind: &str) -> bool {
        self.audit(mission)
            .iter()
            .any(|event| event.kind.name() == kind)
    }
}

/// Builds a runtime root whose `bin` links to the host's real toolchain.
///
/// Only for the no-isolation test backend: it lets an integration test run a
/// real `cargo check` so the pipeline is exercised end to end. It is not a
/// runtime root in the D6 sense and would fail the workspace-root assertion,
/// which is why it is only ever used for the `rust` kind.
pub fn toolchain_runtime_root(into: &Path) -> PathBuf {
    let root = into.join("rust-runtime-root");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for program in ["cargo", "rustc"] {
        if let Some(path) = which(program) {
            let _ = std::os::unix::fs::symlink(path, root.join("bin").join(program));
        }
    }
    root
}

/// Writes the closure manifest for [`toolchain_runtime_root`].
///
/// The harness root links to the host toolchain, so its closure is the root plus
/// wherever those links land. Without this the daemon refuses the run — correctly,
/// since a root whose programs point outside its bound closure would fail with an
/// `execvp` ENOENT once a real backend bound it.
pub fn toolchain_manifests(into: &Path, root: &Path) -> PathBuf {
    let dir = into.join("manifests").join("rust");
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = vec![root.to_path_buf()];
    for program in ["cargo", "rustc"] {
        if let Ok(target) = root.join("bin").join(program).canonicalize() {
            // The store path, not the binary: `/nix/store/<hash>-cargo-x.y.z`.
            paths.extend(target.ancestors().nth(1).map(Path::to_path_buf));
        }
    }
    let listed: Vec<String> = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    std::fs::write(dir.join("store-paths"), listed.join("\n")).unwrap();
    into.join("manifests")
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// A host report that permits everything.
///
/// Used only by the integration harness. The boundary rules that consult a
/// report — most importantly D22's refusal of T2 tasks without cgroup
/// delegation — are asserted against both values in `clyde-policy`'s own tests,
/// so nothing here weakens them.
pub fn capable_host() -> clyde_sandbox::HostReport {
    let available = |detail: &str| clyde_sandbox::Capability::Available {
        detail: detail.to_owned(),
    };
    clyde_sandbox::HostReport {
        user_namespaces: available("test harness"),
        cgroup_v2: available("test harness"),
        cgroup_delegation: available("test harness"),
        session_bus: available("test harness"),
        runtime_roots: available("test harness"),
        kvm: available("test harness"),
        bubblewrap: available("test harness"),
        firecracker: available("test harness"),
        mke2fs: available("test harness"),
        guest_images: available("test harness"),
        nix: available("test harness"),
        hardlinks: available("test harness"),
        known_limitations: Vec::new(),
    }
}
