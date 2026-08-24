//! Artifacts.
//!
//! Every artifact carries the trust class of the environment that produced it, so
//! a consumer can tell whether content came from untrusted execution without
//! inferring it from the artifact kind (schema reference: Artifact).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::entities::classification::TrustClass;
use crate::ids::{ArtifactId, MissionId, TaskRunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Log,
    BuildOutput,
    DependencyBundle,
    FetchManifest,
    Coverage,
    CommitProposal,
    Signature,
    Provenance,
    Diff,
    BaselineProposal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub produced_by: Option<TaskRunId>,
    pub mission: MissionId,
    /// Trust class of the context that produced it.
    pub trust_class: TrustClass,
    pub size_bytes: u64,
    pub blake3: Digest,
    /// Blob store path. A host path, never rendered to an actor.
    pub content_ref: PathBuf,
    pub created_at: DateTime<Utc>,
    pub retain_until: Option<DateTime<Utc>>,
}

impl Artifact {
    /// Builds the content-addressed identifier for a payload.
    pub fn id_for(digest: &Digest) -> Result<ArtifactId, crate::error::ValidationError> {
        ArtifactId::parse(format!("a-blake3:{digest}"))
    }

    /// Whether the artifact's content came from untrusted execution.
    pub fn from_untrusted_execution(&self) -> bool {
        self.trust_class >= TrustClass::T2
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

    #[test]
    fn identifier_is_derived_from_content() {
        let digest = Digest::of_bytes(b"log output");
        let id = Artifact::id_for(&digest).unwrap();
        assert_eq!(id.as_str(), format!("a-blake3:{digest}"));
    }

    #[test]
    fn trust_class_marks_untrusted_provenance() {
        let artifact = |trust_class| Artifact {
            id: Artifact::id_for(&Digest::of_bytes(b"x")).unwrap(),
            kind: ArtifactKind::Log,
            produced_by: None,
            mission: ids::new::mission_id().unwrap(),
            trust_class,
            size_bytes: 1,
            blake3: Digest::of_bytes(b"x"),
            content_ref: PathBuf::from("/var/lib/clyde/blobs/x"),
            created_at: Utc::now(),
            retain_until: None,
        };
        assert!(artifact(TrustClass::T2).from_untrusted_execution());
        assert!(artifact(TrustClass::T3).from_untrusted_execution());
        assert!(!artifact(TrustClass::T1).from_untrusted_execution());
    }
}
