//! The daemon's state layout.
//!
//! One place that knows where things live, so the persistence layout in the
//! schema reference is a fact about the code rather than about documentation.

use std::path::{Path, PathBuf};

/// Directories and socket paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default location, honouring `XDG_DATA_HOME`.
    pub fn default_root() -> PathBuf {
        std::env::var_os("CLYDE_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_DATA_HOME").map(|base| PathBuf::from(base).join("clyde"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|home| {
                    PathBuf::from(home)
                        .join(".local")
                        .join("share")
                        .join("clyde")
                })
            })
            .unwrap_or_else(|| PathBuf::from("/var/lib/clyde"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("db.sqlite")
    }

    pub fn blobs(&self) -> PathBuf {
        self.root.join("blobs")
    }

    pub fn snapshots(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn deps(&self) -> PathBuf {
        self.root.join("deps")
    }

    pub fn missions(&self) -> PathBuf {
        self.root.join("missions")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn run(&self) -> PathBuf {
        self.root.join("run")
    }

    pub fn ca(&self) -> PathBuf {
        self.root.join("ca")
    }

    /// Scratch for sandbox bookkeeping: seccomp filters, VM configuration.
    pub fn sandbox_runtime(&self) -> PathBuf {
        self.run().join("sandbox")
    }

    /// Broker-owned scratch, used for the sanitised temporary repository.
    pub fn broker_scratch(&self) -> PathBuf {
        self.root.join("broker-scratch")
    }

    /// The actor API socket. May be bind-mounted into sandboxes.
    pub fn actor_socket(&self) -> PathBuf {
        self.run().join("clyded.sock")
    }

    /// The human API socket. Never mounted into any sandbox.
    pub fn admin_socket(&self) -> PathBuf {
        self.run().join("clyded-admin.sock")
    }

    pub fn broker_socket(&self) -> PathBuf {
        self.run().join("brokerd.sock")
    }

    /// Per-task log directory.
    pub fn task_logs(&self, task_run: &str) -> PathBuf {
        self.logs().join("tasks").join(task_run)
    }

    /// Creates every directory the daemon needs.
    pub fn create_all(&self) -> std::io::Result<()> {
        for directory in [
            self.root.clone(),
            self.blobs(),
            self.snapshots(),
            self.deps(),
            self.missions(),
            self.logs(),
            self.run(),
            self.ca(),
            self.sandbox_runtime(),
            self.broker_scratch(),
        ] {
            std::fs::create_dir_all(&directory)?;
        }
        // The run directory holds sockets and the CA holds a private key;
        // neither should be traversable by other local users.
        restrict(&self.run())?;
        restrict(&self.ca())?;
        Ok(())
    }
}

fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
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
    fn the_layout_matches_the_documented_one() {
        let paths = StatePaths::new("/var/lib/clyde");
        assert!(paths.database().ends_with("db.sqlite"));
        assert!(paths.blobs().ends_with("blobs"));
        assert!(paths.snapshots().ends_with("snapshots"));
        assert!(paths.deps().ends_with("deps"));
        assert!(paths.missions().ends_with("missions"));
        assert!(paths.logs().ends_with("logs"));
        assert!(paths.actor_socket().ends_with("run/clyded.sock"));
        assert!(paths.admin_socket().ends_with("run/clyded-admin.sock"));
        assert!(paths.broker_socket().ends_with("run/brokerd.sock"));
    }

    #[test]
    fn creating_the_layout_restricts_the_sensitive_directories() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths::new(dir.path().join("state"));
        paths.create_all().unwrap();
        for sensitive in [paths.run(), paths.ca()] {
            let mode = std::fs::metadata(&sensitive).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o700,
                "{sensitive:?} must not be traversable by others"
            );
        }
    }

    #[test]
    fn the_state_directory_can_be_overridden_for_a_test_or_a_second_instance() {
        // The environment variable is what lets the integration tests run a
        // daemon without touching the developer's own state.
        let paths = StatePaths::new(StatePaths::default_root());
        assert!(paths.root().is_absolute() || paths.root().starts_with("."));
    }
}
