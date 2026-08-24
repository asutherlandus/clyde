//! Registered project directories.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::error::ValidationError;
use crate::ids::WorkspaceId;

/// Version control system backing a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VcsKind {
    Git {
        default_remote: Option<String>,
        default_branch: Option<String>,
    },
    None,
}

/// A registered workspace.
///
/// At most one mission in a non-terminal state exists per workspace (D16). That
/// invariant is enforced by the store, which is the only place that can see all
/// missions at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Absolute host path. Never rendered into an actor-facing response
    /// (schema reference: wire representation rule 2).
    pub root: PathBuf,
    pub vcs: VcsKind,
    pub registered_at: DateTime<Utc>,
    /// Digest of `.clyde/policy.toml` as last loaded, or `None` if absent.
    pub policy_digest: Option<Digest>,
}

impl Workspace {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.root.is_absolute() {
            return Err(ValidationError::InvalidRepoPathComponent {
                path: self.root.display().to_string(),
                reason: "workspace root must be an absolute host path",
            });
        }
        Ok(())
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
    use super::*;
    use crate::ids;

    fn workspace(root: &str) -> Workspace {
        Workspace {
            id: ids::new::workspace_id().unwrap(),
            root: PathBuf::from(root),
            vcs: VcsKind::Git {
                default_remote: Some("origin".to_owned()),
                default_branch: Some("main".to_owned()),
            },
            registered_at: Utc::now(),
            policy_digest: None,
        }
    }

    #[test]
    fn requires_an_absolute_root() {
        assert!(workspace("/srv/project").validate().is_ok());
        assert!(workspace("project").validate().is_err());
    }

    #[test]
    fn serde_round_trips() {
        let original = workspace("/srv/project");
        let json = serde_json::to_string(&original).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }
}
