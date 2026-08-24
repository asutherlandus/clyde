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
    pub fn materialise(
        &self,
        digest: &Digest,
        tree: &Path,
        relative: &Path,
    ) -> Result<MaterialisationKind> {
        let source = self.blob_path(digest);
        let target = tree.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| SnapshotError::io("creating a snapshot directory", error))?;
        }
        let _ = std::fs::remove_file(&target);
        match std::fs::hard_link(&source, &target) {
            Ok(()) => Ok(MaterialisationKind::Hardlink),
            Err(_) => {
                // Falls back to copying: correct everywhere, slower, and the
                // record says which happened.
                std::fs::copy(&source, &target)
                    .map_err(|error| SnapshotError::io("copying into a snapshot", error))?;
                set_mode(&target, BLOB_MODE)?;
                Ok(MaterialisationKind::Copy)
            }
        }
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
            .materialise(&digest, &tree, Path::new("src/a.rs"))
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
            .materialise(&digest, &tree, Path::new("a.rs"))
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
