//! The broker gateway (Phase 4 deliverable 1).
//!
//! A single adapter through which all privileged external operations pass. It
//! translates a typed authority request into a broker call, attaches the
//! approval reference, and records the operation. **No other part of clyded may
//! call the broker**, which is why the socket path lives here and nowhere else.

use std::path::{Path, PathBuf};

use clyde_broker_api::transport::{read_message, write_message};
use clyde_broker_api::{
    BrokerRequest, BrokerResponse, Capabilities, GitPushRequest, RefusalReason,
};
use tokio::io::BufReader;
use tokio::net::UnixStream;

use crate::error::{DaemonError, Result};

/// The only path from clyded to the broker.
#[derive(Debug, Clone)]
pub struct BrokerGateway {
    socket: PathBuf,
}

impl BrokerGateway {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Whether the broker is reachable.
    ///
    /// Reported rather than assumed, so `clyde doctor` can say "publishing is
    /// unavailable" instead of a push failing later with a connection error.
    pub async fn is_available(&self) -> bool {
        UnixStream::connect(&self.socket).await.is_ok()
    }

    /// Asks the broker what it supports.
    pub async fn capabilities(&self) -> Result<Capabilities> {
        match self.call(BrokerRequest::Capabilities).await? {
            BrokerResponse::Capabilities(capabilities) => Ok(capabilities),
            BrokerResponse::Refused { reason } => Err(DaemonError::internal(reason.render())),
            BrokerResponse::Pushed(_) => Err(DaemonError::internal(
                "the broker answered a capability query with a push result",
            )),
        }
    }

    /// Performs a brokered push.
    ///
    /// The request carries the approval identifier and digest; the broker
    /// verifies both against its own read of the approval record rather than
    /// trusting this call.
    pub async fn git_push(
        &self,
        request: GitPushRequest,
    ) -> Result<std::result::Result<clyde_broker_api::PushOutcome, RefusalReason>> {
        match self.call(BrokerRequest::GitPush(Box::new(request))).await? {
            BrokerResponse::Pushed(outcome) => Ok(Ok(outcome)),
            BrokerResponse::Refused { reason } => Ok(Err(reason)),
            BrokerResponse::Capabilities(_) => Err(DaemonError::internal(
                "the broker answered a push with a capability list",
            )),
        }
    }

    async fn call(&self, request: BrokerRequest) -> Result<BrokerResponse> {
        let stream = UnixStream::connect(&self.socket).await.map_err(|error| {
            DaemonError::io(
                format!("connecting to the broker at {}", self.socket.display()),
                error,
            )
        })?;
        let (read_half, mut write_half) = stream.into_split();
        write_message(&mut write_half, &request)
            .await
            .map_err(|error| DaemonError::internal(format!("sending to the broker: {error}")))?;
        let mut reader = BufReader::new(read_half);
        read_message(&mut reader)
            .await
            .map_err(|error| DaemonError::internal(format!("reading from the broker: {error}")))
    }
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
    use clyde_broker_api::PushOutcome;
    use tokio::net::UnixListener;

    /// A stand-in broker that answers one request.
    async fn fake_broker(socket: PathBuf, response: BrokerResponse) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            // Serves repeatedly: a caller may probe availability before making
            // a request, and a single-shot server would race with that.
            while let Ok((stream, _)) = listener.accept().await {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let Ok(_request) = read_message::<BrokerRequest, _>(&mut reader).await else {
                    continue;
                };
                let _ = write_message(&mut write_half, &response).await;
            }
        })
    }

    #[tokio::test]
    async fn a_capability_query_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("brokerd.sock");
        let server = fake_broker(
            socket.clone(),
            BrokerResponse::Capabilities(Capabilities {
                version: "0.1.0".to_owned(),
                operations: vec!["git_push".to_owned()],
                holds_credential: false,
                credential_kind: None,
                remote_allowlist: vec![],
                branch_allowlist: vec![],
                protected_branches: vec!["main".to_owned()],
            }),
        )
        .await;

        let gateway = BrokerGateway::new(socket);
        assert!(gateway.is_available().await);
        let capabilities = gateway.capabilities().await.unwrap();
        assert_eq!(capabilities.operations, vec!["git_push".to_owned()]);
        server.abort();
    }

    #[tokio::test]
    async fn a_refusal_is_returned_as_a_value_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("brokerd.sock");
        let server = fake_broker(
            socket.clone(),
            BrokerResponse::Refused {
                reason: RefusalReason::ProtectedBranch {
                    branch: "main".to_owned(),
                },
            },
        )
        .await;

        let gateway = BrokerGateway::new(socket);
        let request = GitPushRequest {
            operation: clyde_core::ids::new::broker_op_id().unwrap(),
            mission: clyde_core::ids::new::mission_id().unwrap(),
            lease: clyde_core::ids::new::lease_id().unwrap(),
            approval: clyde_core::ids::new::approval_id().unwrap(),
            request_digest: clyde_core::Digest::of_bytes(b"x"),
            workspace: PathBuf::from("/srv/project"),
            remote: "origin".to_owned(),
            remote_url: "git@x:y.git".to_owned(),
            refspec: "refs/heads/main".to_owned(),
            commit: "a".repeat(40),
            expected_tree: "b".repeat(40),
        };
        let outcome = gateway.git_push(request).await.unwrap();
        // A refusal is policy information the caller records, not a transport
        // failure it should retry.
        assert!(matches!(
            outcome,
            Err(RefusalReason::ProtectedBranch { .. })
        ));
        server.abort();
    }

    #[tokio::test]
    async fn an_unreachable_broker_is_reported_rather_than_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = BrokerGateway::new(dir.path().join("absent.sock"));
        assert!(!gateway.is_available().await);
        assert!(gateway.capabilities().await.is_err());
    }

    #[tokio::test]
    async fn a_mismatched_answer_is_an_internal_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("brokerd.sock");
        let server = fake_broker(
            socket.clone(),
            BrokerResponse::Pushed(PushOutcome {
                operation: clyde_core::ids::new::broker_op_id().unwrap(),
                commit: "a".repeat(40),
                refspec: "refs/heads/main".to_owned(),
                summary: "pushed".to_owned(),
            }),
        )
        .await;
        let gateway = BrokerGateway::new(socket);
        assert!(gateway.capabilities().await.is_err());
        server.abort();
    }
}
