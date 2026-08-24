//! JSON-RPC clients for the two sockets.
//!
//! The CLI is also the integration-test harness, so `--json` output must be
//! stable from Phase 1 (D13). That stability comes from returning the daemon's
//! own JSON unchanged rather than reformatting it here.

use std::path::PathBuf;

use clyde_api::codec::{RequestReader, ResponseWriter};
use clyde_api::jsonrpc::{Id, Request, Response};
use tokio::net::UnixStream;

/// Errors talking to a daemon.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not reach {socket:?}: {source}\nIs clyded running?")]
    Unreachable {
        socket: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the daemon closed the connection")]
    Closed,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{message}")]
    Rpc {
        message: String,
        code: i32,
        data: Option<serde_json::Value>,
    },
}

impl ClientError {
    /// The structured denial data, where the failure was a policy denial.
    pub fn denial(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Rpc { code, data, .. } if *code == clyde_api::jsonrpc::codes::POLICY_DENIED => {
                data.as_ref()
            }
            _ => None,
        }
    }
}

/// A connection to one of the daemon's sockets.
#[derive(Debug)]
pub struct Client {
    socket: PathBuf,
    next_id: i64,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            next_id: 1,
        }
    }

    /// Sends one request and returns its result.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let stream =
            UnixStream::connect(&self.socket)
                .await
                .map_err(|source| ClientError::Unreachable {
                    socket: self.socket.clone(),
                    source,
                })?;
        let (read_half, write_half) = stream.into_split();
        let mut reader = RequestReader::new(read_half);
        let mut writer = ResponseWriter::new(write_half);

        let id = Id::Number(self.next_id);
        self.next_id += 1;
        let request = Request::new(id, method, params);
        // The codec is shared with the daemon, so the CLI exercises the same
        // framing an agent does.
        write_request(&mut writer, &request).await?;
        let response = read_response(&mut reader).await?;
        into_result(response)
    }

    /// Sends a request on an already-open connection, for the actor surface
    /// where the connection carries the session.
    pub async fn session(&self, token: &str) -> Result<SessionClient, ClientError> {
        let stream =
            UnixStream::connect(&self.socket)
                .await
                .map_err(|source| ClientError::Unreachable {
                    socket: self.socket.clone(),
                    source,
                })?;
        let (read_half, write_half) = stream.into_split();
        let mut client = SessionClient {
            reader: RequestReader::new(read_half),
            writer: ResponseWriter::new(write_half),
            next_id: 1,
        };
        client
            .call("initialize", serde_json::json!({"token": token}))
            .await?;
        Ok(client)
    }
}

/// A connection that has presented a session token.
#[derive(Debug)]
pub struct SessionClient {
    reader: RequestReader<tokio::net::unix::OwnedReadHalf>,
    writer: ResponseWriter<tokio::net::unix::OwnedWriteHalf>,
    next_id: i64,
}

impl SessionClient {
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = Id::Number(self.next_id);
        self.next_id += 1;
        let request = Request::new(id, method, params);
        write_request(&mut self.writer, &request).await?;
        let response = read_response(&mut self.reader).await?;
        into_result(response)
    }

    /// Calls an MCP tool.
    pub async fn tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        self.call(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        )
        .await
    }
}

async fn write_request<W>(
    writer: &mut ResponseWriter<W>,
    request: &Request,
) -> Result<(), ClientError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    // The same codec the daemon uses, so the CLI exercises the framing an agent
    // does rather than a second implementation that could drift from it.
    writer
        .send_value(request)
        .await
        .map_err(|error| ClientError::Protocol(error.to_string()))
}

async fn read_response<R>(reader: &mut RequestReader<R>) -> Result<Response, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    reader
        .next_json::<Response>()
        .await
        .map_err(|error| match error {
            clyde_api::codec::CodecError::Closed => ClientError::Closed,
            other => ClientError::Protocol(other.to_string()),
        })
}

fn into_result(response: Response) -> Result<serde_json::Value, ClientError> {
    if let Some(error) = response.error {
        return Err(ClientError::Rpc {
            message: error.message,
            code: error.code,
            data: error.data,
        });
    }
    Ok(response.result.unwrap_or(serde_json::Value::Null))
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

    #[tokio::test]
    async fn an_absent_socket_says_so_and_suggests_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = Client::new(dir.path().join("absent.sock"));
        let error = client
            .call("doctor", serde_json::json!({}))
            .await
            .expect_err("no daemon");
        assert!(error.to_string().contains("Is clyded running?"));
    }

    #[test]
    fn a_denial_exposes_its_structured_data() {
        let error = ClientError::Rpc {
            message: "denied".to_owned(),
            code: clyde_api::jsonrpc::codes::POLICY_DENIED,
            data: Some(serde_json::json!({"alternatives": ["ask"]})),
        };
        assert!(error.denial().is_some());
        let other = ClientError::Rpc {
            message: "internal".to_owned(),
            code: clyde_api::jsonrpc::codes::INTERNAL_ERROR,
            data: None,
        };
        assert!(other.denial().is_none());
    }
}
