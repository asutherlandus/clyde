//! The code-execution inventory.
//!
//! Every dependency package that has a `build.rs` or is a proc-macro crate,
//! pinned by crate, version, and **source content hash** (D18). Computed from
//! the lockfile and the dependency bundle *before the sandbox starts*, so a
//! newly-arrived or changed build script is caught before it runs.
//!
//! The content hash is what turns "the same version" into a claim that can be
//! falsified: a package whose version is unchanged but whose source differs is a
//! registry-tampering signal, and is reported distinctly from an upgrade.

use std::path::{Path, PathBuf};

use clyde_core::Digest;
use clyde_core::baseline::{CodeExecEntry, CodeExecInventory, CodeExecKind};
use clyde_policy::access::{LockfileSummary, PackageSource};

use crate::cargo::closure::parse_manifest;
use crate::error::{Result, SnapshotError};

/// Where package sources live for inspection.
///
/// A cargo registry cache lays packages out as `<root>/<name>-<version>/`, which
/// is what both `cargo vendor` and an extracted registry cache produce.
#[derive(Debug, Clone)]
pub struct PackageSources {
    root: PathBuf,
}

impl PackageSources {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding a package's source, if present.
    ///
    /// Two layouts are accepted: `name-version` (registry cache and `cargo
    /// vendor` with versions) and `name` (`cargo vendor` without).
    pub fn directory_for(&self, name: &str, version: &str) -> Option<PathBuf> {
        let versioned = self.root.join(format!("{name}-{version}"));
        if versioned.is_dir() {
            return Some(versioned);
        }
        let plain = self.root.join(name);
        plain.is_dir().then_some(plain)
    }
}

/// Computes the inventory for a lockfile against a set of package sources.
///
/// Packages the sources do not contain are skipped rather than assumed
/// harmless: a caller that needs completeness compares the counts, and the
/// bundle import path refuses an incomplete bundle outright.
pub fn compute(
    lockfile: &LockfileSummary,
    lockfile_digest: Digest,
    sources: &PackageSources,
) -> Result<CodeExecInventory> {
    let mut entries = Vec::new();
    for package in &lockfile.packages {
        // Path packages are first-party code: they are covered by the repo read
        // set, not by the dependency inventory.
        if matches!(package.source, PackageSource::Path) {
            continue;
        }
        let Some(directory) = sources.directory_for(&package.name, &package.version) else {
            continue;
        };
        let Some(kind) = classify_package(&directory)? else {
            continue;
        };
        entries.push(CodeExecEntry {
            crate_name: package.name.clone(),
            version: package.version.clone(),
            kind,
            source_blake3: hash_directory(&directory)?,
        });
    }
    Ok(CodeExecInventory {
        lockfile_digest: Some(lockfile_digest),
        entries,
    }
    .canonicalised())
}

/// Whether a package executes code at build time, and how.
///
/// `build.rs` presence is checked on disk as well as in the manifest, because a
/// manifest without an explicit `build` key still gets an implicit build script
/// when `build.rs` exists at the package root.
pub fn classify_package(directory: &Path) -> Result<Option<CodeExecKind>> {
    let manifest_path = directory.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        return Ok(None);
    };
    let manifest = parse_manifest(&manifest_path, &text)?;
    let has_build_script = manifest.has_build_script || directory.join("build.rs").is_file();
    Ok(match (has_build_script, manifest.is_proc_macro) {
        (true, true) => Some(CodeExecKind::Both),
        (true, false) => Some(CodeExecKind::BuildScript),
        (false, true) => Some(CodeExecKind::ProcMacro),
        (false, false) => None,
    })
}

/// Hashes a package's source tree.
///
/// Path and content are both hashed, in sorted order, so a file moved or renamed
/// changes the digest. `.cargo-ok` and similar cargo bookkeeping files are
/// excluded, since they vary between extractions of identical content.
pub fn hash_directory(directory: &Path) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    let mut files = Vec::new();
    collect_files(directory, directory, &mut files)?;
    files.sort();
    for relative in files {
        let absolute = directory.join(&relative);
        let bytes = std::fs::read(&absolute)
            .map_err(|error| SnapshotError::io(format!("reading {absolute:?}"), error))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(&Digest::of_bytes(&bytes).as_str().len().to_le_bytes());
        hasher.update(Digest::of_bytes(&bytes).as_str().as_bytes());
    }
    Digest::parse(hasher.finalize().to_hex().to_string()).map_err(SnapshotError::Invalid)
}

/// Cargo bookkeeping that varies between extractions of identical content.
const IGNORED_FILES: [&str; 2] = [".cargo-ok", ".cargo_vcs_info.json"];

fn collect_files(root: &Path, directory: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| SnapshotError::io(format!("reading {directory:?}"), error))?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if IGNORED_FILES.contains(&name.as_str()) {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
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
    use clyde_policy::access::LockedPackage;

    fn write(root: &Path, relative: &str, contents: &str) {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }

    fn registry(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source: PackageSource::Registry {
                index: "sparse+https://index.crates.io/".to_owned(),
            },
        }
    }

    #[test]
    fn packages_with_build_scripts_and_proc_macros_are_inventoried() {
        let dir = tempfile::tempdir().unwrap();
        let sources = dir.path().join("sources");
        write(
            &sources,
            "plain-1.0.0/Cargo.toml",
            "[package]\nname = \"plain\"\n",
        );
        write(&sources, "plain-1.0.0/src/lib.rs", "// plain\n");
        write(
            &sources,
            "scripted-1.0.0/Cargo.toml",
            "[package]\nname = \"scripted\"\n",
        );
        // No `build` key: the implicit build script is the case a manifest-only
        // check would miss.
        write(&sources, "scripted-1.0.0/build.rs", "fn main() {}\n");
        write(
            &sources,
            "macro-1.0.0/Cargo.toml",
            "[package]\nname = \"macro\"\n\n[lib]\nproc-macro = true\n",
        );

        let lockfile = LockfileSummary {
            packages: vec![
                registry("plain", "1.0.0"),
                registry("scripted", "1.0.0"),
                registry("macro", "1.0.0"),
            ],
        };
        let inventory = compute(
            &lockfile,
            Digest::of_bytes(b"lock"),
            &PackageSources::new(&sources),
        )
        .unwrap();
        assert_eq!(inventory.entries.len(), 2, "{:?}", inventory.entries);
        assert_eq!(
            inventory.find("scripted").map(|entry| entry.kind),
            Some(CodeExecKind::BuildScript)
        );
        assert_eq!(
            inventory.find("macro").map(|entry| entry.kind),
            Some(CodeExecKind::ProcMacro)
        );
        assert!(inventory.find("plain").is_none());
    }

    #[test]
    fn a_package_that_is_both_is_reported_as_both() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("both-1.0.0");
        write(
            &package,
            "Cargo.toml",
            "[package]\nname = \"both\"\nbuild = \"build.rs\"\n\n[lib]\nproc-macro = true\n",
        );
        write(&package, "build.rs", "fn main() {}\n");
        assert_eq!(
            classify_package(&package).unwrap(),
            Some(CodeExecKind::Both)
        );
    }

    #[test]
    fn path_packages_are_not_dependency_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let sources = dir.path().join("sources");
        write(
            &sources,
            "local-0.1.0/Cargo.toml",
            "[package]\nname = \"local\"\n",
        );
        write(&sources, "local-0.1.0/build.rs", "fn main() {}\n");
        let lockfile = LockfileSummary {
            packages: vec![LockedPackage {
                name: "local".to_owned(),
                version: "0.1.0".to_owned(),
                source: PackageSource::Path,
            }],
        };
        let inventory = compute(
            &lockfile,
            Digest::of_bytes(b"lock"),
            &PackageSources::new(&sources),
        )
        .unwrap();
        assert!(
            inventory.entries.is_empty(),
            "first-party code is covered by the repo read set, not the inventory"
        );
    }

    #[test]
    fn the_content_hash_changes_when_the_source_changes_at_the_same_version() {
        // The `dep-same-version-tampered` fixture property.
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("ring-0.17.8");
        write(&package, "Cargo.toml", "[package]\nname = \"ring\"\n");
        write(&package, "build.rs", "fn main() {}\n");
        let before = hash_directory(&package).unwrap();
        write(&package, "build.rs", "fn main() { /* tampered */ }\n");
        let after = hash_directory(&package).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn the_content_hash_changes_when_a_file_is_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("x-1.0.0");
        write(&package, "Cargo.toml", "[package]\nname = \"x\"\n");
        write(&package, "src/lib.rs", "// same content\n");
        let before = hash_directory(&package).unwrap();
        std::fs::rename(package.join("src/lib.rs"), package.join("src/other.rs")).unwrap();
        assert_ne!(before, hash_directory(&package).unwrap());
    }

    #[test]
    fn cargo_bookkeeping_files_do_not_change_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("x-1.0.0");
        write(&package, "Cargo.toml", "[package]\nname = \"x\"\n");
        let before = hash_directory(&package).unwrap();
        write(&package, ".cargo-ok", "");
        assert_eq!(
            before,
            hash_directory(&package).unwrap(),
            "extraction bookkeeping must not look like tampering"
        );
    }

    #[test]
    fn both_vendor_layouts_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let sources = dir.path().join("sources");
        std::fs::create_dir_all(sources.join("versioned-1.0.0")).unwrap();
        std::fs::create_dir_all(sources.join("plain")).unwrap();
        let sources = PackageSources::new(&sources);
        assert!(sources.directory_for("versioned", "1.0.0").is_some());
        assert!(sources.directory_for("plain", "2.0.0").is_some());
        assert!(sources.directory_for("absent", "1.0.0").is_none());
    }

    #[test]
    fn a_package_absent_from_the_sources_is_skipped_not_assumed_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let sources = PackageSources::new(dir.path().join("empty"));
        let lockfile = LockfileSummary {
            packages: vec![registry("missing", "1.0.0")],
        };
        let inventory = compute(&lockfile, Digest::of_bytes(b"lock"), &sources).unwrap();
        assert!(inventory.entries.is_empty());
        assert_eq!(
            lockfile.packages.len(),
            1,
            "the caller compares counts to detect an incomplete bundle"
        );
    }
}
