//! The host end of the guest channel.
//!
//! Firecracker gives a VM **one** vsock device, so job control, log streaming,
//! and the egress bridge are ports on it rather than devices of their own
//! ([D7 amendment]). Firecracker exposes those ports on the host as Unix
//! sockets: a guest connecting out to port *N* arrives on `<uds_path>_N`, which
//! the host must already be listening on.
//!
//! That is what makes the security property testable as a value. The guest has
//! no network device in any configuration, and **the host binds no listener on
//! the egress port unless the profile permits egress** — so under a `none`
//! profile a guest that tries to reach the bridge finds nothing there, and the
//! absence is a fact about the host's socket table rather than a flag in a
//! configuration file.
//!
//! [D7 amendment]: ../../../docs/builder/decisions.md

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clyde_guest_api::{
    GuestMessage, HostMessage, Job, LogChunk, MAX_FRAME_BYTES, PROTOCOL_VERSION, Report, Stream,
    port,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use crate::error::{Result, SandboxError};

/// The most output one run may write to a log file.
///
/// The writer is a VM that has executed project code, so the bound is the host's
/// to enforce as it reads rather than something the guest is asked to respect
/// ([R11]). Hitting it truncates the stream and says so; it never fails the run,
/// because a build that produced too much output still produced a result.
///
/// [R11]: ../../../docs/builder/decisions.md
pub const MAX_LOG_BYTES: u64 = 256 << 20;

/// Where a VM's vsock sockets live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPaths {
    /// The path Firecracker is configured with. Host-initiated connections go
    /// here; guest-initiated ones arrive on the per-port paths below.
    pub uds: PathBuf,
}

impl ChannelPaths {
    pub fn new(directory: &Path, id: &str) -> Self {
        Self {
            uds: directory.join(format!("{id}.vsock")),
        }
    }

    /// The socket a guest connection to `port` arrives on.
    pub fn port_path(&self, port: u32) -> PathBuf {
        let mut path = self.uds.clone().into_os_string();
        path.push(format!("_{port}"));
        PathBuf::from(path)
    }
}

/// How a guest run ended, from the guest's own account of it.
#[derive(Debug)]
pub enum GuestOutcome {
    /// The guest reported. Whether the task passed or failed is inside.
    Reported(Report),
    /// The channel closed before a report arrived — a VM that died, was killed
    /// at its deadline, or never got far enough to speak.
    Silent { detail: String },
}

/// A bound, not-yet-connected guest channel.
///
/// Listeners are bound **before** the VM starts. A guest that connects to a port
/// nobody is listening on gets a refusal, and for the egress port under a `none`
/// profile that is exactly the intent.
#[derive(Debug)]
pub struct GuestChannel {
    control: UnixListener,
    log: UnixListener,
    paths: ChannelPaths,
}

impl GuestChannel {
    /// Binds the control and log ports.
    ///
    /// The egress port is deliberately not bound here: it is bound only for a
    /// profile that permits egress, by the caller that knows the profile.
    pub fn bind(paths: ChannelPaths) -> Result<Self> {
        if let Some(parent) = paths.uds.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| SandboxError::io("creating the vsock directory", source))?;
        }
        let control = bind_port(&paths, port::CONTROL)?;
        let log = bind_port(&paths, port::LOG)?;
        Ok(Self {
            control,
            log,
            paths,
        })
    }

    pub fn paths(&self) -> &ChannelPaths {
        &self.paths
    }

    /// Serves one guest run: hands over the job, streams the logs, and resolves
    /// to the guest's report.
    ///
    /// Returns immediately; the receiver resolves when the guest reports or the
    /// channel closes.
    pub fn serve(
        self,
        job: Job,
        logs: LogTargets,
    ) -> (
        oneshot::Receiver<GuestOutcome>,
        Vec<tokio::task::JoinHandle<()>>,
    ) {
        let (sender, receiver) = oneshot::channel();
        let paths = self.paths.clone();

        let control_task = tokio::spawn(async move {
            let outcome = match run_control(self.control, job).await {
                Ok(outcome) => outcome,
                Err(error) => GuestOutcome::Silent {
                    detail: error.to_string(),
                },
            };
            // A receiver that has gone away means the run was already torn down;
            // there is nobody left to tell.
            let _ = sender.send(outcome);
        });

        let log_task = tokio::spawn(async move {
            if let Err(error) = run_logs(self.log, logs).await {
                tracing::debug!(
                    vsock = ?paths.uds,
                    error = %error,
                    "the guest log stream ended early"
                );
            }
        });

        (receiver, vec![control_task, log_task])
    }

    /// Removes the socket files. Called at teardown; a leftover socket would
    /// make the next run with the same id fail to bind.
    pub fn cleanup(paths: &ChannelPaths) {
        for port in [port::CONTROL, port::LOG, port::EGRESS] {
            let _ = std::fs::remove_file(paths.port_path(port));
        }
        let _ = std::fs::remove_file(&paths.uds);
    }
}

fn bind_port(paths: &ChannelPaths, port: u32) -> Result<UnixListener> {
    let path = paths.port_path(port);
    // A stale socket from a previous run of the same id binds EADDRINUSE.
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path)
        .map_err(|source| SandboxError::io(format!("binding {path:?}"), source))
}

/// The control conversation: hello, job, report.
async fn run_control(listener: UnixListener, job: Job) -> Result<GuestOutcome> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|source| SandboxError::io("accepting the guest control connection", source))?;

    match read_frame::<GuestMessage>(&mut stream).await? {
        GuestMessage::Hello(hello) => {
            if hello.protocol_version != PROTOCOL_VERSION {
                // Fail closed rather than negotiate: an image built against a
                // different contract is a deployment error, and running it
                // anyway would run something nobody specified.
                let detail = format!(
                    "the guest implements protocol {} and this host speaks {PROTOCOL_VERSION}; the guest image and clyded are from different builds",
                    hello.protocol_version
                );
                write_frame(
                    &mut stream,
                    &HostMessage::Abort {
                        reason: detail.clone(),
                    },
                )
                .await?;
                return Ok(GuestOutcome::Silent { detail });
            }
            tracing::debug!(
                kernel = hello.kernel_release,
                "the guest opened its control channel"
            );
        }
        other => {
            return Ok(GuestOutcome::Silent {
                detail: format!("the guest opened with {other:?} rather than a hello"),
            });
        }
    }

    write_frame(&mut stream, &HostMessage::Job(Box::new(job))).await?;

    loop {
        match read_frame::<GuestMessage>(&mut stream).await {
            Ok(GuestMessage::Report(report)) => return Ok(GuestOutcome::Reported(report)),
            // `Started` is progress, not an outcome: it says the drives are
            // mounted, so a hang after it belongs to the task rather than to
            // the guest's own setup.
            Ok(GuestMessage::Started) => continue,
            Ok(GuestMessage::Hello(_)) => continue,
            Err(error) => {
                return Ok(GuestOutcome::Silent {
                    detail: error.to_string(),
                });
            }
        }
    }
}

/// Where each of the guest's streams is written on the host.
#[derive(Debug, Clone)]
pub struct LogTargets {
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
}

/// Streams task output into the host's log files.
///
/// Writing into the same files the namespace backend writes means task
/// classification, artifact capture, and the CLI all work on both backends
/// without knowing which one ran.
async fn run_logs(listener: UnixListener, targets: LogTargets) -> Result<()> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|source| SandboxError::io("accepting the guest log connection", source))?;

    let mut stdout = open_log(targets.stdout.as_deref()).await?;
    let mut stderr = open_log(targets.stderr.as_deref()).await?;
    let mut written = 0_u64;
    let mut truncated = false;

    loop {
        let chunk: LogChunk = match read_frame(&mut stream).await {
            Ok(chunk) => chunk,
            // A closed log stream is the normal end of a run.
            Err(_) => break,
        };

        written = written.saturating_add(chunk.bytes.len() as u64);
        if written > MAX_LOG_BYTES {
            if !truncated {
                truncated = true;
                if let Some(file) = stderr.as_mut() {
                    let notice = format!(
                        "\nclyde: the guest produced more than {MAX_LOG_BYTES} bytes of output; the rest is discarded\n"
                    );
                    let _ = file.write_all(notice.as_bytes()).await;
                }
            }
            continue;
        }

        let target = match chunk.stream {
            Stream::Stdout => stdout.as_mut(),
            Stream::Stderr => stderr.as_mut(),
            // The guest's own diagnostics are prefixed and go to stderr, so
            // they are visible to an operator without being mistaken for the
            // task's structured output.
            Stream::Guest => stderr.as_mut(),
        };
        let Some(file) = target else {
            continue;
        };
        if matches!(chunk.stream, Stream::Guest) {
            let _ = file.write_all(b"clyde-init: ").await;
        }
        file.write_all(&chunk.bytes)
            .await
            .map_err(|source| SandboxError::io("writing guest output", source))?;
    }

    for file in [stdout.as_mut(), stderr.as_mut()].into_iter().flatten() {
        let _ = file.flush().await;
    }
    Ok(())
}

async fn open_log(path: Option<&Path>) -> Result<Option<tokio::fs::File>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|source| SandboxError::io(format!("creating the log file {path:?}"), source))?;
    Ok(Some(file))
}

/// Reads one length-prefixed JSON frame.
async fn read_frame<T: for<'de> serde::Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
    let mut header = [0_u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|source| SandboxError::io("reading a guest frame header", source))?;
    let declared = u32::from_be_bytes(header);
    if declared > MAX_FRAME_BYTES {
        // The length is a request to allocate, and the guest is untrusted.
        return Err(SandboxError::GuestChannel {
            detail: format!("the guest declared a {declared}-byte frame, above the limit"),
        });
    }
    let mut payload = vec![0_u8; declared as usize];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|source| SandboxError::io("reading a guest frame", source))?;
    serde_json::from_slice(&payload).map_err(|source| SandboxError::GuestChannel {
        detail: format!("a guest frame could not be decoded: {source}"),
    })
}

async fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value).map_err(|source| SandboxError::GuestChannel {
        detail: format!("a host frame could not be encoded: {source}"),
    })?;
    let length = u32::try_from(payload.len()).map_err(|_| SandboxError::GuestChannel {
        detail: "a host frame exceeded the frame limit".to_owned(),
    })?;
    stream
        .write_all(&length.to_be_bytes())
        .await
        .map_err(|source| SandboxError::io("writing a host frame header", source))?;
    stream
        .write_all(&payload)
        .await
        .map_err(|source| SandboxError::io("writing a host frame", source))?;
    stream
        .flush()
        .await
        .map_err(|source| SandboxError::io("flushing a host frame", source))
}

/// Shared handle to a served run, so `wait` can collect what `start` set up.
#[derive(Debug)]
pub struct ServedRun {
    pub outcome: oneshot::Receiver<GuestOutcome>,
    pub tasks: Vec<tokio::task::JoinHandle<()>>,
    pub paths: ChannelPaths,
}

/// Runs in flight, keyed by sandbox id.
pub type ServedRuns = Arc<tokio::sync::Mutex<std::collections::HashMap<String, ServedRun>>>;

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
    fn a_guest_port_maps_to_the_socket_firecracker_creates() {
        let paths = ChannelPaths::new(Path::new("/run/clyde/vsock"), "sb-1");
        assert_eq!(
            paths.port_path(port::CONTROL),
            PathBuf::from("/run/clyde/vsock/sb-1.vsock_1024"),
            "Firecracker appends _<port> for a guest-initiated connection"
        );
        assert_ne!(paths.port_path(port::CONTROL), paths.port_path(port::LOG));
    }

    #[tokio::test]
    async fn binding_creates_the_control_and_log_sockets_and_not_the_egress_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-1");
        let channel = GuestChannel::bind(paths.clone()).unwrap();

        assert!(paths.port_path(port::CONTROL).exists());
        assert!(paths.port_path(port::LOG).exists());
        assert!(
            !paths.port_path(port::EGRESS).exists(),
            "no host listener exists on the egress port unless the profile permits egress"
        );
        drop(channel);
    }

    #[tokio::test]
    async fn cleanup_removes_every_socket_the_run_created() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-1");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        drop(channel);
        GuestChannel::cleanup(&paths);
        assert!(!paths.port_path(port::CONTROL).exists());
        assert!(!paths.port_path(port::LOG).exists());
    }
}

/// The channel driven end to end against a fake guest.
///
/// No VM and no kernel: Firecracker's contribution is to connect a guest's
/// socket to the host path this module already binds, so a client on that path
/// exercises everything except Firecracker itself — framing, the hello and job
/// exchange, log streaming, and the report.
#[cfg(test)]
mod round_trip_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use std::collections::BTreeMap;

    use clyde_guest_api::{
        DriveMount, Filesystem, Hello, LogChunk, PROTOCOL_VERSION, Report, Stream, TaskExit,
        TaskUser,
    };

    use super::*;

    fn job() -> Job {
        Job {
            version: PROTOCOL_VERSION,
            id: "sb-1".to_owned(),
            argv: vec!["/nix/store/rust/bin/cargo".to_owned(), "check".to_owned()],
            env: BTreeMap::from([("CARGO_HOME".to_owned(), "/cache/cargo-home".to_owned())]),
            cwd: PathBuf::from("/work"),
            mounts: vec![DriveMount {
                label: "clyde-work".to_owned(),
                target: PathBuf::from("/work"),
                filesystem: Filesystem::Ext4,
                writable: false,
            }],
            user: TaskUser {
                uid: 1000,
                gid: 1000,
            },
            egress: None,
            timeout_seconds: 600,
        }
    }

    /// The guest half, in the same process.
    struct FakeGuest {
        control: UnixStream,
    }

    impl FakeGuest {
        async fn connect(paths: &ChannelPaths) -> Self {
            let mut control = UnixStream::connect(paths.port_path(port::CONTROL))
                .await
                .expect("the host bound the control port before the VM started");
            write_frame(
                &mut control,
                &GuestMessage::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    kernel_release: "6.18.45".to_owned(),
                }),
            )
            .await
            .unwrap();
            Self { control }
        }

        async fn take_job(&mut self) -> Job {
            match read_frame::<HostMessage>(&mut self.control).await.unwrap() {
                HostMessage::Job(job) => *job,
                HostMessage::Abort { reason } => panic!("aborted: {reason}"),
            }
        }

        async fn log(paths: &ChannelPaths, chunks: Vec<LogChunk>) {
            let mut stream = UnixStream::connect(paths.port_path(port::LOG))
                .await
                .expect("the host bound the log port");
            for chunk in chunks {
                write_frame(&mut stream, &chunk).await.unwrap();
            }
        }

        async fn report(&mut self, report: &Report) {
            write_frame(&mut self.control, &GuestMessage::Report(report.clone()))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_guest_receives_its_job_and_its_report_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-1");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        let (outcome, _tasks) = channel.serve(
            job(),
            LogTargets {
                stdout: None,
                stderr: None,
            },
        );

        let mut guest = FakeGuest::connect(&paths).await;
        let received = guest.take_job().await;
        assert_eq!(received.argv, job().argv);
        assert_eq!(received.mounts[0].label, "clyde-work");

        guest
            .report(&Report::Exited {
                exit: TaskExit {
                    code: Some(0),
                    signal: None,
                },
            })
            .await;

        match outcome.await.unwrap() {
            GuestOutcome::Reported(Report::Exited { exit }) => assert_eq!(exit.code, Some(0)),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_guest_that_could_not_run_the_task_is_distinguishable_from_one_that_did() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-2");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        let (outcome, _tasks) = channel.serve(
            job(),
            LogTargets {
                stdout: None,
                stderr: None,
            },
        );

        let mut guest = FakeGuest::connect(&paths).await;
        let _ = guest.take_job().await;
        guest
            .report(&Report::Failed {
                stage: clyde_guest_api::FailureStage::Mount,
                detail: "no device carried label clyde-work".to_owned(),
            })
            .await;

        // This is the distinction the whole contract exists for: a mount failure
        // must never reach the task layer as a project compile error.
        match outcome.await.unwrap() {
            GuestOutcome::Reported(Report::Failed { stage, .. }) => {
                assert_eq!(stage, clyde_guest_api::FailureStage::Mount);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_guest_that_dies_without_reporting_is_silent_rather_than_successful() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-3");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        let (outcome, _tasks) = channel.serve(
            job(),
            LogTargets {
                stdout: None,
                stderr: None,
            },
        );

        let mut guest = FakeGuest::connect(&paths).await;
        let _ = guest.take_job().await;
        drop(guest);

        assert!(
            matches!(outcome.await.unwrap(), GuestOutcome::Silent { .. }),
            "a dead guest must not be readable as a passing task"
        );
    }

    #[tokio::test]
    async fn task_output_reaches_the_host_log_files_on_the_right_streams() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-4");
        let stdout = dir.path().join("stdout");
        let stderr = dir.path().join("stderr");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        let (outcome, tasks) = channel.serve(
            job(),
            LogTargets {
                stdout: Some(stdout.clone()),
                stderr: Some(stderr.clone()),
            },
        );

        let mut guest = FakeGuest::connect(&paths).await;
        let _ = guest.take_job().await;
        FakeGuest::log(
            &paths,
            vec![
                LogChunk {
                    stream: Stream::Stdout,
                    bytes: b"{\"reason\":\"compiler-message\"}\n".to_vec(),
                },
                LogChunk {
                    stream: Stream::Stderr,
                    bytes: b"error[E0425]: cannot find value\n".to_vec(),
                },
                LogChunk {
                    stream: Stream::Guest,
                    bytes: b"2 output chunks were dropped\n".to_vec(),
                },
            ],
        )
        .await;
        guest
            .report(&Report::Exited {
                exit: TaskExit {
                    code: Some(101),
                    signal: None,
                },
            })
            .await;
        let _ = outcome.await;
        for task in tasks {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
        }

        let out = std::fs::read_to_string(&stdout).unwrap();
        let err = std::fs::read_to_string(&stderr).unwrap();
        assert!(
            out.contains("compiler-message"),
            "cargo's JSON diagnostics must arrive unmangled: {out:?}"
        );
        assert!(err.contains("E0425"));
        assert!(
            err.contains("clyde-init: 2 output chunks were dropped"),
            "the guest's own diagnostics are prefixed so they cannot be mistaken for the task's: {err:?}"
        );
        assert!(
            !out.contains("clyde-init:"),
            "guest diagnostics must not contaminate the structured stdout stream"
        );
    }

    #[tokio::test]
    async fn a_guest_speaking_a_different_protocol_version_is_aborted_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ChannelPaths::new(dir.path(), "sb-5");
        let channel = GuestChannel::bind(paths.clone()).unwrap();
        let (outcome, _tasks) = channel.serve(
            job(),
            LogTargets {
                stdout: None,
                stderr: None,
            },
        );

        let mut control = UnixStream::connect(paths.port_path(port::CONTROL))
            .await
            .unwrap();
        write_frame(
            &mut control,
            &GuestMessage::Hello(Hello {
                protocol_version: PROTOCOL_VERSION + 1,
                kernel_release: "6.18.45".to_owned(),
            }),
        )
        .await
        .unwrap();

        // The guest image and clyded came from different builds: fail closed
        // rather than hand a job to something that may read it differently.
        match read_frame::<HostMessage>(&mut control).await.unwrap() {
            HostMessage::Abort { reason } => assert!(reason.contains("protocol")),
            HostMessage::Job(_) => panic!("a mismatched guest must not receive a job"),
        }
        assert!(matches!(
            outcome.await.unwrap(),
            GuestOutcome::Silent { .. }
        ));
    }
}
