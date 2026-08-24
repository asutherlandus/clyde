//! Snapshot construction.
//!
//! Enforcement is by materialisation: the snapshot contains the granted subtrees
//! minus absolute exclusions, plus the pins, and nothing else — so a read outside
//! it fails with `ENOENT`. No tracing, no privilege, and identical behaviour on
//! both sandbox backends (D18).
//!
//! Because grants are subtrees rather than file lists, a file created since the
//! last snapshot is picked up automatically. That is the property that keeps the
//! inner loop free of prompts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use clyde_core::baseline::AccessBaseline;
use clyde_core::ids::{MissionId, WorkspaceId};
use clyde_core::repo_path::RepoPath;
use clyde_core::snapshot::{MaterialisationKind, Snapshot, SnapshotEntry, SnapshotManifest};
use clyde_policy::exclusions::{ExclusionRule, exclusion_for};

use crate::content::ContentStore;
use crate::error::{Result, SnapshotError};

/// What a snapshot should contain.
#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    pub workspace: WorkspaceId,
    pub mission: MissionId,
    /// Absolute host path of the workspace root.
    pub root: PathBuf,
    /// The build target or requested path, recorded on the snapshot.
    pub requested_path: RepoPath,
    /// Subtrees to include whole.
    pub grants: Vec<RepoPath>,
    /// Individual paths admitted from outside the grants.
    pub pins: Vec<RepoPath>,
    /// Paths inside the grants that came from the build closure rather than from
    /// the mission's own scope, recorded for review.
    pub closure_paths: Vec<RepoPath>,
    /// Paths admitted from outside the lease's edit scope, recorded so mission
    /// review can show the confidentiality delta.
    pub out_of_lease_paths: Vec<RepoPath>,
    /// Additional exclusion patterns from configuration.
    pub configured_exclusions: BTreeSet<String>,
    /// Upper bound on the snapshot, so a runaway tree fails loudly rather than
    /// filling the store.
    pub max_bytes: u64,
    /// Upper bound on entry count, for the same reason.
    pub max_entries: usize,
}

impl SnapshotRequest {
    /// Builds a request from a confirmed access baseline.
    ///
    /// This is the normal path: what a task receives is exactly what a human
    /// confirmed, so enforcement cannot drift from the record.
    pub fn from_baseline(
        workspace: WorkspaceId,
        mission: MissionId,
        root: PathBuf,
        baseline: &AccessBaseline,
        configured_exclusions: BTreeSet<String>,
    ) -> Self {
        Self {
            workspace,
            mission,
            root,
            requested_path: baseline.target.clone(),
            grants: baseline.grants().cloned().collect(),
            pins: baseline.pins().cloned().collect(),
            closure_paths: Vec::new(),
            out_of_lease_paths: Vec::new(),
            configured_exclusions,
            max_bytes: 4 << 30,
            max_entries: 200_000,
        }
    }
}

/// A built snapshot and where it was materialised.
#[derive(Debug, Clone)]
pub struct BuiltSnapshot {
    pub snapshot: Snapshot,
    /// Host path of the materialised tree, bound read-only into the sandbox.
    pub tree: PathBuf,
    /// Paths that exist in the workspace but were excluded, which is what makes
    /// a drift diagnostic possible: under `ENOENT` inference, "the build wanted
    /// something that exists but was not admitted" is the signal.
    pub excluded: Vec<(RepoPath, String)>,
}

impl BuiltSnapshot {
    /// Whether an excluded path would explain a failure, and why.
    ///
    /// Under `ENOENT` inference this is a heuristic; exact reporting needs the
    /// FUSE path (OQ5). Saying so here keeps the diagnostic honest.
    pub fn explains(&self, path: &RepoPath) -> Option<&str> {
        self.excluded
            .iter()
            .find(|(candidate, _)| candidate == path)
            .map(|(_, reason)| reason.as_str())
    }
}

/// Builds and materialises a snapshot.
pub fn build(store: &ContentStore, request: &SnapshotRequest) -> Result<BuiltSnapshot> {
    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut excluded: Vec<(RepoPath, String)> = Vec::new();
    let mut exclusion_names: BTreeSet<String> = BTreeSet::new();
    let mut total_bytes: u64 = 0;
    let mut seen: BTreeSet<RepoPath> = BTreeSet::new();

    // Grants are walked as subtrees, so files created since the last snapshot
    // are admitted without any amendment.
    for grant in &request.grants {
        walk(
            store,
            request,
            grant,
            &mut entries,
            &mut excluded,
            &mut exclusion_names,
            &mut total_bytes,
            &mut seen,
        )?;
    }

    // Pins are individual paths from outside every grant. A pin that names a
    // directory admits that directory's subtree, which is the rollup form.
    for pin in &request.pins {
        walk(
            store,
            request,
            pin,
            &mut entries,
            &mut excluded,
            &mut exclusion_names,
            &mut total_bytes,
            &mut seen,
        )?;
    }

    let manifest = SnapshotManifest {
        entries,
        closure_paths: request.closure_paths.clone(),
        out_of_lease_paths: request.out_of_lease_paths.clone(),
        exclusions_applied: exclusion_names.into_iter().collect(),
    };
    manifest.validate()?;
    let id = manifest.identity()?;

    let tree = store.tree_path(id.as_str());
    // A snapshot identity is its content, so an existing tree with the same
    // identity is the same tree; rebuilding it would be wasted work.
    let mut materialisation = MaterialisationKind::Hardlink;
    if !tree.exists() {
        for entry in &manifest.entries {
            let kind = store.materialise(&entry.blake3, &tree, Path::new(entry.path.as_str()))?;
            if kind == MaterialisationKind::Copy {
                materialisation = MaterialisationKind::Copy;
            }
        }
    }

    Ok(BuiltSnapshot {
        snapshot: Snapshot {
            id,
            workspace: request.workspace.clone(),
            mission: request.mission.clone(),
            requested_path: request.requested_path.clone(),
            manifest,
            created_at: Utc::now(),
            materialisation,
        },
        tree,
        excluded,
    })
}

/// Walks one admitted path, which may be a file or a subtree.
#[allow(
    clippy::too_many_arguments,
    reason = "an accumulator-passing walk; a struct here would obscure it"
)]
fn walk(
    store: &ContentStore,
    request: &SnapshotRequest,
    path: &RepoPath,
    entries: &mut Vec<SnapshotEntry>,
    excluded: &mut Vec<(RepoPath, String)>,
    exclusion_names: &mut BTreeSet<String>,
    total_bytes: &mut u64,
    seen: &mut BTreeSet<RepoPath>,
) -> Result<()> {
    // Exclusions are applied *before* grants and cannot be admitted by one.
    if let Some(rule) = exclusion_for(path, &request.configured_exclusions) {
        record_exclusion(path, &rule, excluded, exclusion_names);
        return Ok(());
    }

    let host = path.to_host_path(&request.root);
    let Ok(metadata) = std::fs::symlink_metadata(&host) else {
        // A path that does not exist is not an error: a grant covers a subtree
        // that may be empty, and a pin may name a file that has been removed.
        return Ok(());
    };

    if metadata.file_type().is_symlink() {
        // Symlinks are not followed into the snapshot: a link could point
        // outside the workspace, and materialising its target would admit a path
        // no one approved.
        excluded.push((
            path.clone(),
            "symlink: not followed into a snapshot".to_owned(),
        ));
        exclusion_names.insert("symlink".to_owned());
        return Ok(());
    }

    if metadata.is_dir() {
        let Ok(children) = std::fs::read_dir(&host) else {
            return Ok(());
        };
        let mut names: Vec<String> = children
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        // Sorted so a snapshot's identity does not depend on directory order.
        names.sort();
        for name in names {
            let Ok(child) = path.join(&name) else {
                continue;
            };
            walk(
                store,
                request,
                &child,
                entries,
                excluded,
                exclusion_names,
                total_bytes,
                seen,
            )?;
        }
        return Ok(());
    }

    if !metadata.is_file() {
        // Devices, sockets, and fifos have no place in a source snapshot.
        excluded.push((path.clone(), "not a regular file".to_owned()));
        exclusion_names.insert("non-regular-file".to_owned());
        return Ok(());
    }

    if !seen.insert(path.clone()) {
        return Ok(());
    }
    if entries.len() >= request.max_entries {
        return Err(SnapshotError::io(
            "snapshot entry limit reached",
            std::io::Error::other(format!(
                "the snapshot exceeded {} entries; narrow the mission's scope",
                request.max_entries
            )),
        ));
    }
    *total_bytes = total_bytes.saturating_add(metadata.len());
    if *total_bytes > request.max_bytes {
        return Err(SnapshotError::io(
            "snapshot size limit reached",
            std::io::Error::other(format!(
                "the snapshot exceeded {} bytes; narrow the mission's scope",
                request.max_bytes
            )),
        ));
    }

    let (digest, size) = store.store_file(&host)?;
    entries.push(SnapshotEntry {
        path: path.clone(),
        mode: mode_of(&metadata),
        size,
        blake3: digest,
    });
    Ok(())
}

fn record_exclusion(
    path: &RepoPath,
    rule: &ExclusionRule,
    excluded: &mut Vec<(RepoPath, String)>,
    names: &mut BTreeSet<String>,
) {
    let reason = if rule.is_git() {
        // Diagnosed specifically: a build reading `.git` is neither drift nor a
        // bug in the user's code, and git metadata is a known MVP gap (D21).
        ".git is never available to build tasks, and git metadata is not yet supported".to_owned()
    } else {
        format!("excluded by rule {}", rule.name())
    };
    excluded.push((path.clone(), reason));
    names.insert(rule.name());
}

fn mode_of(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o777
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

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        store: ContentStore,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
        std::fs::write(root.join("crates/core/src/lib.rs"), "// lib\n").unwrap();
        std::fs::write(root.join("crates/core/Cargo.toml"), "[package]\n").unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        Fixture {
            _dir: dir,
            root,
            store,
        }
    }

    fn request(fixture: &Fixture, grants: &[&str]) -> SnapshotRequest {
        SnapshotRequest {
            workspace: ids::new::workspace_id().unwrap(),
            mission: ids::new::mission_id().unwrap(),
            root: fixture.root.clone(),
            requested_path: path("crates/core"),
            grants: grants.iter().map(|grant| path(grant)).collect(),
            pins: Vec::new(),
            closure_paths: Vec::new(),
            out_of_lease_paths: Vec::new(),
            configured_exclusions: BTreeSet::new(),
            max_bytes: 1 << 30,
            max_entries: 10_000,
        }
    }

    #[test]
    fn a_grant_admits_its_whole_subtree() {
        let fixture = fixture();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert_eq!(built.snapshot.manifest.entries.len(), 2);
        assert!(built.tree.join("crates/core/src/lib.rs").exists());
        assert!(
            built
                .snapshot
                .manifest
                .contains(&path("crates/core/src/lib.rs"))
        );
    }

    #[test]
    fn a_file_created_after_the_baseline_is_admitted_without_amendment() {
        // The property that keeps the inner loop prompt-free.
        let fixture = fixture();
        let first = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        std::fs::write(fixture.root.join("crates/core/src/new.rs"), "// new\n").unwrap();
        std::fs::create_dir_all(fixture.root.join("crates/core/tests")).unwrap();
        std::fs::write(fixture.root.join("crates/core/tests/it.rs"), "// test\n").unwrap();
        let second = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert_eq!(second.snapshot.manifest.entries.len(), 4);
        assert_ne!(
            first.snapshot.id, second.snapshot.id,
            "a changed tree is a different snapshot"
        );
        assert!(
            second
                .snapshot
                .manifest
                .contains(&path("crates/core/tests/it.rs"))
        );
    }

    #[test]
    fn a_secret_shaped_file_inside_a_grant_is_not_materialised() {
        // The `secret-shaped-files` fixture property.
        let fixture = fixture();
        std::fs::write(fixture.root.join("crates/core/.env.local"), "TOKEN=x\n").unwrap();
        std::fs::write(fixture.root.join("crates/core/server.pem"), "KEY\n").unwrap();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert!(
            !built
                .snapshot
                .manifest
                .contains(&path("crates/core/.env.local"))
        );
        assert!(!built.tree.join("crates/core/.env.local").exists());
        assert!(!built.tree.join("crates/core/server.pem").exists());
        assert!(
            built
                .snapshot
                .manifest
                .exclusions_applied
                .iter()
                .any(|rule| rule.contains("secret")),
            "the manifest records which rule fired"
        );
    }

    #[test]
    fn git_is_excluded_and_diagnosed_specifically() {
        let fixture = fixture();
        std::fs::create_dir_all(fixture.root.join("crates/core/.git")).unwrap();
        std::fs::write(fixture.root.join("crates/core/.git/config"), "x\n").unwrap();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert!(!built.tree.join("crates/core/.git").exists());
        let explanation = built.explains(&path("crates/core/.git")).unwrap();
        assert!(
            explanation.contains("never available") && explanation.contains("not yet supported"),
            "a build reading .git must not be sent hunting for a bug: {explanation}"
        );
    }

    #[test]
    fn build_output_is_excluded() {
        let fixture = fixture();
        std::fs::create_dir_all(fixture.root.join("crates/core/target/debug")).unwrap();
        std::fs::write(fixture.root.join("crates/core/target/debug/app"), "bin\n").unwrap();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert!(!built.tree.join("crates/core/target").exists());
    }

    #[test]
    fn symlinks_are_not_followed_out_of_the_workspace() {
        let fixture = fixture();
        let outside = fixture.root.parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, fixture.root.join("crates/core/link")).unwrap();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert!(!built.snapshot.manifest.contains(&path("crates/core/link")));
        assert!(built.explains(&path("crates/core/link")).is_some());
    }

    #[test]
    fn a_pin_admits_exactly_one_path_from_outside_the_grants() {
        let fixture = fixture();
        std::fs::create_dir_all(fixture.root.join("docs")).unwrap();
        std::fs::write(fixture.root.join("docs/schema.sql"), "CREATE TABLE\n").unwrap();
        std::fs::write(fixture.root.join("docs/other.sql"), "SECRET\n").unwrap();
        let mut request = request(&fixture, &["crates/core"]);
        request.pins = vec![path("docs/schema.sql")];
        let built = build(&fixture.store, &request).unwrap();
        assert!(built.snapshot.manifest.contains(&path("docs/schema.sql")));
        assert!(!built.snapshot.manifest.contains(&path("docs/other.sql")));
    }

    #[test]
    fn nothing_outside_grants_and_pins_is_materialised() {
        let fixture = fixture();
        std::fs::create_dir_all(fixture.root.join("crates/secrets")).unwrap();
        std::fs::write(fixture.root.join("crates/secrets/keys.rs"), "KEY\n").unwrap();
        let built = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert!(!built.tree.join("crates/secrets").exists());
        // Enforcement is by materialisation, so "not in the tree" and "cannot be
        // read" are the same statement.
        assert!(
            !built
                .snapshot
                .manifest
                .contains(&path("crates/secrets/keys.rs"))
        );
    }

    #[test]
    fn identical_content_produces_an_identical_snapshot_identity() {
        let fixture = fixture();
        let first = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        let second = build(&fixture.store, &request(&fixture, &["crates/core"])).unwrap();
        assert_eq!(first.snapshot.id, second.snapshot.id);
    }

    #[test]
    fn the_size_limit_fails_loudly_rather_than_filling_the_store() {
        let fixture = fixture();
        std::fs::write(fixture.root.join("crates/core/big.bin"), vec![0u8; 4096]).unwrap();
        let mut request = request(&fixture, &["crates/core"]);
        request.max_bytes = 100;
        let error = build(&fixture.store, &request).expect_err("the limit must fire");
        assert!(error.to_string().contains("narrow the mission"), "{error}");
    }

    #[test]
    fn a_missing_grant_is_not_an_error() {
        let fixture = fixture();
        let built = build(&fixture.store, &request(&fixture, &["crates/absent"])).unwrap();
        assert!(built.snapshot.manifest.entries.is_empty());
    }

    #[test]
    fn configured_exclusions_are_applied_too() {
        let fixture = fixture();
        std::fs::write(fixture.root.join("crates/core/data.bin"), "x\n").unwrap();
        let mut request = request(&fixture, &["crates/core"]);
        request.configured_exclusions = ["*.bin".to_owned()].into_iter().collect();
        let built = build(&fixture.store, &request).unwrap();
        assert!(
            !built
                .snapshot
                .manifest
                .contains(&path("crates/core/data.bin"))
        );
    }
}
