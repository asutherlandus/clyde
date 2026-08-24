//! The dependency bundle store.
//!
//! Bundles are content-addressed, immutable, and mounted read-only wherever they
//! are used (D3). A mission's `CARGO_HOME` is *seeded by hardlink* from a bundle
//! rather than mounted from it, because cargo writes lock files into `CARGO_HOME`
//! even offline — so the writable copy is per-mission and the shared content
//! stays read-only.

use std::path::{Path, PathBuf};

use chrono::Utc;
use clyde_core::artifact::Artifact;
use clyde_core::baseline::CodeExecInventory;
use clyde_core::ids::ArtifactId;
use clyde_policy::access::PackageSource;
use clyde_store::BundleRecord;

use crate::cargo::inventory::{self, PackageSources};
use crate::cargo::lockfile;
use crate::error::{Result, SnapshotError};

/// Mode applied to bundle contents: read-only for everyone.
const BUNDLE_MODE: u32 = 0o444;

/// The read-only bundle store.
#[derive(Debug, Clone)]
pub struct BundleStore {
    root: PathBuf,
}

impl BundleStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|error| SnapshotError::io("creating the dependency bundle store", error))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a bundle lives, by the digest of the lockfile it satisfies plus its
    /// own content.
    pub fn bundle_path(&self, id: &ArtifactId) -> PathBuf {
        let name = id.as_str().rsplit(':').next().unwrap_or(id.as_str());
        self.root.join(name)
    }

    /// Imports a host-produced cargo cache or vendor directory.
    ///
    /// `clyde deps import` is a human, admin-channel operation: Phase 2a has no
    /// fetch task, so offline builds need a bundle to exist, and this is also
    /// what makes the code-execution inventory computable before Phase 3.
    pub fn import(&self, source: &Path, lockfile_path: &Path) -> Result<BundleRecord> {
        if !source.is_dir() {
            return Err(SnapshotError::Bundle {
                path: source.to_path_buf(),
                detail: "not a directory".to_owned(),
            });
        }
        let parsed = lockfile::read(lockfile_path)?;
        let sources = PackageSources::new(source);

        // Every registry package the lockfile names must be present: a bundle
        // that satisfies a lockfile only partially would make a build fail later
        // with a confusing error rather than failing here with a clear one.
        let missing: Vec<String> = parsed
            .summary
            .packages
            .iter()
            .filter(|package| !matches!(package.source, PackageSource::Path))
            .filter(|package| {
                sources
                    .directory_for(&package.name, &package.version)
                    .is_none()
            })
            .map(|package| format!("{} {}", package.name, package.version))
            .collect();
        if !missing.is_empty() {
            return Err(SnapshotError::Bundle {
                path: source.to_path_buf(),
                detail: format!(
                    "does not satisfy the lockfile; {} package(s) missing, starting with {}",
                    missing.len(),
                    missing.first().map(String::as_str).unwrap_or("")
                ),
            });
        }

        let inventory = inventory::compute(&parsed.summary, parsed.digest.clone(), &sources)?;
        let content_digest = inventory::hash_directory(source)?;
        let id = Artifact::id_for(&content_digest).map_err(SnapshotError::Invalid)?;
        let target = self.bundle_path(&id);
        if !target.exists() {
            copy_read_only(source, &target)?;
        }

        let registries: Vec<String> = {
            let mut names: Vec<String> = parsed
                .summary
                .packages
                .iter()
                .filter_map(|package| match &package.source {
                    PackageSource::Registry { index } => Some(index.clone()),
                    _ => None,
                })
                .collect();
            names.sort();
            names.dedup();
            names
        };

        Ok(BundleRecord {
            artifact: id,
            lockfile_digest: parsed.digest,
            crate_count: u32::try_from(parsed.summary.packages.len()).unwrap_or(u32::MAX),
            lockfile: parsed.summary,
            content_ref: target,
            inventory,
            created_at: Utc::now(),
            registries,
        })
    }

    /// Seeds a mission's writable `CARGO_HOME` from a bundle, by hardlink.
    ///
    /// Hardlinks keep the seeding cheap while the bundle stays read-only; the
    /// mission's copy is writable because cargo writes into `CARGO_HOME` even
    /// offline.
    pub fn seed_cargo_home(&self, record: &BundleRecord, cargo_home: &Path) -> Result<u64> {
        let registry_src = record.content_ref.clone();
        let cache = cargo_home.join("registry/src/clyde-bundle");
        std::fs::create_dir_all(&cache)
            .map_err(|error| SnapshotError::io("creating the mission CARGO_HOME", error))?;
        link_tree(&registry_src, &cache)
    }

    /// The inventory a bundle was recorded with.
    pub fn inventory_of(record: &BundleRecord) -> &CodeExecInventory {
        &record.inventory
    }

    /// Verifies a bundle's content still hashes to its identifier.
    ///
    /// Cheap tamper detection for a store that is meant to be immutable.
    pub fn verify(&self, record: &BundleRecord) -> Result<bool> {
        let digest = inventory::hash_directory(&record.content_ref)?;
        Ok(Artifact::id_for(&digest)
            .map(|id| id == record.artifact)
            .unwrap_or(false))
    }
}

/// Copies a tree, making every file read-only.
fn copy_read_only(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(target)
        .map_err(|error| SnapshotError::io("creating a bundle directory", error))?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| SnapshotError::io(format!("reading {source:?}"), error))?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let child_target = target.join(entry.file_name());
        if kind.is_symlink() {
            // A symlink in a bundle could point anywhere; bundles hold content,
            // not references.
            continue;
        }
        if kind.is_dir() {
            copy_read_only(&entry.path(), &child_target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &child_target)
                .map_err(|error| SnapshotError::io("copying into the bundle store", error))?;
            std::fs::set_permissions(&child_target, std::fs::Permissions::from_mode(BUNDLE_MODE))
                .map_err(|error| SnapshotError::io("restricting a bundle file", error))?;
        }
    }
    Ok(())
}

/// Hardlinks a tree, falling back to copying where hardlinks are unavailable.
fn link_tree(source: &Path, target: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    std::fs::create_dir_all(target)
        .map_err(|error| SnapshotError::io("creating a seeded directory", error))?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| SnapshotError::io(format!("reading {source:?}"), error))?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let child_target = target.join(entry.file_name());
        if kind.is_dir() {
            bytes = bytes.saturating_add(link_tree(&entry.path(), &child_target)?);
        } else if kind.is_file() {
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            let _ = std::fs::remove_file(&child_target);
            if std::fs::hard_link(entry.path(), &child_target).is_err() {
                std::fs::copy(entry.path(), &child_target)
                    .map_err(|error| SnapshotError::io("seeding the mission cache", error))?;
            }
            bytes = bytes.saturating_add(size);
        }
    }
    Ok(bytes)
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
    use std::os::unix::fs::PermissionsExt as _;

    fn write(root: &Path, relative: &str, contents: &str) {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        vendor: PathBuf,
        lockfile: PathBuf,
        store: BundleStore,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor");
        write(
            &vendor,
            "serde-1.0.229/Cargo.toml",
            "[package]\nname = \"serde\"\n",
        );
        write(&vendor, "serde-1.0.229/src/lib.rs", "// serde\n");
        write(
            &vendor,
            "ring-0.17.8/Cargo.toml",
            "[package]\nname = \"ring\"\n",
        );
        write(&vendor, "ring-0.17.8/build.rs", "fn main() {}\n");

        let lockfile = dir.path().join("Cargo.lock");
        std::fs::write(
            &lockfile,
            r#"
version = 4

[[package]]
name = "serde"
version = "1.0.229"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "ring"
version = "0.17.8"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "app"
version = "0.1.0"
"#,
        )
        .unwrap();

        let store = BundleStore::open(dir.path().join("deps")).unwrap();
        Fixture {
            _dir: dir,
            vendor,
            lockfile,
            store,
        }
    }

    #[test]
    fn importing_records_the_lockfile_it_satisfies_and_the_inventory() {
        let fixture = fixture();
        let record = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        assert_eq!(record.crate_count, 3);
        assert_eq!(
            record.inventory.entries.len(),
            1,
            "only ring executes code at build time"
        );
        assert!(record.inventory.find("ring").is_some());
        assert_eq!(record.registries.len(), 1);
        assert!(record.content_ref.exists());
        assert_eq!(
            record.lockfile_digest,
            crate::cargo::lockfile::read(&fixture.lockfile)
                .unwrap()
                .digest
        );
    }

    #[test]
    fn bundle_contents_are_read_only() {
        let fixture = fixture();
        let record = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        let file = record.content_ref.join("serde-1.0.229/src/lib.rs");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444, "a bundle is read-only wherever it is mounted");
    }

    #[test]
    fn an_incomplete_bundle_is_refused_at_import_rather_than_at_build_time() {
        let fixture = fixture();
        std::fs::remove_dir_all(fixture.vendor.join("ring-0.17.8")).unwrap();
        let error = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .expect_err("an incomplete bundle must be refused");
        assert!(
            error.to_string().contains("does not satisfy the lockfile"),
            "{error}"
        );
    }

    #[test]
    fn path_packages_are_not_expected_in_the_bundle() {
        // `app` is a path package in the fixture lockfile and is not vendored;
        // that must not make the bundle incomplete.
        let fixture = fixture();
        assert!(
            fixture
                .store
                .import(&fixture.vendor, &fixture.lockfile)
                .is_ok()
        );
    }

    #[test]
    fn importing_is_content_addressed_and_idempotent() {
        let fixture = fixture();
        let first = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        let second = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        assert_eq!(first.artifact, second.artifact);
        assert_eq!(first.content_ref, second.content_ref);
    }

    #[test]
    fn changed_content_produces_a_different_bundle() {
        let fixture = fixture();
        let first = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        write(
            &fixture.vendor,
            "ring-0.17.8/build.rs",
            "fn main() { /* new */ }\n",
        );
        let second = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        assert_ne!(first.artifact, second.artifact);
        assert_ne!(
            first
                .inventory
                .find("ring")
                .map(|entry| entry.source_blake3.clone()),
            second
                .inventory
                .find("ring")
                .map(|entry| entry.source_blake3.clone()),
            "the same version with different content must be distinguishable"
        );
    }

    #[test]
    fn seeding_a_cargo_home_shares_content_but_leaves_it_writable_where_it_must_be() {
        let fixture = fixture();
        let record = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        let cargo_home = fixture._dir.path().join("missions/m1/cargo-home");
        let bytes = fixture.store.seed_cargo_home(&record, &cargo_home).unwrap();
        assert!(bytes > 0);
        let seeded = cargo_home.join("registry/src/clyde-bundle/serde-1.0.229/src/lib.rs");
        assert!(seeded.exists());
        // Cargo writes lock files into CARGO_HOME even offline, so the directory
        // itself must be writable.
        assert!(std::fs::write(cargo_home.join("probe"), b"x").is_ok());
    }

    #[test]
    fn verification_detects_tampering_with_a_stored_bundle() {
        let fixture = fixture();
        let record = fixture
            .store
            .import(&fixture.vendor, &fixture.lockfile)
            .unwrap();
        assert!(fixture.store.verify(&record).unwrap());
        let file = record.content_ref.join("ring-0.17.8/build.rs");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&file, "fn main() { /* tampered */ }\n").unwrap();
        assert!(!fixture.store.verify(&record).unwrap());
    }

    #[test]
    fn importing_something_that_is_not_a_directory_is_refused() {
        let fixture = fixture();
        let error = fixture
            .store
            .import(&fixture.lockfile, &fixture.lockfile)
            .expect_err("a file is not a bundle");
        assert!(matches!(error, SnapshotError::Bundle { .. }));
    }
}
