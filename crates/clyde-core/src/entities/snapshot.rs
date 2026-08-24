//! Snapshots: the immutable input surface for build and test tasks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::{CanonicalError, Digest};
use crate::error::ValidationError;
use crate::ids::{MissionId, SnapshotId, WorkspaceId};
use crate::repo_path::RepoPath;

/// How a snapshot tree was materialised.
///
/// Hardlink materialisation shares inodes with the content store, which is why
/// snapshots are *always* bound read-only: a writable bind would corrupt the
/// store (schema reference: Snapshot invariants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialisationKind {
    Hardlink,
    Reflink,
    Copy,
}

/// One file in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub path: RepoPath,
    /// Unix mode bits, as stored.
    pub mode: u32,
    pub size: u64,
    pub blake3: Digest,
}

/// The manifest a snapshot's identity is computed over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub entries: Vec<SnapshotEntry>,
    /// Build-closure paths admitted beyond the requested path.
    pub closure_paths: Vec<RepoPath>,
    /// Paths admitted read-only from outside the lease's edit scope, recorded so
    /// mission review can show the confidentiality delta.
    pub out_of_lease_paths: Vec<RepoPath>,
    /// Exclusion rules that fired, by name.
    pub exclusions_applied: Vec<String>,
}

impl SnapshotManifest {
    /// Canonicalises the manifest and computes the snapshot identity.
    ///
    /// Entries are sorted so that two traversals of the same tree in different
    /// directory orders produce one identity.
    pub fn identity(&self) -> Result<SnapshotId, SnapshotIdentityError> {
        let mut canonical = self.clone();
        canonical.entries.sort_by(|a, b| a.path.cmp(&b.path));
        canonical.closure_paths.sort();
        canonical.closure_paths.dedup();
        canonical.out_of_lease_paths.sort();
        canonical.out_of_lease_paths.dedup();
        canonical.exclusions_applied.sort();
        canonical.exclusions_applied.dedup();
        let digest = Digest::of_canonical("clyde.snapshot-manifest.v1", &canonical)?;
        SnapshotId::parse(format!("s-blake3:{digest}")).map_err(SnapshotIdentityError::Id)
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |acc, entry| acc.saturating_add(entry.size))
    }

    /// Whether the snapshot contains `path`.
    ///
    /// This is the enforcement question in the form the daemon asks it: path
    /// enforcement is by materialisation, so "is it in the manifest" and "can the
    /// task read it" are the same question (D18).
    pub fn contains(&self, path: &RepoPath) -> bool {
        self.entries.iter().any(|entry| &entry.path == path)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        // A snapshot with duplicate paths would have an ambiguous materialisation.
        let mut paths: Vec<&RepoPath> = self.entries.iter().map(|entry| &entry.path).collect();
        paths.sort();
        let before = paths.len();
        paths.dedup();
        if paths.len() != before {
            return Err(ValidationError::InvalidRepoPathComponent {
                path: "<manifest>".to_owned(),
                reason: "contains duplicate entries",
            });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotIdentityError {
    #[error("snapshot manifest is not canonically encodable: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("computed snapshot identifier is malformed: {0}")]
    Id(ValidationError),
}

/// A snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub workspace: WorkspaceId,
    pub mission: MissionId,
    pub requested_path: RepoPath,
    pub manifest: SnapshotManifest,
    pub created_at: DateTime<Utc>,
    pub materialisation: MaterialisationKind,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn entry(path: &str, content: &[u8]) -> SnapshotEntry {
        SnapshotEntry {
            path: RepoPath::parse(path).unwrap(),
            mode: 0o644,
            size: content.len() as u64,
            blake3: Digest::of_bytes(content),
        }
    }

    #[test]
    fn identity_is_order_independent() {
        let a = SnapshotManifest {
            entries: vec![entry("a.rs", b"a"), entry("b.rs", b"b")],
            ..SnapshotManifest::default()
        };
        let b = SnapshotManifest {
            entries: vec![entry("b.rs", b"b"), entry("a.rs", b"a")],
            ..SnapshotManifest::default()
        };
        assert_eq!(a.identity().unwrap(), b.identity().unwrap());
    }

    #[test]
    fn identity_changes_with_content() {
        let a = SnapshotManifest {
            entries: vec![entry("a.rs", b"one")],
            ..SnapshotManifest::default()
        };
        let b = SnapshotManifest {
            entries: vec![entry("a.rs", b"two")],
            ..SnapshotManifest::default()
        };
        assert_ne!(a.identity().unwrap(), b.identity().unwrap());
    }

    #[test]
    fn identity_changes_when_exclusions_change() {
        // Exclusions are applied before hashing, so the snapshot id reflects
        // exactly what the task can see.
        let a = SnapshotManifest {
            entries: vec![entry("a.rs", b"one")],
            exclusions_applied: vec![".env".to_owned()],
            ..SnapshotManifest::default()
        };
        let b = SnapshotManifest {
            entries: vec![entry("a.rs", b"one")],
            ..SnapshotManifest::default()
        };
        assert_ne!(a.identity().unwrap(), b.identity().unwrap());
    }

    #[test]
    fn duplicate_entries_are_rejected() {
        let manifest = SnapshotManifest {
            entries: vec![entry("a.rs", b"one"), entry("a.rs", b"two")],
            ..SnapshotManifest::default()
        };
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn containment_answers_the_enforcement_question() {
        let manifest = SnapshotManifest {
            entries: vec![entry("src/lib.rs", b"x")],
            ..SnapshotManifest::default()
        };
        assert!(manifest.contains(&RepoPath::parse("src/lib.rs").unwrap()));
        assert!(!manifest.contains(&RepoPath::parse("src/secret.rs").unwrap()));
    }

    #[test]
    fn total_bytes_saturates() {
        let manifest = SnapshotManifest {
            entries: vec![
                SnapshotEntry {
                    path: RepoPath::parse("a").unwrap(),
                    mode: 0o644,
                    size: u64::MAX,
                    blake3: Digest::of_bytes(b""),
                },
                entry("b", b"b"),
            ],
            ..SnapshotManifest::default()
        };
        assert_eq!(manifest.total_bytes(), u64::MAX);
    }
}
