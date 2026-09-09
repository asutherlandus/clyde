//! Running the task and streaming what it produces.
//!
//! Two properties live here. The task process **drops out of guest root** before
//! it runs: guest root is defensible because the VM is the boundary, but a guest
//! kernel exploit then needs two steps rather than one (D24). And its output
//! **leaves over vsock while it runs** (R11), because the serial console cannot
//! carry cargo's JSON diagnostics without stalling the guest, and because a
//! guest killed mid-run should still have delivered what it emitted.

use std::io::Read;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clyde_guest_api::{Job, LogChunk, Stream, TaskExit};

use crate::error::{GuestError, Result};
use crate::log::LogSink;

/// How often the guest checks a running task against its own deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Builds the task command from the job.
///
/// Separated from execution so the command a job produces is a value a test can
/// assert on, the same way the namespace backend's argv is.
pub fn command_for(job: &Job) -> Result<Command> {
    let (program, arguments) = job.argv.split_first().ok_or(GuestError::EmptyArgv)?;

    let mut command = Command::new(program);
    command.args(arguments);
    // The environment is exactly what the job named. Nothing is inherited: the
    // init's own environment is the kernel's, and none of it belongs to a task.
    command.env_clear();
    command.envs(&job.env);
    command.current_dir(&job.cwd);
    command.uid(job.user.uid);
    command.gid(job.user.gid);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    Ok(command)
}

/// Runs the task, streaming its output, and returns how it finished.
///
/// A non-zero exit is a successful run of this function: the task's failure is
/// the project's business, and only a failure to *run* it is the guest's.
pub fn run(job: &Job, sink: Arc<Mutex<LogSink>>) -> Result<TaskExit> {
    let mut command = command_for(job)?;
    let mut child = command.spawn().map_err(|source| GuestError::Exec {
        program: job.argv.first().cloned().unwrap_or_default(),
        source,
    })?;

    let pumps = [
        child
            .stdout
            .take()
            .map(|stream| pump(Box::new(stream), Stream::Stdout, Arc::clone(&sink))),
        child
            .stderr
            .take()
            .map(|stream| pump(Box::new(stream), Stream::Stderr, Arc::clone(&sink))),
    ];

    let deadline = Instant::now() + Duration::from_secs(job.timeout_seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(source) => {
                return Err(GuestError::io("waiting for the task", source));
            }
        }
        if Instant::now() >= deadline {
            // The host deadline is the real bound; this one exists so a guest
            // whose host has stopped listening still stops.
            let _ = child.kill();
            let _ = child.wait();
            join_all(pumps);
            return Err(GuestError::Timeout {
                seconds: job.timeout_seconds,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    // Drain before reporting: a report that overtakes the last of the output
    // would truncate exactly the diagnostics the failure needs.
    join_all(pumps);

    Ok(TaskExit {
        code: status.code(),
        signal: status.signal(),
    })
}

/// Copies one output stream into the log sink until it closes.
fn pump(
    mut source: Box<dyn Read + Send>,
    stream: Stream,
    sink: Arc<Mutex<LogSink>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            match source.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    let Some(bytes) = buffer.get(..read) else {
                        return;
                    };
                    let chunk = LogChunk {
                        stream,
                        bytes: bytes.to_vec(),
                    };
                    // A host that has stopped reading is not a reason to kill a
                    // running build: the run continues and the loss is the
                    // host's to notice.
                    if let Ok(mut sink) = sink.lock() {
                        sink.send(&chunk);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
    })
}

fn join_all(pumps: [Option<std::thread::JoinHandle<()>>; 2]) {
    for pump in pumps.into_iter().flatten() {
        let _ = pump.join();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use clyde_guest_api::{PROTOCOL_VERSION, TaskUser};

    use super::*;

    fn job(argv: &[&str]) -> Job {
        Job {
            version: PROTOCOL_VERSION,
            id: "sb-1".to_owned(),
            argv: argv.iter().map(|part| (*part).to_owned()).collect(),
            env: BTreeMap::from([("CARGO_HOME".to_owned(), "/cache/cargo-home".to_owned())]),
            cwd: PathBuf::from("/work"),
            mounts: Vec::new(),
            user: TaskUser {
                uid: 1000,
                gid: 1000,
            },
            egress: None,
            timeout_seconds: 60,
        }
    }

    #[test]
    fn the_command_carries_only_the_environment_the_job_named() {
        let command = command_for(&job(&["/bin/true"])).unwrap();
        let inherited: Vec<_> = command.get_envs().collect();
        assert_eq!(
            inherited.len(),
            1,
            "env_clear then the job's own map, nothing else: {inherited:?}"
        );
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/work"))
        );
    }

    #[test]
    fn a_job_with_no_argv_is_refused_rather_than_defaulted() {
        let error = command_for(&job(&[])).expect_err("nothing to run");
        assert!(matches!(error, GuestError::EmptyArgv));
    }

    #[test]
    fn the_program_and_its_arguments_are_split_at_the_first_element() {
        let command = command_for(&job(&["/bin/cargo", "check", "--offline"])).unwrap();
        assert_eq!(command.get_program(), "/bin/cargo");
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, vec!["check", "--offline"]);
    }
}
