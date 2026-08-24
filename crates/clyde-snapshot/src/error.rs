//! Snapshot errors.

use std::path::PathBuf;

use clyde_core::RepoPath;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("input/output error: {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is outside the workspace root")]
    OutsideWorkspace { path: String },

    #[error("{path:?} could not be parsed as a cargo manifest: {detail}")]
    Manifest { path: PathBuf, detail: String },

    #[error("{path:?} could not be parsed as a cargo lockfile: {detail}")]
    Lockfile { path: PathBuf, detail: String },

    #[error("no cargo manifest was found at or above {path}")]
    NoManifest { path: RepoPath },

    #[error("snapshot manifest is invalid: {0}")]
    Invalid(#[from] clyde_core::ValidationError),

    #[error("snapshot identity could not be computed: {0}")]
    Identity(#[from] clyde_core::snapshot::SnapshotIdentityError),

    #[error("the dependency bundle at {path:?} is not usable: {detail}")]
    Bundle { path: PathBuf, detail: String },

    #[error("learn-mode observation failed: {0}")]
    Observation(String),
}

impl SnapshotError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, SnapshotError>;
