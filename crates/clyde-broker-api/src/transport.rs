//! Newline-delimited framing for the broker socket.
//!
//! Small on purpose. The broker is a separate process precisely so that its
//! surface is narrow, and a full RPC stack would be a larger trusted surface
//! than one request type justifies.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Largest broker message. A push request is small; anything larger is a fault.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the peer closed the connection")]
    Closed,
    #[error("a message exceeded the {MAX_MESSAGE_BYTES}-byte limit")]
    TooLarge,
    #[error("input/output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("a message could not be decoded: {0}")]
    Malformed(String),
}

/// Reads one newline-delimited message.
pub async fn read_message<T, R>(reader: &mut BufReader<R>) -> Result<T, TransportError>
where
    T: DeserializeOwned,
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Err(TransportError::Closed);
    }
    if read > MAX_MESSAGE_BYTES {
        return Err(TransportError::TooLarge);
    }
    serde_json::from_str(line.trim()).map_err(|error| TransportError::Malformed(error.to_string()))
}

/// Writes one newline-delimited message.
pub async fn write_message<T, W>(writer: &mut W, message: &T) -> Result<(), TransportError>
where
    T: Serialize,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(message)
        .map_err(|error| TransportError::Malformed(error.to_string()))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(TransportError::TooLarge);
    }
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
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
    use crate::BrokerRequest;

    #[tokio::test]
    async fn a_message_round_trips() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &BrokerRequest::Capabilities)
            .await
            .unwrap();
        let mut reader = BufReader::new(buffer.as_slice());
        let back: BrokerRequest = read_message(&mut reader).await.unwrap();
        assert_eq!(back, BrokerRequest::Capabilities);
    }

    #[tokio::test]
    async fn a_closed_connection_is_distinguishable_from_a_fault() {
        let mut reader = BufReader::new(&b""[..]);
        let error = read_message::<BrokerRequest, _>(&mut reader)
            .await
            .expect_err("closed");
        assert!(matches!(error, TransportError::Closed));
    }

    #[tokio::test]
    async fn malformed_messages_are_refused() {
        let mut reader = BufReader::new(&b"not json\n"[..]);
        let error = read_message::<BrokerRequest, _>(&mut reader)
            .await
            .expect_err("malformed");
        assert!(matches!(error, TransportError::Malformed(_)));
    }

    #[tokio::test]
    async fn an_oversized_message_is_refused_rather_than_buffered() {
        let huge = format!("{}\n", "a".repeat(MAX_MESSAGE_BYTES + 1));
        let mut reader = BufReader::new(huge.as_bytes());
        assert!(matches!(
            read_message::<BrokerRequest, _>(&mut reader).await,
            Err(TransportError::TooLarge)
        ));
    }
}
