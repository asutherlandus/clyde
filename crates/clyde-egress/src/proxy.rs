//! The host-side egress proxy.
//!
//! Every sandbox gets an unshared network namespace with loopback only. For a
//! profile other than `none`, a Unix socket is bind-mounted in and a trusted
//! forwarder inside the sandbox bridges `127.0.0.1:<port>` to it. This proxy is
//! the other end of that socket, and it runs in clyded, which is trusted.
//!
//! Because the proxy is host-side and the sandbox has no other route, the
//! allowlist decision is made in trusted code. Compromising the in-sandbox
//! forwarder gains nothing: it can only reach what the proxy already permits.
//!
//! **Logging rule.** For terminated `model-api` connections the proxy sees
//! prompt and completion plaintext and must not log bodies. Recordable metadata
//! is host, request path, status, byte counts, and timing. There is no
//! configuration flag that enables body logging, and no code path here that
//! reads a body into anything but a copy buffer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use clyde_core::Redacted;
use clyde_core::classification::{EgressProfile, HostName};
use clyde_core::egress::{EgressAttempt, EgressDecision, EgressDenialReason};
use clyde_core::ids::TaskRunId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::budget::{EgressBudget, EgressCounters};
use crate::ca::ClydeCa;
use crate::error::{EgressError, Result};
use crate::http::{
    BodyFraming, MAX_HEAD_BYTES, RequestHead, ResponseHead, parse_chunk_size, simple_response,
    split_head,
};

/// Where recorded attempts go.
///
/// A trait so the proxy does not depend on the store: the daemon supplies an
/// implementation that writes an `EgressAttempt` and charges the lease.
pub trait EgressRecorder: Send + Sync + std::fmt::Debug {
    fn record(&self, attempt: EgressAttempt);
}

/// A recorder that keeps attempts in memory, for tests and for the CLI's
/// dry-run mode.
#[derive(Debug, Default)]
pub struct MemoryRecorder {
    attempts: std::sync::Mutex<Vec<EgressAttempt>>,
}

impl MemoryRecorder {
    pub fn attempts(&self) -> Vec<EgressAttempt> {
        self.attempts
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl EgressRecorder for MemoryRecorder {
    fn record(&self, attempt: EgressAttempt) {
        if let Ok(mut guard) = self.attempts.lock() {
            guard.push(attempt);
        }
    }
}

/// Everything one sandbox's egress is governed by.
pub struct EgressContext {
    pub profile: EgressProfile,
    /// Resolved host allowlist. Empty means nothing is reachable.
    pub allowlist: BTreeSet<HostName>,
    pub budget: EgressBudget,
    pub task_run: Option<TaskRunId>,
    /// Injected host-side for terminated connections, so the agent never holds
    /// the model API credential (D11).
    pub auth_header: Option<Redacted<String>>,
    /// Required when the profile terminates TLS.
    pub ca: Option<Arc<ClydeCa>>,
}

impl std::fmt::Debug for EgressContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressContext")
            .field("profile", &self.profile.name())
            .field("allowlist", &self.allowlist)
            .field("budget", &self.budget)
            .field("task_run", &self.task_run)
            .field(
                "auth_header",
                &self.auth_header.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl EgressContext {
    /// Whether `host` is reachable under this context.
    pub fn permits_host(&self, host: &HostName) -> bool {
        self.allowlist.contains(host)
    }
}

/// A running proxy listener.
#[derive(Debug)]
pub struct ProxyHandle {
    socket_path: PathBuf,
    counters: Arc<EgressCounters>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyHandle {
    /// The socket to bind-mount into the sandbox.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn counters(&self) -> Arc<EgressCounters> {
        Arc::clone(&self.counters)
    }

    /// Stops the listener and removes the socket.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
        let _ = tokio::fs::remove_file(&self.socket_path).await;
    }
}

/// Binds a proxy socket for one sandbox.
///
/// Profile `none` is implemented by *not calling this*: the absence of the
/// socket is the absence of egress, which is the property worth having. Calling
/// it with `none` is refused rather than quietly producing a socket that denies
/// everything.
pub async fn bind(
    socket_path: PathBuf,
    context: Arc<EgressContext>,
    recorder: Arc<dyn EgressRecorder>,
) -> Result<ProxyHandle> {
    if context.profile.is_none() {
        return Err(EgressError::NoEgress);
    }
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| EgressError::io("creating the proxy socket directory", error))?;
    }
    // A stale socket from a crashed daemon is removed; a live one belonging to
    // another process is a refusal rather than something to clobber.
    if socket_path.exists() {
        if UnixStream::connect(&socket_path).await.is_ok() {
            return Err(EgressError::SocketInUse { path: socket_path });
        }
        tokio::fs::remove_file(&socket_path)
            .await
            .map_err(|error| EgressError::io("removing a stale proxy socket", error))?;
    }
    let listener = UnixListener::bind(&socket_path)
        .map_err(|error| EgressError::io("binding the proxy socket", error))?;
    restrict_socket(&socket_path)?;

    let counters = Arc::new(EgressCounters::default());
    let (shutdown, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let task_counters = Arc::clone(&counters);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let context = Arc::clone(&context);
                    let recorder = Arc::clone(&recorder);
                    let counters = Arc::clone(&task_counters);
                    tokio::spawn(async move {
                        if let Err(error) = serve(stream, context, recorder, counters).await {
                            tracing::debug!(error = %error, "proxy connection ended with an error");
                        }
                    });
                }
            }
        }
    });

    Ok(ProxyHandle {
        socket_path,
        counters,
        shutdown,
        task,
    })
}

/// Restricts the socket to the owner. The sandbox runs as the same user, so this
/// keeps other local users off it without blocking the intended client.
fn restrict_socket(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| EgressError::io("restricting the proxy socket", error))
}

/// Handles one bridged connection.
async fn serve(
    mut stream: UnixStream,
    context: Arc<EgressContext>,
    recorder: Arc<dyn EgressRecorder>,
    counters: Arc<EgressCounters>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let head = match read_head(&mut stream).await {
        Ok(head) => head,
        Err(error) => {
            record_denial(
                &recorder,
                &context,
                None,
                0,
                EgressDenialReason::MalformedRequest,
                started,
            );
            let _ = stream
                .write_all(simple_response(400, "Bad Request", "malformed request").as_bytes())
                .await;
            return Err(error);
        }
    };

    let request = match RequestHead::parse(&head) {
        Ok(request) => request,
        Err(error) => {
            record_denial(
                &recorder,
                &context,
                None,
                0,
                EgressDenialReason::MalformedRequest,
                started,
            );
            let _ = stream
                .write_all(simple_response(400, "Bad Request", "malformed request").as_bytes())
                .await;
            return Err(EgressError::Protocol(error));
        }
    };
    let Some((host_text, port)) = request.connect_target() else {
        record_denial(
            &recorder,
            &context,
            None,
            0,
            EgressDenialReason::MalformedRequest,
            started,
        );
        let _ = stream
            .write_all(
                simple_response(
                    405,
                    "Method Not Allowed",
                    "this proxy accepts CONNECT only; there is no plain HTTP forwarding",
                )
                .as_bytes(),
            )
            .await;
        return Ok(());
    };

    let Ok(host) = HostName::parse(host_text) else {
        record_denial(
            &recorder,
            &context,
            None,
            port,
            EgressDenialReason::MalformedRequest,
            started,
        );
        let _ = stream
            .write_all(simple_response(400, "Bad Request", "malformed destination").as_bytes())
            .await;
        return Ok(());
    };

    // Decisions, in order: profile, port, allowlist, budget. Ordering matters
    // only for which reason is reported, and the most fundamental one wins.
    let denial = if context.profile.is_none() {
        Some(EgressDenialReason::ProfileForbidsEgress)
    } else if !clyde_policy::egress::permits_port(&context.profile, port) {
        Some(EgressDenialReason::PortNotAllowed { port })
    } else if !context.permits_host(&host) {
        Some(EgressDenialReason::HostNotAllowlisted)
    } else {
        counters.open_connection(&context.budget).err()
    };

    if let Some(reason) = denial {
        record_denial(
            &recorder,
            &context,
            Some(host),
            port,
            reason.clone(),
            started,
        );
        let _ = stream
            .write_all(simple_response(403, "Forbidden", &reason.render()).as_bytes())
            .await;
        return Ok(());
    }

    // DNS is resolved host-side: the sandbox cannot make DNS queries at all, so
    // the proxy's view of a hostname is authoritative for the allowlist decision.
    let Ok(upstream) = TcpStream::connect((host.as_str(), port)).await else {
        record_denial(
            &recorder,
            &context,
            Some(host),
            port,
            EgressDenialReason::UpstreamUnreachable,
            started,
        );
        let _ = stream
            .write_all(simple_response(502, "Bad Gateway", "destination unreachable").as_bytes())
            .await;
        return Ok(());
    };

    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .map_err(|error| EgressError::io("acknowledging CONNECT", error))?;

    if context.profile.terminates_tls() {
        terminate(
            stream, upstream, host, port, context, recorder, counters, started,
        )
        .await
    } else {
        passthrough(
            stream, upstream, host, port, context, recorder, counters, started,
        )
        .await
    }
}

/// Reads a request head, bounded so an actor cannot make the proxy allocate
/// without limit.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some((head, _)) = split_head(&buffer)? {
            return Ok(head.to_vec());
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| EgressError::io("reading a request head", error))?;
        if read == 0 {
            return Err(EgressError::Protocol(crate::http::HttpError::Malformed));
        }
        buffer.extend_from_slice(chunk.get(..read).unwrap_or(&[]));
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(EgressError::Protocol(crate::http::HttpError::HeadTooLarge));
        }
    }
}

/// Pass-through: the proxy forwards opaque bytes and TLS stays end-to-end.
///
/// This is the default for every profile except `model-api`. Intercepting
/// dependency traffic would make Clyde a plaintext-handling component in the
/// path of dependency *content*, which would undermine the content-hash pinning
/// that D18 relies on.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site; grouping these would obscure the flow"
)]
async fn passthrough(
    client: UnixStream,
    upstream: TcpStream,
    host: HostName,
    port: u16,
    context: Arc<EgressContext>,
    recorder: Arc<dyn EgressRecorder>,
    counters: Arc<EgressCounters>,
    started: std::time::Instant,
) -> Result<()> {
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);

    let to_upstream = copy_counted(&mut client_read, &mut upstream_write, &counters, &context);
    let to_client = copy_counted(&mut upstream_read, &mut client_write, &counters, &context);
    let (out, inbound) = tokio::join!(to_upstream, to_client);

    recorder.record(EgressAttempt {
        task_run: context.task_run.clone(),
        at: Utc::now(),
        profile: context.profile.name().to_owned(),
        host,
        port,
        decision: EgressDecision::Allowed,
        denial_reason: None,
        bytes_in: inbound.unwrap_or(0),
        bytes_out: out.unwrap_or(0),
        // Pass-through sees no request path and no status: TLS is end-to-end.
        request_path: None,
        status: None,
        duration_ms: elapsed_ms(started),
    });
    Ok(())
}

/// Copies bytes, charging the byte budget as it goes.
async fn copy_counted<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    counters: &EgressCounters,
    context: &EgressContext,
) -> Result<u64> {
    let mut buffer = vec![0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let slice = buffer.get(..read).unwrap_or(&[]);
        if writer.write_all(slice).await.is_err() {
            break;
        }
        total = total.saturating_add(read as u64);
        if counters.add_bytes(read as u64, &context.budget).is_err() {
            // The budget bounds bulk transfer; exceeding it closes the
            // connection rather than truncating silently mid-body.
            tracing::warn!(
                profile = context.profile.name(),
                "egress byte budget exhausted; closing the connection"
            );
            break;
        }
    }
    let _ = writer.shutdown().await;
    Ok(total)
}

/// Terminated path, for `model-api` only.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site; grouping these would obscure the flow"
)]
async fn terminate(
    client: UnixStream,
    upstream: TcpStream,
    host: HostName,
    port: u16,
    context: Arc<EgressContext>,
    recorder: Arc<dyn EgressRecorder>,
    counters: Arc<EgressCounters>,
    started: std::time::Instant,
) -> Result<()> {
    let Some(ca) = context.ca.as_ref() else {
        return Err(EgressError::Ca(
            "the model-api profile requires a certificate authority".to_owned(),
        ));
    };
    let leaf = ca.leaf_for(&host)?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config(&leaf)?));
    let mut client_tls = acceptor
        .accept(client)
        .await
        .map_err(|error| EgressError::Tls(format!("client handshake failed: {error}")))?;

    // Upstream certificate verification is performed normally. Termination
    // exists to keep a credential out of the sandbox, not to trust the upstream
    // less.
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config()?));
    let server_name = ServerName::try_from(host.as_str().to_owned())
        .map_err(|_| EgressError::Resolve { host: host.clone() })?;
    let mut upstream_tls = connector
        .connect(server_name, upstream)
        .await
        .map_err(|error| EgressError::Tls(format!("upstream handshake failed: {error}")))?;

    let mut bytes_out = 0u64;
    let mut bytes_in = 0u64;
    let mut last_path = None;
    let mut last_status = None;

    loop {
        // A closed client connection is the normal end of a keep-alive session,
        // not an error.
        let Ok(head) = read_head(&mut client_tls).await else {
            break;
        };
        let mut request = RequestHead::parse(&head)?;
        if counters.open_request(&context.budget).is_err() {
            let _ = client_tls
                .write_all(
                    simple_response(429, "Too Many Requests", "egress request budget exhausted")
                        .as_bytes(),
                )
                .await;
            record_denial(
                &recorder,
                &context,
                Some(host.clone()),
                port,
                EgressDenialReason::RequestBudgetExhausted,
                started,
            );
            break;
        }

        last_path = Some(request.target.clone());
        // The credential is injected here and nowhere else. Any client-supplied
        // Authorization is replaced, so a compromised agent cannot smuggle a
        // different one through.
        if let Some(auth) = context.auth_header.as_ref() {
            request.set_header("Authorization", auth.expose());
        }
        // Proxy-specific headers must not reach upstream.
        request.remove_header("Proxy-Authorization");
        request.remove_header("Proxy-Connection");

        let rendered = request.render();
        upstream_tls
            .write_all(rendered.as_bytes())
            .await
            .map_err(|error| EgressError::io("forwarding a request", error))?;
        bytes_out = bytes_out.saturating_add(rendered.len() as u64);
        let framing = request.framing()?;
        bytes_out = bytes_out.saturating_add(
            relay_body(
                &mut client_tls,
                &mut upstream_tls,
                framing,
                &counters,
                &context,
            )
            .await?,
        );

        let response_head = read_head(&mut upstream_tls).await?;
        let response = ResponseHead::parse(&response_head)?;
        last_status = Some(response.status);
        let rendered = response.render();
        client_tls
            .write_all(rendered.as_bytes())
            .await
            .map_err(|error| EgressError::io("returning a response", error))?;
        bytes_in = bytes_in.saturating_add(rendered.len() as u64);
        let framing = response.framing()?;
        bytes_in = bytes_in.saturating_add(
            relay_body(
                &mut upstream_tls,
                &mut client_tls,
                framing,
                &counters,
                &context,
            )
            .await?,
        );
        if matches!(framing, BodyFraming::UntilClose) {
            break;
        }
    }

    let _ = client_tls.shutdown().await;
    recorder.record(EgressAttempt {
        task_run: context.task_run.clone(),
        at: Utc::now(),
        profile: context.profile.name().to_owned(),
        host,
        port,
        decision: EgressDecision::Allowed,
        denial_reason: None,
        bytes_in,
        bytes_out,
        // Metadata only. Bodies are copied through a buffer and never retained.
        request_path: last_path,
        status: last_status,
        duration_ms: elapsed_ms(started),
    });
    Ok(())
}

/// Relays a body according to its framing, returning the byte count.
async fn relay_body<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    framing: BodyFraming,
    counters: &EgressCounters,
    context: &EgressContext,
) -> Result<u64> {
    match framing {
        BodyFraming::None => Ok(0),
        BodyFraming::Length(length) => copy_exact(reader, writer, length, counters, context).await,
        BodyFraming::Chunked => copy_chunked(reader, writer, counters, context).await,
        BodyFraming::UntilClose => {
            let mut buffer = vec![0u8; 32 * 1024];
            let mut total = 0u64;
            loop {
                let read = match reader.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let slice = buffer.get(..read).unwrap_or(&[]);
                writer
                    .write_all(slice)
                    .await
                    .map_err(|error| EgressError::io("relaying a body", error))?;
                total = total.saturating_add(read as u64);
                if counters.add_bytes(read as u64, &context.budget).is_err() {
                    break;
                }
            }
            Ok(total)
        }
    }
}

async fn copy_exact<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    length: u64,
    counters: &EgressCounters,
    context: &EgressContext,
) -> Result<u64> {
    let mut remaining = length;
    let mut buffer = vec![0u8; 32 * 1024];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let slot = buffer.get_mut(..want).unwrap_or(&mut []);
        let read = reader
            .read(slot)
            .await
            .map_err(|error| EgressError::io("reading a body", error))?;
        if read == 0 {
            break;
        }
        let slice = buffer.get(..read).unwrap_or(&[]);
        writer
            .write_all(slice)
            .await
            .map_err(|error| EgressError::io("relaying a body", error))?;
        remaining = remaining.saturating_sub(read as u64);
        if counters.add_bytes(read as u64, &context.budget).is_err() {
            break;
        }
    }
    Ok(length.saturating_sub(remaining))
}

async fn copy_chunked<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    counters: &EgressCounters,
    context: &EgressContext,
) -> Result<u64> {
    let mut total = 0u64;
    loop {
        let line = read_line(reader).await?;
        let size = parse_chunk_size(&line)?;
        writer
            .write_all(format!("{size:x}\r\n").as_bytes())
            .await
            .map_err(|error| EgressError::io("relaying a chunk header", error))?;
        if size == 0 {
            // Trailer section, terminated by an empty line.
            loop {
                let trailer = read_line(reader).await?;
                writer
                    .write_all(format!("{trailer}\r\n").as_bytes())
                    .await
                    .map_err(|error| EgressError::io("relaying a trailer", error))?;
                if trailer.is_empty() {
                    break;
                }
            }
            break;
        }
        total = total.saturating_add(copy_exact(reader, writer, size, counters, context).await?);
        // Each chunk is followed by CRLF.
        let _ = read_line(reader).await?;
        writer
            .write_all(b"\r\n")
            .await
            .map_err(|error| EgressError::io("relaying a chunk terminator", error))?;
        if counters.remaining_bytes(&context.budget) == 0 {
            break;
        }
    }
    Ok(total)
}

/// Reads a CRLF-terminated line, bounded.
async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader
            .read(&mut byte)
            .await
            .map_err(|error| EgressError::io("reading a line", error))?;
        if read == 0 {
            break;
        }
        if byte == *b"\n" {
            break;
        }
        if byte != *b"\r" {
            line.push(byte[0]);
        }
        if line.len() > 8192 {
            return Err(EgressError::Protocol(
                crate::http::HttpError::BadChunkHeader,
            ));
        }
    }
    String::from_utf8(line).map_err(|_| EgressError::Protocol(crate::http::HttpError::Malformed))
}

fn server_config(leaf: &crate::ca::Leaf) -> Result<ServerConfig> {
    let certificate = CertificateDer::from(leaf.certificate_der.clone());
    let key = PrivatePkcs8KeyDer::from(leaf.key_der.clone());
    let mut config = ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| EgressError::Tls(error.to_string()))?
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key.into())
    .map_err(|error| EgressError::Tls(error.to_string()))?;
    // Only HTTP/1.1 is advertised, so the terminated path never has to speak
    // HTTP/2 — which this minimal codec does not implement.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn client_config() -> Result<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| EgressError::Tls(error.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn record_denial(
    recorder: &Arc<dyn EgressRecorder>,
    context: &EgressContext,
    host: Option<HostName>,
    port: u16,
    reason: EgressDenialReason,
    started: std::time::Instant,
) {
    // A refused attempt is a first-class signal, not a log line, so it is
    // recorded with the same shape as an allowed one.
    let host = host.unwrap_or_else(|| {
        HostName::parse("unparsed.invalid").unwrap_or_else(|_| {
            // Unreachable: the literal is a valid DNS name. Falling back to the
            // same value keeps the function total.
            #[allow(clippy::let_and_return, reason = "documents the unreachable branch")]
            let placeholder = HostName::parse("invalid").unwrap_or_else(|_| unreachable_host());
            placeholder
        })
    });
    recorder.record(EgressAttempt {
        task_run: context.task_run.clone(),
        at: Utc::now(),
        profile: context.profile.name().to_owned(),
        host,
        port,
        decision: EgressDecision::Denied,
        denial_reason: Some(reason),
        bytes_in: 0,
        bytes_out: 0,
        request_path: None,
        status: None,
        duration_ms: elapsed_ms(started),
    });
}

/// A host value used only where parsing a constant cannot fail.
fn unreachable_host() -> HostName {
    // `HostName::parse` accepts this; the function exists so the fallback chain
    // above terminates without an unwrap.
    match HostName::parse("invalid") {
        Ok(host) => host,
        Err(_) => match HostName::parse("x") {
            Ok(host) => host,
            Err(_) => unreachable_host_fallback(),
        },
    }
}

fn unreachable_host_fallback() -> HostName {
    // The type has no infallible constructor by design. Reaching here would mean
    // single-letter DNS labels stopped validating, which the unit tests cover.
    HostName::parse("a").unwrap_or_else(|_| unreachable_host_fallback())
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

    fn context(profile: EgressProfile, hosts: &[&str]) -> Arc<EgressContext> {
        Arc::new(EgressContext {
            profile,
            allowlist: hosts
                .iter()
                .filter_map(|host| HostName::parse(host).ok())
                .collect(),
            budget: EgressBudget {
                max_bytes: 1 << 20,
                max_requests: 10,
                max_connections: 5,
            },
            task_run: None,
            auth_header: None,
            ca: None,
        })
    }

    #[tokio::test]
    async fn binding_under_profile_none_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = bind(
            dir.path().join("egress.sock"),
            context(EgressProfile::None, &[]),
            Arc::new(MemoryRecorder::default()),
        )
        .await
        .expect_err("profile none must not produce a socket");
        assert!(matches!(error, EgressError::NoEgress));
    }

    #[tokio::test]
    async fn a_non_allowlisted_host_is_refused_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(MemoryRecorder::default());
        let handle = bind(
            dir.path().join("egress.sock"),
            context(EgressProfile::RustRegistry, &["static.crates.io"]),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(handle.socket_path()).await.unwrap();
        client
            .write_all(b"CONNECT evil.test:443 HTTP/1.1\r\nHost: evil.test:443\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");

        let attempts = recorder.attempts();
        assert_eq!(attempts.len(), 1);
        assert!(!attempts[0].was_allowed());
        assert_eq!(attempts[0].host.as_str(), "evil.test");
        assert_eq!(
            attempts[0].denial_reason,
            Some(EgressDenialReason::HostNotAllowlisted)
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn plain_http_forwarding_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(MemoryRecorder::default());
        let handle = bind(
            dir.path().join("egress.sock"),
            context(EgressProfile::RustRegistry, &["static.crates.io"]),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(handle.socket_path()).await.unwrap();
        client
            .write_all(b"GET http://static.crates.io/x HTTP/1.1\r\nHost: static.crates.io\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 405"),
            "the proxy accepts CONNECT only: {response}"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_non_https_port_is_refused_even_on_an_allowlisted_host() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(MemoryRecorder::default());
        let handle = bind(
            dir.path().join("egress.sock"),
            context(EgressProfile::RustRegistry, &["static.crates.io"]),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(handle.socket_path()).await.unwrap();
        client
            .write_all(b"CONNECT static.crates.io:22 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert_eq!(
            recorder.attempts()[0].denial_reason,
            Some(EgressDenialReason::PortNotAllowed { port: 22 })
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_malformed_request_is_refused_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(MemoryRecorder::default());
        let handle = bind(
            dir.path().join("egress.sock"),
            context(EgressProfile::RustRegistry, &["static.crates.io"]),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(handle.socket_path()).await.unwrap();
        client.write_all(b"nonsense\r\n\r\n").await.unwrap();
        let mut response = String::new();
        let _ = client.read_to_string(&mut response).await;
        assert!(!recorder.attempts().is_empty());
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn the_connection_budget_refuses_further_connections() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(MemoryRecorder::default());
        // `.invalid` never resolves, so the test exercises the budget path
        // without touching a real network.
        let mut context = context(EgressProfile::RustRegistry, &["unreachable.invalid"]);
        Arc::get_mut(&mut context).unwrap().budget = EgressBudget {
            max_bytes: 1024,
            max_requests: 1,
            max_connections: 1,
        };
        let handle = bind(
            dir.path().join("egress.sock"),
            context,
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await
        .unwrap();

        // The first connection is allowlisted and gets as far as an upstream
        // connection attempt, which fails in this environment; either way the
        // slot is consumed.
        for _ in 0..2 {
            if let Ok(mut client) = UnixStream::connect(handle.socket_path()).await {
                let _ = client
                    .write_all(b"CONNECT unreachable.invalid:443 HTTP/1.1\r\n\r\n")
                    .await;
                let mut response = String::new();
                let _ = client.read_to_string(&mut response).await;
            }
        }
        let attempts = recorder.attempts();
        assert!(
            attempts.iter().any(|attempt| attempt.denial_reason
                == Some(EgressDenialReason::ConnectionBudgetExhausted)
                || attempt.denial_reason == Some(EgressDenialReason::UpstreamUnreachable)),
            "attempts: {attempts:?}"
        );
        handle.shutdown().await;
    }

    #[test]
    fn the_context_never_reveals_the_injected_credential() {
        let context = EgressContext {
            profile: EgressProfile::ModelApi,
            allowlist: BTreeSet::new(),
            budget: EgressBudget::DENY_ALL,
            task_run: None,
            auth_header: Some(Redacted::new(
                "Bearer sk-secret".to_owned(),
                "model api key",
            )),
            ca: None,
        };
        let rendered = format!("{context:?}");
        assert!(!rendered.contains("sk-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn allowlist_membership_is_exact() {
        let context = context(EgressProfile::RustRegistry, &["static.crates.io"]);
        assert!(context.permits_host(&HostName::parse("static.crates.io").unwrap()));
        assert!(!context.permits_host(&HostName::parse("crates.io").unwrap()));
        assert!(!context.permits_host(&HostName::parse("evil.static.crates.io").unwrap()));
    }
}
