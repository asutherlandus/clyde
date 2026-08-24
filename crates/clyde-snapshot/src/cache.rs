//! Per-mission build caches (D3).
//!
//! A cold `target/` per task run makes the inner loop unusable for anything but
//! toy crates; a long-lived per-project cache is the cache-poisoning persistence
//! vector in the threat model. Per-mission scoping keeps the loop warm within the
//! unit of work a human actually approved, and makes the blast radius of a
//! hostile `build.rs` the mission it ran in.
//!
//! One cold build per mission is accepted, and should be surfaced in the UX as
//! such.

use std::path::{Path, PathBuf};

use clyde_core::ids::MissionId;

use crate::content::directory_size;
use crate::error::{Result, SnapshotError};

/// A mission's writable cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionCache {
    root: PathBuf,
}

impl MissionCache {
    /// Creates the cache for a mission, at activation.
    pub fn create(missions_root: &Path, mission: &MissionId) -> Result<Self> {
        let root = missions_root.join(mission.as_str());
        std::fs::create_dir_all(root.join("cargo-home"))
            .map_err(|error| SnapshotError::io("creating the mission CARGO_HOME", error))?;
        std::fs::create_dir_all(root.join("cargo-target"))
            .map_err(|error| SnapshotError::io("creating the mission target directory", error))?;
        std::fs::create_dir_all(root.join("scratch"))
            .map_err(|error| SnapshotError::io("creating the mission scratch", error))?;
        Ok(Self { root })
    }

    /// Opens an existing cache without creating it.
    pub fn existing(missions_root: &Path, mission: &MissionId) -> Option<Self> {
        let root = missions_root.join(mission.as_str());
        root.is_dir().then_some(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `CARGO_HOME`, writable because cargo writes lock files into it even
    /// offline; seeded by hardlink from the read-only dependency bundle.
    pub fn cargo_home(&self) -> PathBuf {
        self.root.join("cargo-home")
    }

    /// `CARGO_TARGET_DIR`, shared across task runs within the mission.
    pub fn target_dir(&self) -> PathBuf {
        self.root.join("cargo-target")
    }

    pub fn scratch(&self) -> PathBuf {
        self.root.join("scratch")
    }

    /// Current size, which counts against `max_cache_bytes`.
    pub fn size_bytes(&self) -> u64 {
        directory_size(&self.root)
    }

    /// Whether the cache has been warmed by at least one build.
    ///
    /// Used to tell the user which run is the cold one rather than leaving them
    /// to guess why the first build was slow.
    pub fn is_warm(&self) -> bool {
        std::fs::read_dir(self.target_dir())
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    }

    /// Destroys the cache at mission closeout.
    ///
    /// Seeded files are hardlinks into the read-only bundle store, so removing
    /// them removes links rather than shared content.
    pub fn destroy(self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)
                .map_err(|error| SnapshotError::io("removing the mission cache", error))?;
        }
        Ok(())
    }
}

/// Removes every mission cache that has no corresponding live mission.
///
/// A daemon that crashed between closeout and cleanup would otherwise leave
/// caches behind, and a cache that outlives its mission is exactly the
/// cross-mission persistence the design excludes.
pub fn remove_orphans(missions_root: &Path, live: &[MissionId]) -> Result<Vec<String>> {
    if !missions_root.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(missions_root)
        .map_err(|error| SnapshotError::io("listing mission caches", error))?;
    let mut removed = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if live.iter().any(|mission| mission.as_str() == name) {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed.push(name);
        }
    }
    Ok(removed)
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
    use clyde_core::ids;

    #[test]
    fn a_cache_has_a_writable_cargo_home_and_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mission = ids::new::mission_id().unwrap();
        let cache = MissionCache::create(dir.path(), &mission).unwrap();
        assert!(cache.cargo_home().is_dir());
        assert!(cache.target_dir().is_dir());
        assert!(std::fs::write(cache.cargo_home().join(".package-cache"), b"x").is_ok());
        assert!(std::fs::write(cache.target_dir().join("marker"), b"x").is_ok());
    }

    #[test]
    fn warmth_is_observable_so_the_cold_run_can_be_explained() {
        let dir = tempfile::tempdir().unwrap();
        let mission = ids::new::mission_id().unwrap();
        let cache = MissionCache::create(dir.path(), &mission).unwrap();
        assert!(!cache.is_warm(), "the first build in a mission is cold");
        std::fs::write(cache.target_dir().join("debug"), b"x").unwrap();
        assert!(cache.is_warm());
    }

    #[test]
    fn caches_do_not_cross_missions() {
        // The property the cross-mission fixture asserts: a marker written in one
        // mission is absent in the next.
        let dir = tempfile::tempdir().unwrap();
        let first = ids::new::mission_id().unwrap();
        let second = ids::new::mission_id().unwrap();
        let one = MissionCache::create(dir.path(), &first).unwrap();
        std::fs::write(one.target_dir().join("marker"), b"from mission one").unwrap();
        let two = MissionCache::create(dir.path(), &second).unwrap();
        assert!(
            !two.target_dir().join("marker").exists(),
            "cache state must not cross missions"
        );
    }

    #[test]
    fn closeout_destroys_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mission = ids::new::mission_id().unwrap();
        let cache = MissionCache::create(dir.path(), &mission).unwrap();
        let root = cache.root().to_path_buf();
        std::fs::write(cache.target_dir().join("big"), vec![0u8; 1024]).unwrap();
        assert!(cache.size_bytes() >= 1024);
        cache.destroy().unwrap();
        assert!(!root.exists());
        assert!(MissionCache::existing(dir.path(), &mission).is_none());
    }

    #[test]
    fn orphaned_caches_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let live = ids::new::mission_id().unwrap();
        let orphan = ids::new::mission_id().unwrap();
        MissionCache::create(dir.path(), &live).unwrap();
        MissionCache::create(dir.path(), &orphan).unwrap();
        let removed = remove_orphans(dir.path(), std::slice::from_ref(&live)).unwrap();
        assert_eq!(removed, vec![orphan.to_string()]);
        assert!(MissionCache::existing(dir.path(), &live).is_some());
    }

    #[test]
    fn removing_orphans_from_a_missing_root_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            remove_orphans(&dir.path().join("absent"), &[])
                .unwrap()
                .is_empty()
        );
    }
}
