//! The content-addressed store and snapshot materialisation.
//!
//! Snapshots are **always** bound read-only. Hardlink materialisation shares
//! inodes with the content store, so a writable bind would corrupt the store:
//! the read-only bind is what makes hardlinking safe, not a stylistic choice
//! (schema reference: Snapshot invariants).

use std::path::{Path, PathBuf};

use clyde_core::Digest;
use clyde_core::snapshot::MaterialisationKind;

use crate::error::{Result, SnapshotError};

/// Mode applied to stored blobs.
///
/// Read-only for everyone, including the owner, so that even a mistake in the
/// mount table cannot rewrite shared content through a hardlink.
const BLOB_MODE: u32 = 0o444;

/// Which mtime a materialised file should carry (D27).
///
/// Cargo decides freshness for local sources by comparing source mtimes against
/// build output mtimes, and has no content-hash freshness mode on the stable
/// toolchain. So a snapshot must present mtimes that agree with what actually
/// changed, in both directions:
///
/// - an unchanged file must keep an mtime *older* than the build outputs, or
///   every run rebuilds everything;
/// - a changed file must carry one *newer* than them, or cargo skips work it
///   needed to do and the task reports a pass for a tree it did not build.
///
/// The second is the dangerous one. Content addressing makes it reachable by an
/// ordinary `git checkout --`: reverting a file re-links a blob ingested
/// earlier, whose mtime predates the outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// The content is unchanged since this mission's previous snapshot. Share the
    /// blob's inode and therefore its mtime.
    Unchanged,
    /// The content differs from the previous snapshot in either direction. Give
    /// it a materialisation-time mtime of its own, which needs a copy: a hardlink
    /// cannot carry an mtime that differs from the blob's.
    Changed,
}

/// A content-addressed blob store.
#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    /// Opens or creates the store under `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("objects"))
            .map_err(|error| SnapshotError::io("creating the content store", error))?;
        std::fs::create_dir_all(root.join("trees"))
            .map_err(|error| SnapshotError::io("creating the snapshot tree directory", error))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a blob lives, sharded by digest prefix so directories stay small.
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest.as_str();
        let shard = hex.get(..2).unwrap_or("00");
        self.root.join("objects").join(shard).join(hex)
    }

    /// Where a snapshot's materialised tree lives.
    pub fn tree_path(&self, snapshot_id: &str) -> PathBuf {
        // The identifier contains a colon; the filesystem name uses the hex part
        // only, so the path is portable.
        let name = snapshot_id.rsplit(':').next().unwrap_or(snapshot_id);
        self.root.join("trees").join(name)
    }

    /// Stores a file's content, returning its digest.
    ///
    /// Idempotent: a blob that already exists is left alone, which is what makes
    /// repeated snapshots of a mostly unchanged tree cheap.
    pub fn store_file(&self, source: &Path) -> Result<(Digest, u64)> {
        let bytes = std::fs::read(source)
            .map_err(|error| SnapshotError::io(format!("reading {source:?}"), error))?;
        let digest = Digest::of_bytes(&bytes);
        let target = self.blob_path(&digest);
        if target.exists() {
            return Ok((digest, bytes.len() as u64));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| SnapshotError::io("creating a blob shard", error))?;
        }
        // Written to a temporary name and renamed, so a crash mid-write cannot
        // leave a truncated blob at a digest that claims to describe it.
        let temporary = target.with_extension("partial");
        std::fs::write(&temporary, &bytes)
            .map_err(|error| SnapshotError::io("writing a blob", error))?;
        set_mode(&temporary, BLOB_MODE)?;
        std::fs::rename(&temporary, &target)
            .map_err(|error| SnapshotError::io("publishing a blob", error))?;
        Ok((digest, bytes.len() as u64))
    }

    /// Materialises one entry into a tree, preferring hardlinks.
    ///
    /// Returns which strategy was used, so the snapshot record states how it was
    /// built rather than assuming.
    ///
    /// `freshness` decides the mtime the task will see, and it is not an
    /// optimisation hint (D27): a [`Freshness::Changed`] entry is copied
    /// precisely *because* a hardlink cannot carry an mtime of its own, and a
    /// [`Freshness::Unchanged`] entry is hardlinked precisely because sharing the
    /// blob's inode is what preserves the mtime the last run saw.
    pub fn materialise(
        &self,
        digest: &Digest,
        tree: &Path,
        relative: &Path,
        freshness: Freshness,
    ) -> Result<MaterialisationKind> {
        let source = self.blob_path(digest);
        let target = tree.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| SnapshotError::io("creating a snapshot directory", error))?;
        }
        let _ = std::fs::remove_file(&target);
        if freshness == Freshness::Changed {
            return self.copy_fresh(&source, &target).map(|()| {
                // A deliberate copy, not a fallback. The record says `Copy`
                // either way; what differs is why, and the reason is that this
                // file changed.
                MaterialisationKind::Copy
            });
        }
        match std::fs::hard_link(&source, &target) {
            Ok(()) => Ok(MaterialisationKind::Hardlink),
            Err(_) => {
                // Falls back to copying: correct everywhere, slower, and the
                // record says which happened.
                //
                // The copied file must still carry the blob's mtime. `fs::copy`
                // carries permissions and not timestamps, so without this the
                // fallback would stamp every file with the copy time and cargo
                // would rebuild the world on every run — a silent change of
                // performance class, not merely a slower path (D27).
                self.copy_inheriting_mtime(&source, &target)?;
                Ok(MaterialisationKind::Copy)
            }
        }
    }

    /// Copies a blob and gives the copy an mtime of its own, taken now.
    fn copy_fresh(&self, source: &Path, target: &Path) -> Result<()> {
        copy_with_mtime(source, target, std::time::SystemTime::now())
    }

    /// Copies a blob and carries its mtime across, so the copy is
    /// indistinguishable from the hardlink it stands in for.
    fn copy_inheriting_mtime(&self, source: &Path, target: &Path) -> Result<()> {
        let modified = std::fs::metadata(source)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| SnapshotError::io("reading a blob's mtime", error))?;
        copy_with_mtime(source, target, modified)
    }

    /// Removes a materialised tree. The blobs it shared stay in the store.
    pub fn remove_tree(&self, snapshot_id: &str) -> Result<()> {
        let path = self.tree_path(snapshot_id);
        if path.exists() {
            // Tree entries are read-only, so the directory walk needs to restore
            // write permission on directories before removal.
            std::fs::remove_dir_all(&path)
                .map_err(|error| SnapshotError::io("removing a snapshot tree", error))?;
        }
        Ok(())
    }

    /// Total bytes held in the object store, for budget accounting.
    pub fn size_bytes(&self) -> u64 {
        directory_size(&self.root.join("objects"))
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| SnapshotError::io("setting file permissions", error))
}

/// Copies a blob and stamps the copy with an explicit mtime.
///
/// The mode dance is not incidental: blobs are stored `0444`, `fs::copy` carries
/// permissions across, and `futimens` needs the file opened for writing. So the
/// copy is made writable, stamped, and then sealed read-only like every other
/// entry in a snapshot tree.
fn copy_with_mtime(source: &Path, target: &Path, modified: std::time::SystemTime) -> Result<()> {
    std::fs::copy(source, target)
        .map_err(|error| SnapshotError::io("copying into a snapshot", error))?;
    set_mode(target, 0o600)?;
    set_modified(target, modified)?;
    set_mode(target, BLOB_MODE)
}

/// Sets a file's modification time.
///
/// Done before the read-only mode is applied, since a blob is stored `0444` and
/// the timestamp is what a build tool actually reads to decide freshness.
fn set_modified(path: &Path, modified: std::time::SystemTime) -> Result<()> {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .map_err(|error| SnapshotError::io("opening a snapshot file to set its mtime", error))?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .map_err(|error| SnapshotError::io("setting a snapshot file's mtime", error))
}

/// Recursive size, saturating rather than overflowing.
pub fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .fold(0u64, |total, entry| {
            let Ok(metadata) = entry.metadata() else {
                return total;
            };
            if metadata.is_dir() {
                total.saturating_add(directory_size(&entry.path()))
            } else {
                total.saturating_add(metadata.len())
            }
        })
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

    #[test]
    fn storing_is_content_addressed_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, b"fn main() {}").unwrap();

        let (digest, size) = store.store_file(&source).unwrap();
        assert_eq!(size, 12);
        assert_eq!(digest, Digest::of_bytes(b"fn main() {}"));
        assert!(store.blob_path(&digest).exists());

        let other = dir.path().join("b.rs");
        std::fs::write(&other, b"fn main() {}").unwrap();
        let (again, _) = store.store_file(&other).unwrap();
        assert_eq!(again, digest, "identical content is one blob");
    }

    #[test]
    fn stored_blobs_are_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, b"x").unwrap();
        let (digest, _) = store.store_file(&source).unwrap();
        let mode = std::fs::metadata(store.blob_path(&digest))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o444,
            "a writable blob could be rewritten through a hardlink"
        );
    }

    #[test]
    fn materialising_shares_the_blob_and_the_tree_entry_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, b"content").unwrap();
        let (digest, _) = store.store_file(&source).unwrap();

        let tree = store.tree_path("s-blake3:abc");
        let kind = store
            .materialise(&digest, &tree, Path::new("src/a.rs"), Freshness::Unchanged)
            .unwrap();
        let target = tree.join("src/a.rs");
        assert!(target.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"content");
        if kind == MaterialisationKind::Hardlink {
            let blob_inode = std::fs::metadata(store.blob_path(&digest)).unwrap();
            let tree_inode = std::fs::metadata(&target).unwrap();
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                blob_inode.ino(),
                tree_inode.ino(),
                "hardlink materialisation shares the inode"
            );
        }
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444);
    }

    #[test]
    fn a_tree_can_be_removed_without_losing_the_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, b"x").unwrap();
        let (digest, _) = store.store_file(&source).unwrap();
        let tree = store.tree_path("s-blake3:abc");
        store
            .materialise(&digest, &tree, Path::new("a.rs"), Freshness::Unchanged)
            .unwrap();
        store.remove_tree("s-blake3:abc").unwrap();
        assert!(!tree.exists());
        assert!(store.blob_path(&digest).exists());
    }

    #[test]
    fn tree_paths_are_derived_from_the_digest_not_the_whole_identifier() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        let path = store.tree_path("s-blake3:deadbeef");
        assert!(path.ends_with("deadbeef"), "{path:?}");
    }

    #[test]
    fn size_accounting_walks_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        assert_eq!(store.size_bytes(), 0);
        let source = dir.path().join("a.rs");
        std::fs::write(&source, vec![0u8; 100]).unwrap();
        store.store_file(&source).unwrap();
        assert_eq!(store.size_bytes(), 100);
    }

    #[test]
    fn a_missing_source_is_an_error_not_an_empty_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path().join("store")).unwrap();
        assert!(store.store_file(&dir.path().join("absent")).is_err());
    }
}
