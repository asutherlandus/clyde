//! Newline-delimited JSON-RPC framing.
//!
//! The actor surface speaks newline-delimited JSON-RPC directly over the socket,
//! treating it as a duplex stream — which is what MCP's stdio transport already
//! is (D19). Message size limits and backpressure are the codec's
//! responsibility, and are explicit here, because an actor is untrusted input.

use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::jsonrpc::{Error, Request, Response};

/// Largest message accepted, in bytes.
///
/// Generous enough for a task's log range and far below anything that would let
/// a client exhaust the daemon's memory.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Codec errors.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("the peer closed the connection")]
    Closed,
    #[error("a message exceeded the {MAX_MESSAGE_BYTES}-byte limit")]
    TooLarge,
    #[error("input/output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("a message was not valid JSON: {0}")]
    Malformed(String),
}

impl CodecError {
    /// The JSON-RPC error a malformed message should produce.
    pub fn to_rpc_error(&self) -> Error {
        match self {
            Self::TooLarge => Error::invalid_request(self.to_string()),
            Self::Malformed(detail) => Error::parse_error(detail.clone()),
            Self::Closed | Self::Io(_) => Error::internal(self.to_string()),
        }
    }
}

/// Reads newline-delimited requests from a stream.
#[derive(Debug)]
pub struct RequestReader<R> {
    inner: BufReader<R>,
    line: String,
}

impl<R: tokio::io::AsyncRead + Unpin> RequestReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
            line: String::new(),
        }
    }

    /// Reads the next request.
    ///
    /// Blank lines are skipped rather than treated as errors, since a client
    /// that flushes an empty line is not misbehaving.
    pub async fn next(&mut self) -> Result<Request, CodecError> {
        loop {
            self.line.clear();
            let read = self.inner.read_line(&mut self.line).await?;
            if read == 0 {
                return Err(CodecError::Closed);
            }
            if read > MAX_MESSAGE_BYTES {
                return Err(CodecError::TooLarge);
            }
            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed)
                .map_err(|error| CodecError::Malformed(error.to_string()));
        }
    }
}

/// Writes newline-delimited responses.
///
/// Each response is written and flushed as one unit, so a slow reader applies
/// backpressure to the writer rather than causing interleaved output.
#[derive(Debug)]
pub struct ResponseWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> ResponseWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { inner: writer }
    }

    pub async fn send(&mut self, response: &Response) -> Result<(), CodecError> {
        let mut encoded = serde_json::to_vec(response)
            .map_err(|error| CodecError::Malformed(error.to_string()))?;
        if encoded.len() > MAX_MESSAGE_BYTES {
            // A response too large to frame is replaced by an error rather than
            // truncated, because a truncated JSON line is indistinguishable from
            // a protocol fault.
            let replacement = Response::failure(
                response.id.clone(),
                Error::internal("the response exceeded the message size limit"),
            );
            encoded = serde_json::to_vec(&replacement)
                .map_err(|error| CodecError::Malformed(error.to_string()))?;
        }
        encoded.push(b'\n');
        self.inner.write_all(&encoded).await?;
        self.inner.flush().await?;
        Ok(())
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
    use crate::jsonrpc::Id;

    #[tokio::test]
    async fn requests_are_read_line_by_line() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"a"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"b"}"#,
            "\n"
        );
        let mut reader = RequestReader::new(input.as_bytes());
        assert_eq!(reader.next().await.unwrap().method, "a");
        assert_eq!(reader.next().await.unwrap().method, "b");
        assert!(matches!(reader.next().await, Err(CodecError::Closed)));
    }

    #[tokio::test]
    async fn blank_lines_are_skipped() {
        let input = "\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"a\"}\n";
        let mut reader = RequestReader::new(input.as_bytes());
        assert_eq!(reader.next().await.unwrap().method, "a");
    }

    #[tokio::test]
    async fn malformed_json_produces_a_parse_error_not_a_disconnect() {
        let mut reader = RequestReader::new(&b"not json\n"[..]);
        let error = reader.next().await.expect_err("must not parse");
        assert!(matches!(error, CodecError::Malformed(_)));
        assert_eq!(
            error.to_rpc_error().code,
            crate::jsonrpc::codes::PARSE_ERROR
        );
    }

    #[tokio::test]
    async fn an_oversized_message_is_refused_rather_than_buffered() {
        let huge = format!("{}\n", "a".repeat(MAX_MESSAGE_BYTES + 1));
        let mut reader = RequestReader::new(huge.as_bytes());
        assert!(matches!(reader.next().await, Err(CodecError::TooLarge)));
    }

    #[tokio::test]
    async fn responses_are_newline_framed() {
        let mut buffer = Vec::new();
        let mut writer = ResponseWriter::new(&mut buffer);
        writer
            .send(&Response::success(
                Some(Id::Number(1)),
                serde_json::json!({"ok": true}),
            ))
            .await
            .unwrap();
        writer
            .send(&Response::success(
                Some(Id::Number(2)),
                serde_json::json!({"ok": false}),
            ))
            .await
            .unwrap();
        let text = String::from_utf8(buffer).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
    }

    #[tokio::test]
    async fn an_oversized_response_becomes_an_error_rather_than_a_truncated_line() {
        let mut buffer = Vec::new();
        let mut writer = ResponseWriter::new(&mut buffer);
        let huge = serde_json::json!({"logs": "x".repeat(MAX_MESSAGE_BYTES)});
        writer
            .send(&Response::success(Some(Id::Number(1)), huge))
            .await
            .unwrap();
        let text = String::from_utf8(buffer).unwrap();
        assert_eq!(text.lines().count(), 1);
        let parsed: Response = serde_json::from_str(text.trim()).unwrap();
        assert!(
            parsed.is_error(),
            "a truncated JSON line would be indistinguishable from a fault"
        );
    }
}
