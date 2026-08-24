//! Artifact storage.
//!
//! Artifacts are content-addressed in the blob store and read back through the
//! API, never through a mounted store, so an agent cannot read another mission's
//! outputs by walking a filesystem.

use std::path::{Path, PathBuf};

use chrono::Utc;
use clyde_core::Digest;
use clyde_core::artifact::{Artifact, ArtifactKind};
use clyde_core::audit::AuditEventKind;
use clyde_core::classification::TrustClass;
use clyde_core::ids::{ArtifactId, MissionId, TaskRunId};
use clyde_store::Store;

use crate::audit;
use crate::error::{DaemonError, Result};

/// Largest log or artifact retained inline.
///
/// Logs are bounded in size with truncation recorded rather than silent
/// (Phase 2a deliverable 7).
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

/// Stores bytes as an artifact.
pub fn store_bytes(
    store: &dyn Store,
    blobs: &Path,
    mission: &MissionId,
    kind: ArtifactKind,
    trust_class: TrustClass,
    produced_by: Option<TaskRunId>,
    bytes: &[u8],
) -> Result<Artifact> {
    let (bytes, truncated) = if bytes.len() > MAX_ARTIFACT_BYTES {
        let mut cut = MAX_ARTIFACT_BYTES;
        while cut > 0 && !bytes.is_char_boundary_at(cut) {
            cut -= 1;
        }
        (bytes.get(..cut).unwrap_or(&[]), true)
    } else {
        (bytes, false)
    };

    let digest = Digest::of_bytes(bytes);
    let id = ArtifactId::parse(format!("a-blake3:{digest}"))?;
    let path = blob_path(blobs, &digest);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| DaemonError::io("creating a blob directory", error))?;
    }
    if !path.exists() {
        std::fs::write(&path, bytes)
            .map_err(|error| DaemonError::io("writing an artifact", error))?;
    }

    let artifact = Artifact {
        id: id.clone(),
        kind,
        produced_by,
        mission: mission.clone(),
        trust_class,
        size_bytes: bytes.len() as u64,
        blake3: digest,
        content_ref: path,
        created_at: Utc::now(),
        retain_until: None,
    };
    store.insert_artifact(artifact.clone())?;
    audit::record(
        store,
        audit::draft(
            AuditEventKind::ArtifactStored,
            serde_json::json!({
                "kind": format!("{kind:?}"),
                "bytes": artifact.size_bytes,
                "truncated": truncated,
                "trust_class": trust_class.to_string(),
            }),
        )
        .mission(mission.clone())
        .artifacts(vec![id]),
    );
    Ok(artifact)
}

/// Reads an artifact's content.
pub fn read(store: &dyn Store, id: &ArtifactId) -> Result<Vec<u8>> {
    let artifact = store.get_artifact(id)?;
    std::fs::read(&artifact.content_ref)
        .map_err(|error| DaemonError::io("reading an artifact", error))
}

fn blob_path(blobs: &Path, digest: &Digest) -> PathBuf {
    let hex = digest.as_str();
    let shard = hex.get(..2).unwrap_or("00");
    blobs.join(shard).join(hex)
}

/// A helper for truncating on a character boundary when the bytes happen to be
/// text, which logs are.
trait CharBoundary {
    fn is_char_boundary_at(&self, index: usize) -> bool;
}

impl CharBoundary for [u8] {
    fn is_char_boundary_at(&self, index: usize) -> bool {
        match self.get(index) {
            // A continuation byte is not a boundary; anything else is.
            Some(byte) => (byte & 0xC0) != 0x80,
            None => true,
        }
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
    use clyde_store::MemoryStore;

    #[test]
    fn storing_is_content_addressed_and_readable_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new();
        let mission = clyde_core::ids::new::mission_id().unwrap();
        let artifact = store_bytes(
            &store,
            &dir.path().join("blobs"),
            &mission,
            ArtifactKind::Log,
            TrustClass::T2,
            None,
            b"build output",
        )
        .unwrap();
        assert_eq!(artifact.size_bytes, 12);
        assert_eq!(read(&store, &artifact.id).unwrap(), b"build output");
        assert!(artifact.from_untrusted_execution());
    }

    #[test]
    fn an_oversized_artifact_is_truncated_and_the_truncation_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new();
        let mission = clyde_core::ids::new::mission_id().unwrap();
        let huge = vec![b'x'; MAX_ARTIFACT_BYTES + 100];
        let artifact = store_bytes(
            &store,
            &dir.path().join("blobs"),
            &mission,
            ArtifactKind::Log,
            TrustClass::T2,
            None,
            &huge,
        )
        .unwrap();
        assert!(artifact.size_bytes as usize <= MAX_ARTIFACT_BYTES);
        let events = store
            .list_audit(&clyde_store::AuditFilter::default())
            .unwrap();
        assert_eq!(
            events[0].payload["truncated"], true,
            "truncation is recorded, not silent"
        );
    }

    #[test]
    fn identical_content_is_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new();
        let mission = clyde_core::ids::new::mission_id().unwrap();
        let blobs = dir.path().join("blobs");
        let first = store_bytes(
            &store,
            &blobs,
            &mission,
            ArtifactKind::Log,
            TrustClass::T1,
            None,
            b"same",
        )
        .unwrap();
        let second = store_bytes(
            &store,
            &blobs,
            &mission,
            ArtifactKind::Diff,
            TrustClass::T1,
            None,
            b"same",
        )
        .unwrap();
        assert_eq!(first.content_ref, second.content_ref);
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        let bytes = "é".repeat(10).into_bytes();
        assert!(bytes.is_char_boundary_at(0));
        assert!(
            !bytes.is_char_boundary_at(1),
            "a continuation byte is not a boundary"
        );
        assert!(bytes.is_char_boundary_at(2));
    }
}
