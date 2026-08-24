//! Dependency bundle records.
//!
//! Kept separate from [`crate::types`] because a bundle is the one store entity
//! that is not in the schema reference's entity list: it is the artifact plus the
//! lockfile digest it satisfies, which is what makes "what were the inputs to
//! this build" answerable from a task run alone (Phase 3 deliverable 2).

use chrono::{DateTime, Utc};
use clyde_core::Digest;
use clyde_core::baseline::CodeExecInventory;
use clyde_core::ids::ArtifactId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRecord {
    pub artifact: ArtifactId,
    /// The `Cargo.lock` digest this bundle satisfies.
    pub lockfile_digest: Digest,
    /// The lockfile itself, reduced to what the fetch policy needs.
    ///
    /// Kept so the next fetch can be classified against the previously satisfied
    /// lockfile. Without it, "four new crates from a source you have not used
    /// before" is not a distinction Clyde could make.
    pub lockfile: clyde_policy::access::LockfileSummary,
    /// Content-addressed root of the bundle in the read-only bundle store.
    pub content_ref: std::path::PathBuf,
    pub crate_count: u32,
    pub inventory: CodeExecInventory,
    pub created_at: DateTime<Utc>,
    /// Registries the crates came from, for mission review.
    pub registries: Vec<String>,
}
