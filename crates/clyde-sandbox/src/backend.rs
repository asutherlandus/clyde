//! The `SandboxBackend` trait and its handle types.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use clyde_core::classification::{BackendKind, IsolationLevel};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::spec::SandboxSpec;

/// Boxed future, so the trait stays usable behind `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How a sandbox finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: Option<i32>,
    /// Set when the daemon's wall-clock bound fired.
    pub timed_out: bool,
    /// Set when the process was killed by a signal, which on a cgroup-limited
    /// sandbox usually means the memory limit was hit.
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && self.signal.is_none()
    }

    /// Whether the outcome looks like a resource limit firing.
    ///
    /// A cgroup OOM kill arrives as SIGKILL, which is indistinguishable at this
    /// layer from any other SIGKILL; the classifier treats both as resource
    /// exhaustion, which is the safer reading for a build sandbox.
    pub fn resource_exhausted(&self) -> bool {
        self.timed_out || self.signal == Some(9)
    }
}

/// A running sandbox.
///
/// Cloneable and interior-mutable so the task engine can hold one reference for
/// waiting and another for termination without threading `&mut` through the
/// call graph.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub id: String,
    pub backend: BackendKind,
    pub started_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    /// Host process identifier, where the backend has one.
    pub pid: Option<u32>,
    child: Arc<Mutex<Option<tokio::process::Child>>>,
}

impl SandboxHandle {
    pub(crate) fn new(
        id: String,
        backend: BackendKind,
        started_at: DateTime<Utc>,
        deadline: DateTime<Utc>,
        child: tokio::process::Child,
    ) -> Self {
        let pid = child.id();
        Self {
            id,
            backend,
            started_at,
            deadline,
            pid,
            child: Arc::new(Mutex::new(Some(child))),
        }
    }

    pub(crate) fn child(&self) -> Arc<Mutex<Option<tokio::process::Child>>> {
        Arc::clone(&self.child)
    }

    /// Whether the process has already been reaped.
    pub async fn is_finished(&self) -> bool {
        self.child.lock().await.is_none()
    }
}

/// A sandbox implementation.
///
/// Anything a backend cannot honour is a `preflight` failure, never a silent
/// relaxation. That rule is what keeps this trait from becoming the place where
/// boundaries quietly weaken (Phase 2a deliverable 1).
pub trait SandboxBackend: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> BackendKind;

    /// The isolation level this backend provides.
    fn isolation_level(&self) -> IsolationLevel;

    /// Checks whether this backend can honour the spec, without starting
    /// anything.
    fn preflight(&self, spec: &SandboxSpec) -> Result<()>;

    fn start<'a>(&'a self, spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>>;

    /// Waits for the sandbox, enforcing the handle's deadline.
    fn wait<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<ExitStatus>>;

    fn terminate<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<()>>;
}

/// Waits for a child with a deadline, terminating it if the deadline passes.
///
/// Shared by the backends, because "the wall clock is enforced by the daemon" is
/// a property of the system rather than of one backend.
pub(crate) async fn wait_with_deadline(
    id: &str,
    deadline: DateTime<Utc>,
    child: Arc<Mutex<Option<tokio::process::Child>>>,
) -> Result<ExitStatus> {
    use std::os::unix::process::ExitStatusExt as _;

    let mut guard = child.lock().await;
    let Some(process) = guard.as_mut() else {
        return Err(crate::error::SandboxError::NotRunning { id: id.to_owned() });
    };
    let remaining = (deadline - Utc::now()).to_std().unwrap_or_default();
    let status = match tokio::time::timeout(remaining, process.wait()).await {
        Ok(result) => {
            let status = result
                .map_err(|error| crate::error::SandboxError::io("waiting for sandbox", error))?;
            *guard = None;
            ExitStatus {
                code: status.code(),
                timed_out: false,
                signal: status.signal(),
            }
        }
        Err(_elapsed) => {
            // `--die-with-parent` covers a daemon crash; this covers a task that
            // simply runs too long.
            let _ = process.kill().await;
            let _ = process.wait().await;
            *guard = None;
            ExitStatus {
                code: None,
                timed_out: true,
                signal: Some(9),
            }
        }
    };
    Ok(status)
}

/// Terminates a running child, if it is still running.
pub(crate) async fn terminate_child(
    child: Arc<Mutex<Option<tokio::process::Child>>>,
) -> Result<()> {
    let mut guard = child.lock().await;
    if let Some(process) = guard.as_mut() {
        let _ = process.kill().await;
        let _ = process.wait().await;
        *guard = None;
    }
    Ok(())
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

    #[test]
    fn success_requires_a_clean_exit() {
        let ok = ExitStatus {
            code: Some(0),
            timed_out: false,
            signal: None,
        };
        assert!(ok.success());
        assert!(!ok.resource_exhausted());

        let failed = ExitStatus {
            code: Some(1),
            timed_out: false,
            signal: None,
        };
        assert!(!failed.success());
        assert!(!failed.resource_exhausted());

        let timed_out = ExitStatus {
            code: None,
            timed_out: true,
            signal: Some(9),
        };
        assert!(!timed_out.success());
        assert!(timed_out.resource_exhausted());

        let killed = ExitStatus {
            code: None,
            timed_out: false,
            signal: Some(9),
        };
        assert!(
            killed.resource_exhausted(),
            "an OOM kill arrives as SIGKILL"
        );
    }

    #[test]
    fn a_zero_exit_with_a_signal_is_not_success() {
        let odd = ExitStatus {
            code: Some(0),
            timed_out: false,
            signal: Some(15),
        };
        assert!(!odd.success());
    }
}
