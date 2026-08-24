//! The in-sandbox forwarder.
//!
//! Trusted code running in an untrusted network namespace. It listens on
//! `127.0.0.1:<port>` inside the sandbox and bridges each accepted connection to
//! the bind-mounted Unix socket, which is the only route out.
//!
//! It holds no policy: compromising it grants nothing beyond the allowlist the
//! host-side proxy already enforces.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

use crate::error::{EgressError, Result};

/// Forwarder configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwarderConfig {
    /// Loopback port to listen on inside the sandbox.
    pub port: u16,
    /// The bind-mounted proxy socket.
    pub socket: PathBuf,
}

/// The environment variables that point in-sandbox clients at the forwarder.
///
/// Returned as data rather than set here, because the sandbox environment is
/// built by the spec and cleared otherwise; a forwarder that mutated the
/// environment would be doing policy.
pub fn proxy_environment(port: u16) -> Vec<(String, String)> {
    let endpoint = format!("http://127.0.0.1:{port}");
    [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        // cargo reads its own variable in addition to the conventional ones.
        "CARGO_HTTP_PROXY",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), endpoint.clone()))
    .chain([(
        // Nothing bypasses the proxy: an empty no-proxy list is explicit, since
        // a value inherited from the host could carve out a destination.
        "no_proxy".to_owned(),
        String::new(),
    )])
    .collect()
}

/// Runs the forwarder until the process is terminated.
pub async fn run(config: ForwarderConfig) -> Result<()> {
    if !config.socket.exists() {
        return Err(EgressError::NoEgress);
    }
    let listener = TcpListener::bind(("127.0.0.1", config.port))
        .await
        .map_err(|error| EgressError::io("binding the forwarder listener", error))?;
    loop {
        let Ok((client, _)) = listener.accept().await else {
            continue;
        };
        let socket = config.socket.clone();
        tokio::spawn(async move {
            if let Err(error) = bridge(client, &socket).await {
                tracing_debug(&error);
            }
        });
    }
}

/// Bridges one accepted connection to the proxy socket.
async fn bridge(client: tokio::net::TcpStream, socket: &Path) -> Result<()> {
    let upstream = UnixStream::connect(socket)
        .await
        .map_err(|error| EgressError::io("connecting to the proxy socket", error))?;
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let outbound = copy(&mut client_read, &mut upstream_write);
    let inbound = copy(&mut upstream_read, &mut client_write);
    let _ = tokio::join!(outbound, inbound);
    Ok(())
}

async fn copy<R, W>(reader: &mut R, writer: &mut W)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; 32 * 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let slice = buffer.get(..read).unwrap_or(&[]);
        if writer.write_all(slice).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

/// The forwarder runs inside a sandbox with no logging infrastructure, so
/// failures are written to stderr, which the daemon captures.
fn tracing_debug(error: &EgressError) {
    eprintln!("clyde-forward: {error}");
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
    fn the_proxy_environment_points_only_at_loopback() {
        let env = proxy_environment(8118);
        let endpoint = "http://127.0.0.1:8118";
        for name in [
            "http_proxy",
            "https_proxy",
            "HTTPS_PROXY",
            "CARGO_HTTP_PROXY",
        ] {
            let value = env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str());
            assert_eq!(value, Some(endpoint), "{name}");
        }
    }

    #[test]
    fn nothing_bypasses_the_proxy() {
        let env = proxy_environment(8118);
        let no_proxy = env
            .iter()
            .find(|(key, _)| key == "no_proxy")
            .map(|(_, value)| value.as_str());
        assert_eq!(
            no_proxy,
            Some(""),
            "an inherited no_proxy could carve out a destination"
        );
    }

    #[tokio::test]
    async fn the_forwarder_refuses_to_start_without_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(ForwarderConfig {
            port: 0,
            socket: dir.path().join("absent.sock"),
        })
        .await
        .expect_err("no socket means no egress");
        assert!(matches!(error, EgressError::NoEgress));
    }

    #[tokio::test]
    async fn the_forwarder_bridges_bytes_to_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("egress.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 5];
            stream.read_exact(&mut buffer).await.unwrap();
            stream.write_all(b"pong!").await.unwrap();
            buffer
        });

        let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let socket = socket_path.clone();
        tokio::spawn(async move {
            let (client, _) = tcp.accept().await.unwrap();
            let _ = bridge(client, &socket).await;
        });

        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        client.write_all(b"ping!").await.unwrap();
        let mut response = [0u8; 5];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"pong!");
        assert_eq!(&server.await.unwrap(), b"ping!");
    }
}
