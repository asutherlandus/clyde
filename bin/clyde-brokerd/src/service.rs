//! The broker's socket service.
//!
//! Listens on `brokerd.sock`, mode `0600`, never mounted into any sandbox, and
//! accepts requests only from clyded — verified with `SO_PEERCRED` rather than
//! taken on trust.

use std::path::Path;
use std::sync::Arc;

use clyde_broker_api::transport::{TransportError, read_message, write_message};
use clyde_broker_api::{BrokerRequest, BrokerResponse, RefusalReason};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};

use crate::Broker;

/// Binds the broker socket.
pub async fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket that answers belongs to a live broker; one that does not is stale.
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another broker is already listening",
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serves one connection.
pub async fn serve(broker: Arc<Broker>, stream: UnixStream) {
    // The broker accepts requests only from clyded. The mode already excludes
    // other users; this is the check that does not depend on a path.
    let permitted = stream
        .peer_cred()
        .ok()
        .is_some_and(|credentials| Some(credentials.uid()) == current_uid());
    let (read_half, mut write_half) = stream.into_split();
    if !permitted {
        let _ = write_message(
            &mut write_half,
            &BrokerResponse::Refused {
                reason: RefusalReason::UntrustedCaller {
                    detail: "the peer is not the daemon's user".to_owned(),
                },
            },
        )
        .await;
        return;
    }

    let mut reader = BufReader::new(read_half);
    loop {
        let request: BrokerRequest = match read_message(&mut reader).await {
            Ok(request) => request,
            Err(TransportError::Closed) => break,
            Err(error) => {
                let _ = write_message(
                    &mut write_half,
                    &BrokerResponse::Refused {
                        reason: RefusalReason::MalformedRequest {
                            detail: error.to_string(),
                        },
                    },
                )
                .await;
                break;
            }
        };
        let response = broker.handle(request).await;
        if write_message(&mut write_half, &response).await.is_err() {
            break;
        }
    }
}

/// Matches a branch against a pattern list.
///
/// Literal, or a trailing `*`. A full glob language in a security control is a
/// source of surprises.
pub fn matches_pattern(patterns: &std::collections::BTreeSet<String>, branch: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => branch.starts_with(prefix),
            None => pattern.as_str() == branch,
        })
}

/// The current user's id, read from `/proc/self` so the crate needs no `unsafe`.
fn current_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
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
    use std::collections::BTreeSet;

    fn patterns(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn patterns_match_literally_or_by_prefix() {
        let allowed = patterns(&["feature/*", "hotfix"]);
        assert!(matches_pattern(&allowed, "feature/x"));
        assert!(matches_pattern(&allowed, "hotfix"));
        assert!(!matches_pattern(&allowed, "hotfixes"));
        assert!(!matches_pattern(&allowed, "main"));
    }

    #[tokio::test]
    async fn binding_restricts_the_socket() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("brokerd.sock");
        let listener = bind(&socket).await.unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the broker socket must be owner-only");
        drop(listener);
    }

    #[tokio::test]
    async fn a_stale_socket_is_replaced_and_a_live_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("brokerd.sock");
        std::fs::write(&socket, b"stale").unwrap();
        let listener = bind(&socket).await.expect("a stale socket is replaced");
        assert!(
            bind(&socket).await.is_err(),
            "a live broker must not be displaced"
        );
        drop(listener);
    }
}
