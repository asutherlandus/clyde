//! Sandbox errors.

use std::path::PathBuf;

use clyde_core::classification::IsolationLevel;

use crate::spec::SpecViolation;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox specification is invalid: {0}")]
    Spec(#[from] SpecViolation),

    /// The backend cannot honour something the spec requires.
    ///
    /// Always a refusal, never a relaxation: a backend that quietly dropped a
    /// requirement would make the approval prompt's claims untrue.
    #[error("{backend} cannot honour this specification: {requirement}")]
    Unsupported {
        backend: &'static str,
        requirement: String,
    },

    #[error(
        "this host cannot provide {required} isolation: {detail}. The task is refused rather than run at a weaker boundary"
    )]
    IsolationUnavailable {
        required: IsolationLevel,
        detail: String,
    },

    #[error(
        "cgroup v2 delegation is unavailable, and build tasks are refused without it: {detail}"
    )]
    CgroupDelegationUnavailable { detail: String },

    #[error(
        "required program {program:?} was not found; check the flake devShell or configuration"
    )]
    ProgramNotFound { program: PathBuf },

    #[error("sandbox {id} could not be started: {source}")]
    Spawn {
        id: String,
        #[source]
        source: std::io::Error,
    },

    #[error("sandbox {id} is no longer running, so this operation has nothing to act on")]
    NotRunning { id: String },

    #[error("input/output error in the sandbox layer: {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("sandbox {id} exceeded its {seconds}s wall-clock limit and was terminated")]
    WallClockExceeded { id: String, seconds: u64 },

    #[error("runtime root {root:?} is not usable: {detail}")]
    RuntimeRoot { root: PathBuf, detail: String },
}

impl SandboxError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Whether this error means the host is at fault rather than the task.
    ///
    /// Task outcomes classify these as `SandboxFailure`, so "Clyde is broken" is
    /// never reported as "your code is broken".
    pub fn is_host_fault(&self) -> bool {
        !matches!(self, Self::WallClockExceeded { .. })
    }
}

pub type Result<T> = std::result::Result<T, SandboxError>;
