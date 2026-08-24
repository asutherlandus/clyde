//! The daemon's shared state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use clyde_core::Redacted;
use clyde_core::ids::LeaseId;
use clyde_egress::ClydeCa;
use clyde_egress::proxy::ProxyHandle;
use clyde_git::GitRunner;
use clyde_policy::config::Config;
use clyde_sandbox::capability::{ProbePaths, probe};
use clyde_sandbox::firecracker::FirecrackerConfig;
use clyde_sandbox::runtime_root::RuntimeRoots;
use clyde_sandbox::{
    BackendRegistry, BubblewrapBackend, FirecrackerBackend, HostReport, LimitTools, SandboxHandle,
};
use clyde_snapshot::{BundleStore, ContentStore};
use clyde_store::Store;
use tokio::sync::Mutex;

use crate::broker_gateway::BrokerGateway;
use crate::config_load::{self, ConfigPaths};
use crate::error::{DaemonError, Result};
use crate::paths::StatePaths;

/// A running workspace-environment sandbox and the proxy bound to it.
#[derive(Debug)]
pub struct RunningEnvironment {
    pub sandbox: SandboxHandle,
    pub proxy: Option<ProxyHandle>,
    /// The agent process, where one was started.
    pub agent_started: bool,
}

/// Everything the daemon holds.
///
/// Constructed once and shared; the mutable parts are behind their own locks so
/// no request handler holds a lock across a socket operation.
pub struct Daemon {
    pub paths: StatePaths,
    pub store: Arc<dyn Store>,
    /// Defaults plus host and user configuration. Repository configuration is
    /// applied per workspace, because it is untrusted content.
    pub base_config: Config,
    pub ca: Arc<ClydeCa>,
    pub backends: BackendRegistry,
    pub runtime_roots: RuntimeRoots,
    pub content: ContentStore,
    pub bundles: BundleStore,
    pub host: HostReport,
    pub git: GitRunner,
    pub broker: BrokerGateway,
    /// The model API credential, injected host-side by the proxy so the agent
    /// never holds it (D11).
    pub model_api_credential: Option<Redacted<String>>,
    /// Live workspace environments, by lease.
    pub environments: Mutex<BTreeMap<LeaseId, RunningEnvironment>>,
    /// The identity commits are attributed to.
    pub commit_identity: Option<clyde_git::Identity>,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("root", &self.paths.root())
            .field("backends", &self.backends)
            .field(
                "model_api_credential",
                &self.model_api_credential.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// How to construct a daemon.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub state_root: PathBuf,
    pub config_paths: ConfigPaths,
    /// Overrides the backend registry. Used by integration tests to inject a
    /// backend; never set by the shipped binary.
    pub backends: Option<BackendRegistry>,
    /// Overrides the host capability report.
    ///
    /// Used by integration tests so the pipeline can be exercised on a host that
    /// cannot provide the boundary — a container, or a default Ubuntu 24.04
    /// install. It does not weaken any check: admission still consults the
    /// report, and the rules themselves (D22 in particular) are tested directly
    /// against both values in `clyde-policy`. The shipped binary leaves this
    /// `None` and gets the real probe.
    pub host_report: Option<HostReport>,
}

impl DaemonOptions {
    pub fn new(state_root: PathBuf) -> Self {
        Self {
            state_root,
            config_paths: ConfigPaths::discover(),
            backends: None,
            host_report: None,
        }
    }
}

impl Daemon {
    /// Builds the daemon, with the store supplied by the caller so tests can use
    /// the in-memory implementation.
    pub fn new(options: DaemonOptions, store: Arc<dyn Store>) -> Result<Self> {
        let paths = StatePaths::new(options.state_root);
        paths
            .create_all()
            .map_err(|error| DaemonError::io("creating the state directory", error))?;

        let loaded = config_load::load_base(&options.config_paths)?;
        for record in &loaded.records {
            config_load::record_load(store.as_ref(), None, record);
        }
        let config = loaded.config;

        let host = options.host_report.clone().unwrap_or_else(|| {
            probe(&ProbePaths {
                bwrap: config.sandbox.bwrap.clone(),
                firecracker: config.sandbox.firecracker.clone(),
                nix: None,
                state_dir: Some(paths.root().to_path_buf()),
            })
        });

        let ca = Arc::new(ClydeCa::load_or_create(&paths.ca())?);
        let content = ContentStore::open(paths.snapshots())?;
        let bundles = BundleStore::open(paths.deps())?;
        let runtime_roots =
            RuntimeRoots::from_paths(&config.sandbox.runtime_roots, None).unwrap_or_default();
        let git = GitRunner::discover()?;
        let broker = BrokerGateway::new(
            config
                .broker
                .socket
                .clone()
                .unwrap_or_else(|| paths.broker_socket()),
        );

        let backends = match options.backends {
            Some(backends) => backends,
            None => build_backends(&config, &host, &paths),
        };

        // The credential is read from the environment once, at start, and lives
        // only in memory behind a redaction wrapper. It is never written to
        // state, never logged, and never placed in a sandbox.
        let model_api_credential = std::env::var("CLYDE_MODEL_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                let header = config
                    .egress
                    .model_api_auth_header
                    .clone()
                    .unwrap_or_else(|| "Bearer {}".to_owned());
                Redacted::new(header.replace("{}", &value), "model api credential")
            });

        let commit_identity = commit_identity();

        Ok(Self {
            paths,
            store,
            base_config: config,
            ca,
            backends,
            runtime_roots,
            content,
            bundles,
            host,
            git,
            broker,
            model_api_credential,
            environments: Mutex::new(BTreeMap::new()),
            commit_identity,
        })
    }

    /// The policy layer's view of what this host can provide.
    ///
    /// Isolation comes from the *registry* rather than from the raw probe: a
    /// backend that is registered is one that can run, and a probe that says
    /// otherwise would refuse work the host can actually do. Cgroup delegation
    /// still comes from the probe, because no backend can conjure it (D22).
    pub fn host_capabilities(&self) -> clyde_policy::HostCapabilities {
        clyde_policy::HostCapabilities {
            strongest_isolation: self.backends.strongest_isolation(),
            cgroup_delegation: self.host.cgroup_delegation.is_available(),
        }
    }

    /// The configuration in force for a workspace, with its repository layer
    /// applied.
    ///
    /// A repository whose configuration would widen authority produces an error
    /// here, which surfaces at mission creation rather than silently narrowing.
    pub fn config_for(&self, workspace_root: &std::path::Path) -> Result<Config> {
        match config_load::load_repository(&self.base_config, workspace_root) {
            Ok((config, record)) => {
                if let Some(record) = record {
                    config_load::record_load(self.store.as_ref(), None, &record);
                }
                Ok(config)
            }
            Err(error) => {
                config_load::record_rejection(self.store.as_ref(), None, &error);
                Err(DaemonError::Config(error))
            }
        }
    }
}

/// Builds the backend registry from configuration and host capability.
///
/// A backend whose prerequisites are missing is simply not registered, so
/// selection refuses rather than starting something that cannot work.
fn build_backends(config: &Config, host: &HostReport, paths: &StatePaths) -> BackendRegistry {
    let mut registry = BackendRegistry::new();
    let tools = LimitTools {
        systemd_run: clyde_sandbox::capability::resolve_program(
            config.sandbox.systemd_run.as_deref(),
            "systemd-run",
        ),
        prlimit: clyde_sandbox::capability::resolve_program(
            config.sandbox.prlimit.as_deref(),
            "prlimit",
        ),
        cgroup_delegation: host.cgroup_delegation.is_available(),
    };
    if let Some(bwrap) =
        clyde_sandbox::capability::resolve_program(config.sandbox.bwrap.as_deref(), "bwrap")
    {
        registry = registry.with(Arc::new(BubblewrapBackend::new(
            bwrap,
            tools,
            paths.sandbox_runtime(),
        )));
    }
    if let Some(firecracker) = clyde_sandbox::capability::resolve_program(
        config.sandbox.firecracker.as_deref(),
        "firecracker",
    ) {
        registry = registry.with(Arc::new(FirecrackerBackend::new(FirecrackerConfig {
            firecracker,
            kernel: paths.root().join("vm/vmlinux"),
            rootfs_dir: paths.root().join("vm/rootfs"),
            runtime_dir: paths.sandbox_runtime(),
            vsock_dir: paths.run().join("vsock"),
        })));
    }
    registry
}

/// The identity commits are attributed to.
///
/// Taken from the environment rather than from git's global configuration,
/// because every git invocation Clyde makes neutralises global configuration —
/// and a commit author is an input Clyde records, not something read from a file
/// an agent could have written.
fn commit_identity() -> Option<clyde_git::Identity> {
    let name = std::env::var("CLYDE_GIT_AUTHOR_NAME")
        .or_else(|_| std::env::var("GIT_AUTHOR_NAME"))
        .ok()?;
    let email = std::env::var("CLYDE_GIT_AUTHOR_EMAIL")
        .or_else(|_| std::env::var("GIT_AUTHOR_EMAIL"))
        .ok()?;
    clyde_git::Identity::new(name, email).ok()
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
    use clyde_store::MemoryStore;

    fn options(root: PathBuf) -> DaemonOptions {
        DaemonOptions {
            state_root: root,
            config_paths: ConfigPaths {
                host: PathBuf::from("/nonexistent/host.toml"),
                user: None,
            },
            backends: Some(BackendRegistry::new()),
            host_report: None,
        }
    }

    #[test]
    fn a_daemon_builds_its_state_layout_and_ca() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(
            options(dir.path().join("state")),
            Arc::new(MemoryStore::new()),
        )
        .expect("the daemon builds");
        assert!(daemon.paths.snapshots().is_dir());
        assert!(daemon.paths.deps().is_dir());
        assert!(daemon.ca.certificate_path().exists());
        assert!(
            !format!("{daemon:?}").contains("PRIVATE"),
            "the daemon's Debug must not reveal key material"
        );
    }

    #[test]
    fn a_workspace_with_widening_configuration_fails_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(
            options(dir.path().join("state")),
            Arc::new(MemoryStore::new()),
        )
        .unwrap();
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(workspace.join(".clyde")).unwrap();
        std::fs::write(
            workspace.join(".clyde/policy.toml"),
            "[agent]\ncommand = \"/tmp/evil\"\n",
        )
        .unwrap();
        assert!(daemon.config_for(&workspace).is_err());
        // The attempt is recorded even though it was refused.
        let loads = daemon.store.list_config_loads(None).unwrap();
        assert!(loads.iter().any(|load| !load.rejected_keys.is_empty()));
    }

    #[test]
    fn an_empty_backend_registry_reports_no_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(
            options(dir.path().join("state")),
            Arc::new(MemoryStore::new()),
        )
        .unwrap();
        assert!(daemon.backends.is_empty());
        assert!(
            !daemon
                .backends
                .has(clyde_core::classification::BackendKind::TestOnly)
        );
    }
}
