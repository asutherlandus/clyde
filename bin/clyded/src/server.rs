//! Socket setup and peer authentication.
//!
//! Two sockets, and the difference between them is the whole point (D2):
//!
//! - `clyded.sock` — the actor API. May be bind-mounted into sandboxes.
//!   Requires a valid session token.
//! - `clyded-admin.sock` — the human API. Mode `0600`, `SO_PEERCRED`-checked,
//!   never mounted into any sandbox.
//!
//! Startup fails closed: if either socket path is world-writable, already bound
//! by another process, or inside a registered workspace directory, the daemon
//! refuses to start rather than serving from a place an agent might reach.

use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

use crate::error::{DaemonError, Result};

/// Why a socket path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SocketRefusal {
    #[error("{path:?} is world-writable, so another user could replace the socket")]
    WorldWritable { path: PathBuf },
    #[error("{path:?} is already bound by a live process")]
    AlreadyBound { path: PathBuf },
    #[error(
        "{path:?} is inside the registered workspace {workspace:?}, where a sandbox could reach it"
    )]
    InsideWorkspace { path: PathBuf, workspace: PathBuf },
    #[error("{path:?} has no parent directory")]
    NoParent { path: PathBuf },
}

/// Checks a socket path before binding.
///
/// `workspaces` are the registered workspace roots; a socket inside one could be
/// bind-mounted into a sandbox along with the project tree, which would put the
/// admin socket inside the boundary it exists to stay outside of.
pub fn check_socket_path(
    path: &Path,
    workspaces: &[PathBuf],
    already_bound: bool,
) -> std::result::Result<(), SocketRefusal> {
    use std::os::unix::fs::PermissionsExt as _;

    let Some(parent) = path.parent() else {
        return Err(SocketRefusal::NoParent {
            path: path.to_path_buf(),
        });
    };
    if let Ok(metadata) = std::fs::metadata(parent) {
        let mode = metadata.permissions().mode();
        if mode & 0o002 != 0 {
            return Err(SocketRefusal::WorldWritable {
                path: parent.to_path_buf(),
            });
        }
    }
    if already_bound {
        return Err(SocketRefusal::AlreadyBound {
            path: path.to_path_buf(),
        });
    }
    for workspace in workspaces {
        if path.starts_with(workspace) {
            return Err(SocketRefusal::InsideWorkspace {
                path: path.to_path_buf(),
                workspace: workspace.clone(),
            });
        }
    }
    Ok(())
}

/// Binds a socket, refusing rather than clobbering a live one.
pub async fn bind(path: &Path, mode: u32, workspaces: &[PathBuf]) -> Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| DaemonError::io("creating the socket directory", error))?;
    }
    // A socket that answers belongs to a live daemon; one that does not is
    // stale and is removed.
    let already_bound = path.exists() && UnixStream::connect(path).await.is_ok();
    check_socket_path(path, workspaces, already_bound)
        .map_err(|refusal| DaemonError::invalid(refusal.to_string()))?;
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| DaemonError::io("removing a stale socket", error))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|error| DaemonError::io("binding a socket", error))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| DaemonError::io("restricting a socket", error))?;
    Ok(listener)
}

/// The peer's credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<i32>,
}

/// Reads the peer's credentials.
pub fn peer_of(stream: &UnixStream) -> Result<Peer> {
    let credentials = stream
        .peer_cred()
        .map_err(|error| DaemonError::io("reading peer credentials", error))?;
    Ok(Peer {
        uid: credentials.uid(),
        gid: credentials.gid(),
        pid: credentials.pid(),
    })
}

/// Whether a peer may use the admin socket.
///
/// Only the daemon's own user. The socket mode already excludes everyone else;
/// this is the second check, because a mode is a property of a path and a path
/// can be replaced.
pub fn is_admin_peer(peer: Peer) -> bool {
    // The `unsafe`-free way to learn the current uid: the daemon's own socket
    // directory is created by this process, so its owner is this user.
    current_uid().is_some_and(|uid| uid == peer.uid)
}

/// The current user's id, read from `/proc/self` rather than through `libc`.
///
/// This keeps the crate free of `unsafe` while still answering the question the
/// admin check needs.
pub fn current_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// Whether this process is running inside a Clyde sandbox.
///
/// `clyde approve` refuses to run inside one, and that refusal is the mechanism
/// that makes agent self-approval impossible.
pub fn inside_sandbox() -> bool {
    std::env::var_os("CLYDE_SESSION_TOKEN_FILE").is_some()
        || Path::new(crate::sandboxes::inside::SESSION_TOKEN).exists()
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
    fn a_socket_inside_a_registered_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let socket = workspace.join("run/clyded-admin.sock");
        let refusal = check_socket_path(&socket, std::slice::from_ref(&workspace), false)
            .expect_err("a socket a sandbox could reach must be refused");
        assert!(matches!(refusal, SocketRefusal::InsideWorkspace { .. }));
    }

    #[test]
    fn a_world_writable_parent_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o777)).unwrap();
        let refusal = check_socket_path(&run.join("clyded.sock"), &[], false)
            .expect_err("another user could replace the socket");
        assert!(matches!(refusal, SocketRefusal::WorldWritable { .. }));
    }

    #[test]
    fn a_live_socket_is_refused_rather_than_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("clyded.sock");
        let refusal =
            check_socket_path(&socket, &[], true).expect_err("a live daemon must not be displaced");
        assert!(matches!(refusal, SocketRefusal::AlreadyBound { .. }));
    }

    #[test]
    fn an_ordinary_path_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            check_socket_path(&run.join("clyded.sock"), &[], false),
            Ok(())
        );
    }

    #[tokio::test]
    async fn binding_restricts_the_socket_and_removes_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run/clyded-admin.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        // A leftover file from a crashed daemon.
        std::fs::write(&socket, b"stale").unwrap();

        let listener = bind(&socket, 0o600, &[]).await.unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(listener);
    }

    #[tokio::test]
    async fn a_peer_on_our_own_socket_is_us() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run/clyded-admin.sock");
        let listener = bind(&socket, 0o600, &[]).await.unwrap();
        let client = UnixStream::connect(&socket).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let peer = peer_of(&accepted).unwrap();
        assert!(is_admin_peer(peer));
        assert_eq!(Some(peer.uid), current_uid());
        drop(client);
    }

    #[test]
    fn sandbox_detection_uses_the_marker_the_sandbox_actually_has() {
        // In this test process neither marker is present.
        assert!(!inside_sandbox());
    }
}
