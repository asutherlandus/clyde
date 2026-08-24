//! A backend that runs a command with **no isolation at all**.
//!
//! It exists so integration tests can exercise the whole pipeline — admission,
//! snapshot, execution, classification, artifacts, audit — on a host where
//! unprivileged user namespaces are unavailable, which is the common case in
//! containers and on a default Ubuntu 24.04 install.
//!
//! It is gated behind the `test-backend` feature, which no shipped binary
//! enables, and it reports [`BackendKind::TestOnly`] so a spec that ran on it is
//! identifiable in the audit record. The daemon's own registry construction
//! never adds it: a test must inject it explicitly.
//!
//! **This backend provides no security boundary.** Anything asserted while using
//! it is a statement about the pipeline, never about isolation.

use std::process::Stdio;

use chrono::Utc;
use clyde_core::classification::{BackendKind, IsolationLevel};

use crate::backend::{
    BoxFuture, ExitStatus, SandboxBackend, SandboxHandle, terminate_child, wait_with_deadline,
};
use crate::error::{Result, SandboxError};
use crate::spec::SandboxSpec;

/// A no-isolation backend for tests.
#[derive(Debug, Clone, Default)]
pub struct TestBackend {
    /// The isolation level to claim, so a test can exercise selection logic.
    claimed: Option<IsolationLevel>,
}

impl TestBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims a specific isolation level, for selection tests.
    pub fn claiming(level: IsolationLevel) -> Self {
        Self {
            claimed: Some(level),
        }
    }
}

impl SandboxBackend for TestBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::TestOnly
    }

    fn isolation_level(&self) -> IsolationLevel {
        self.claimed.unwrap_or(IsolationLevel::NamespaceSandbox)
    }

    fn preflight(&self, spec: &SandboxSpec) -> Result<()> {
        // The spec's structural rules still apply: a test must not be able to
        // assert a property of a spec this backend would have refused.
        spec.validate()?;
        Ok(())
    }

    fn start<'a>(&'a self, spec: SandboxSpec) -> BoxFuture<'a, Result<SandboxHandle>> {
        Box::pin(async move {
            self.preflight(&spec)?;
            let Some((program, arguments)) = spec.argv.split_first() else {
                return Err(SandboxError::Spec(crate::spec::SpecViolation::EmptyArgv));
            };
            let mut command = tokio::process::Command::new(program);
            command.args(arguments);
            command.env_clear();
            // With no mount namespace, an in-sandbox path in the environment
            // names nothing. Each mount target is rewritten to its host source so
            // the command sees a coherent filesystem; a real backend needs none
            // of this, which is part of why this one is not a boundary.
            for (key, value) in &spec.env {
                command.env(key, translate(value, &spec));
            }
            command.kill_on_drop(true);
            command.stdin(Stdio::null());
            // The working directory is the snapshot mount's *host* path, since
            // there is no mount namespace here to make the in-sandbox path real.
            if let Some(source) = spec
                .mounts
                .iter()
                .find(|mount| mount.target == spec.cwd)
                .and_then(|mount| mount.source.clone())
            {
                command.current_dir(source);
            }
            command.stdout(open(spec.stdout_path.as_deref())?);
            command.stderr(open(spec.stderr_path.as_deref())?);

            let started_at = Utc::now();
            let deadline = started_at
                + chrono::Duration::from_std(spec.limits.max_wall_clock.as_duration())
                    .unwrap_or_else(|_| chrono::Duration::minutes(5));
            let child = command.spawn().map_err(|error| SandboxError::Spawn {
                id: spec.id.clone(),
                source: error,
            })?;
            Ok(SandboxHandle::new(
                spec.id,
                BackendKind::TestOnly,
                started_at,
                deadline,
                child,
            ))
        })
    }

    fn wait<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<ExitStatus>> {
        Box::pin(
            async move { wait_with_deadline(&handle.id, handle.deadline, handle.child()).await },
        )
    }

    fn terminate<'a>(&'a self, handle: &'a SandboxHandle) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { terminate_child(handle.child()).await })
    }
}

fn open(path: Option<&std::path::Path>) -> Result<Stdio> {
    match path {
        None => Ok(Stdio::null()),
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| SandboxError::io("creating a log directory", error))?;
            }
            let file = std::fs::File::create(path)
                .map_err(|error| SandboxError::io("creating a log file", error))?;
            Ok(Stdio::from(file))
        }
    }
}

/// Rewrites an in-sandbox path to its host source.
///
/// Only the test backend needs this: a real backend gives the sandbox a mount
/// namespace in which the in-sandbox path is the real one.
fn translate(value: &str, spec: &SandboxSpec) -> String {
    for mount in &spec.mounts {
        let Some(source) = mount.source.as_ref() else {
            continue;
        };
        let target = mount.target.to_string_lossy();
        if value.starts_with(target.as_ref()) {
            // The mount source must exist for the command to use it, and a
            // writable mount's directory is created here because there is no
            // sandbox setup step to create it.
            if mount.mode.is_writable() {
                let _ = std::fs::create_dir_all(source);
            }
            return value.replacen(target.as_ref(), &source.to_string_lossy(), 1);
        }
    }
    value.to_owned()
}
