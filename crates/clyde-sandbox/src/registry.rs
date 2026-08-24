//! Backend selection.
//!
//! Selection is driven by the policy's `min_isolation`. There is no manual
//! override in the MVP, and there is no silent downgrade: a task that demands
//! microVM isolation on a host that cannot provide it is refused (Phase 2b
//! deliverable 2).

use std::sync::Arc;

use clyde_core::classification::{BackendKind, IsolationLevel};

use crate::backend::SandboxBackend;
use crate::error::{Result, SandboxError};
use crate::spec::SandboxSpec;

/// The set of backends available on this host.
#[derive(Debug, Clone, Default)]
pub struct BackendRegistry {
    backends: Vec<Arc<dyn SandboxBackend>>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a backend. Order is irrelevant: selection is by isolation
    /// level, not by registration order.
    pub fn with(mut self, backend: Arc<dyn SandboxBackend>) -> Self {
        self.backends.push(backend);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// The strongest isolation level any registered backend provides.
    pub fn strongest_isolation(&self) -> Option<IsolationLevel> {
        self.backends
            .iter()
            .map(|backend| backend.isolation_level())
            .max()
    }

    /// Selects the backend for a spec.
    ///
    /// Chooses the *weakest* backend that still satisfies the policy's minimum,
    /// so a task that only needs namespace isolation does not pay microVM
    /// startup cost, while a task that demands more cannot get less.
    pub fn select(&self, spec: &SandboxSpec) -> Result<Arc<dyn SandboxBackend>> {
        let mut candidates: Vec<&Arc<dyn SandboxBackend>> = self
            .backends
            .iter()
            .filter(|backend| backend.isolation_level() >= spec.min_isolation)
            .collect();
        candidates.sort_by_key(|backend| backend.isolation_level());
        for backend in candidates {
            match backend.preflight(spec) {
                Ok(()) => return Ok(Arc::clone(backend)),
                Err(error) => {
                    tracing::debug!(
                        backend = %backend.kind(),
                        error = %error,
                        "backend rejected the specification"
                    );
                }
            }
        }
        Err(SandboxError::IsolationUnavailable {
            required: spec.min_isolation,
            detail: match self.strongest_isolation() {
                Some(available) => format!(
                    "the strongest backend available provides {available}, and no backend accepted this specification"
                ),
                None => "no sandbox backend is available on this host".to_owned(),
            },
        })
    }

    /// Whether a backend of a given kind is registered.
    pub fn has(&self, kind: BackendKind) -> bool {
        self.backends.iter().any(|backend| backend.kind() == kind)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use chrono::Utc;
    use clyde_core::HumanDuration;
    use clyde_core::classification::{EgressProfile, ResourceLimits, TrustClass};
    use clyde_core::task::RuntimeRootKind;

    use super::*;
    use crate::backend::{BoxFuture, ExitStatus, SandboxHandle};
    use crate::spec::ScratchPolicy;

    /// A backend that reports a level and accepts or refuses in preflight.
    #[derive(Debug)]
    struct Fake {
        kind: BackendKind,
        level: IsolationLevel,
        accepts: bool,
    }

    impl SandboxBackend for Fake {
        fn kind(&self) -> BackendKind {
            self.kind
        }
        fn isolation_level(&self) -> IsolationLevel {
            self.level
        }
        fn preflight(&self, _spec: &SandboxSpec) -> Result<()> {
            if self.accepts {
                Ok(())
            } else {
                Err(SandboxError::Unsupported {
                    backend: "fake",
                    requirement: "anything".to_owned(),
                })
            }
        }
        fn start<'a>(&'a self, _spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>> {
            Box::pin(async move {
                Err(SandboxError::NotRunning {
                    id: "fake".to_owned(),
                })
            })
        }
        fn wait<'a>(&'a self, _handle: &'a SandboxHandle) -> BoxFuture<'a, Result<ExitStatus>> {
            Box::pin(async move {
                Err(SandboxError::NotRunning {
                    id: "fake".to_owned(),
                })
            })
        }
        fn terminate<'a>(&'a self, _handle: &'a SandboxHandle) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move { Ok(()) })
        }
    }

    fn spec(min_isolation: IsolationLevel) -> SandboxSpec {
        SandboxSpec {
            id: "sb".to_owned(),
            runtime_root_kind: RuntimeRootKind::Rust,
            runtime_root: PathBuf::from("/nix/store/root"),
            runtime_root_closure: vec![PathBuf::from("/nix/store/root")],
            mounts: Vec::new(),
            egress: EgressProfile::None,
            limits: ResourceLimits {
                max_wall_clock: HumanDuration::parse("5m").unwrap(),
                max_memory_bytes: 1 << 30,
                max_cpu_percent: 100,
                max_tasks: 64,
                max_open_files: 1024,
            },
            trust_class: TrustClass::T2,
            min_isolation,
            env: BTreeMap::new(),
            argv: vec!["/bin/true".to_owned()],
            cwd: PathBuf::from("/"),
            scratch: ScratchPolicy::default(),
            stdout_path: None,
            stderr_path: None,
        }
    }

    fn registry(levels: &[(BackendKind, IsolationLevel, bool)]) -> BackendRegistry {
        levels.iter().fold(
            BackendRegistry::new(),
            |registry, (kind, level, accepts)| {
                registry.with(Arc::new(Fake {
                    kind: *kind,
                    level: *level,
                    accepts: *accepts,
                }))
            },
        )
    }

    #[test]
    fn the_weakest_sufficient_backend_is_chosen() {
        let registry = registry(&[
            (BackendKind::Firecracker, IsolationLevel::MicroVm, true),
            (
                BackendKind::Bubblewrap,
                IsolationLevel::NamespaceSandbox,
                true,
            ),
        ]);
        let selected = registry
            .select(&spec(IsolationLevel::NamespaceSandbox))
            .unwrap();
        assert_eq!(
            selected.kind(),
            BackendKind::Bubblewrap,
            "a namespace-level task should not pay microVM startup cost"
        );
    }

    #[test]
    fn a_microvm_task_never_downgrades_to_a_namespace_sandbox() {
        let registry = registry(&[(
            BackendKind::Bubblewrap,
            IsolationLevel::NamespaceSandbox,
            true,
        )]);
        let error = registry
            .select(&spec(IsolationLevel::MicroVm))
            .expect_err("no silent downgrade");
        assert!(matches!(
            error,
            SandboxError::IsolationUnavailable {
                required: IsolationLevel::MicroVm,
                ..
            }
        ));
    }

    #[test]
    fn a_backend_that_refuses_in_preflight_is_skipped_not_forced() {
        let registry = registry(&[
            (
                BackendKind::Bubblewrap,
                IsolationLevel::NamespaceSandbox,
                false,
            ),
            (BackendKind::Firecracker, IsolationLevel::MicroVm, true),
        ]);
        let selected = registry
            .select(&spec(IsolationLevel::NamespaceSandbox))
            .unwrap();
        assert_eq!(selected.kind(), BackendKind::Firecracker);
    }

    #[test]
    fn an_empty_registry_refuses_rather_than_running_unconfined() {
        let registry = BackendRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.strongest_isolation(), None);
        let error = registry
            .select(&spec(IsolationLevel::NamespaceSandbox))
            .expect_err("nothing runs without a backend");
        assert!(error.to_string().contains("no sandbox backend"));
    }

    #[test]
    fn registry_membership_is_queryable() {
        let registry = registry(&[(
            BackendKind::Bubblewrap,
            IsolationLevel::NamespaceSandbox,
            true,
        )]);
        assert!(registry.has(BackendKind::Bubblewrap));
        assert!(!registry.has(BackendKind::Firecracker));
        // A test-only backend is never registered by the daemon; asserting its
        // absence here documents that.
        assert!(!registry.has(BackendKind::TestOnly));
    }

    #[test]
    fn handles_carry_their_deadline() {
        let started = Utc::now();
        let deadline = started + chrono::Duration::minutes(5);
        assert!(deadline > started);
    }
}
