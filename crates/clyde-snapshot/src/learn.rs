//! Learn-mode observation (D18).
//!
//! For reads static analysis cannot see — an `include_str!` to an arbitrary
//! path, a `build.rs` reading repo files — a human may run the task in learn
//! mode: auto-admit within the mission's read scope, observe actual reads
//! through host-side `inotify` on the materialised tree, and produce a baseline
//! proposal for review.
//!
//! Learn mode is itself a privilege and is constrained accordingly: human-invoked
//! on the admin channel only, never reachable by an actor, never a default and
//! never a fallback Clyde selects on its own, scoped to a single run, marked
//! distinctly in the audit log, and producing a proposal that has no effect until
//! a human confirms it. The static-proposal path exists so that this is rarely
//! needed, because a learn run is a wide-scope execution of exactly the code you
//! are trying to constrain.
//!
//! **Backend limitation.** Host-side inotify cannot see reads inside a microVM
//! guest. Until the FUSE/virtio-fs path exists (OQ5), learn mode runs on the
//! namespace backend and the resulting baseline is used on both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use clyde_core::repo_path::RepoPath;
use inotify::{Inotify, WatchMask};

use crate::error::{Result, SnapshotError};

/// A running observation of a materialised tree.
#[derive(Debug)]
pub struct Observation {
    observed: Arc<Mutex<Vec<RepoPath>>>,
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Directories the watch limit prevented us from watching. Reported rather
    /// than swallowed: an incomplete observation must not look like a complete
    /// one.
    unwatched: usize,
}

impl Observation {
    /// Begins observing reads under `tree`.
    ///
    /// `tree` is the materialised snapshot, and paths are reported relative to
    /// it, which is what the baseline speaks in.
    pub fn start(tree: &Path) -> Result<Self> {
        let inotify = Inotify::init()
            .map_err(|error| SnapshotError::Observation(format!("inotify init: {error}")))?;
        let mut watches = inotify.watches();
        let mut by_descriptor: BTreeMap<i32, PathBuf> = BTreeMap::new();
        let mut unwatched = 0usize;

        // Watching is per-directory, so the whole tree is enumerated up front.
        // A tree that grows during the run is not fully covered; that is a known
        // limitation of the inotify approach and is why the proposal is reviewed.
        for directory in directories(tree) {
            match watches.add(
                &directory,
                WatchMask::OPEN | WatchMask::ACCESS | WatchMask::CLOSE_NOWRITE,
            ) {
                Ok(descriptor) => {
                    by_descriptor.insert(descriptor_id(&descriptor), directory);
                }
                Err(_) => unwatched = unwatched.saturating_add(1),
            }
        }

        let observed = Arc::new(Mutex::new(Vec::new()));
        let (stop, stop_rx) = mpsc::channel();
        let tree = tree.to_path_buf();
        let sink = Arc::clone(&observed);
        let worker = std::thread::spawn(move || {
            collect(inotify, by_descriptor, tree, sink, stop_rx);
        });

        Ok(Self {
            observed,
            stop,
            worker: Some(worker),
            unwatched,
        })
    }

    /// How many directories could not be watched.
    pub fn unwatched_directories(&self) -> usize {
        self.unwatched
    }

    /// Stops observing and returns the paths that were read.
    pub fn finish(mut self) -> Vec<RepoPath> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let mut paths = self
            .observed
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Reads events until told to stop.
fn collect(
    mut inotify: Inotify,
    by_descriptor: BTreeMap<i32, PathBuf>,
    tree: PathBuf,
    sink: Arc<Mutex<Vec<RepoPath>>>,
    stop: mpsc::Receiver<()>,
) {
    let mut buffer = [0u8; 8192];
    loop {
        if stop.try_recv().is_ok() {
            // Drain whatever arrived before the stop signal, so a read that
            // happened just before the task exited is not lost.
            drain(&mut inotify, &by_descriptor, &tree, &sink, &mut buffer);
            return;
        }
        drain(&mut inotify, &by_descriptor, &tree, &sink, &mut buffer);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn drain(
    inotify: &mut Inotify,
    by_descriptor: &BTreeMap<i32, PathBuf>,
    tree: &Path,
    sink: &Arc<Mutex<Vec<RepoPath>>>,
    buffer: &mut [u8],
) {
    let Ok(events) = inotify.read_events(buffer) else {
        return;
    };
    for event in events {
        let Some(name) = event.name else { continue };
        let Some(directory) = by_descriptor.get(&descriptor_id(&event.wd)) else {
            continue;
        };
        let absolute = directory.join(name);
        let Ok(relative) = absolute.strip_prefix(tree) else {
            continue;
        };
        let Ok(path) = RepoPath::parse(relative.to_string_lossy()) else {
            continue;
        };
        if let Ok(mut guard) = sink.lock() {
            guard.push(path);
        }
    }
}

/// `inotify`'s watch descriptor is opaque; its debug form carries the numeric
/// identifier, which is enough to key a map without reaching into internals.
fn descriptor_id(descriptor: &inotify::WatchDescriptor) -> i32 {
    format!("{descriptor:?}")
        .trim_matches(|c: char| !c.is_ascii_digit() && c != '-')
        .parse()
        .unwrap_or(-1)
}

/// Every directory in a tree, including the root.
fn directories(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let mut frontier = vec![root.to_path_buf()];
    while let Some(directory) = frontier.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                found.push(entry.path());
                frontier.push(entry.path());
            }
        }
    }
    found
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

    #[test]
    fn directories_are_enumerated_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("a/file.rs"), "x").unwrap();
        let found = directories(dir.path());
        assert_eq!(found.len(), 4, "{found:?}");
    }

    #[test]
    fn reads_under_the_tree_are_observed() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/lib.rs"), "// lib\n").unwrap();
        std::fs::write(tree.join("other.rs"), "// other\n").unwrap();

        let observation = Observation::start(&tree).expect("inotify is available on Linux");
        // Reading a file is what a build script doing include_str! looks like
        // from the outside.
        let _ = std::fs::read_to_string(tree.join("src/lib.rs")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let observed = observation.finish();

        assert!(
            observed.iter().any(|path| path.as_str() == "src/lib.rs"),
            "observed: {observed:?}"
        );
        assert!(
            !observed.iter().any(|path| path.as_str() == "other.rs"),
            "a file that was not read must not appear: {observed:?}"
        );
    }

    #[test]
    fn an_observation_with_no_reads_returns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let observation = Observation::start(&tree).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(observation.finish().is_empty());
    }

    #[test]
    fn unwatched_directories_are_reported_rather_than_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let observation = Observation::start(&tree).unwrap();
        // On an ordinary host nothing is unwatched; the accessor exists so an
        // incomplete observation cannot silently look complete.
        assert_eq!(observation.unwatched_directories(), 0);
        observation.finish();
    }
}
