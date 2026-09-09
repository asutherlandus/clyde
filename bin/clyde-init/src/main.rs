//! `clyde-init`: the microVM guest init (Part 1b, [D24]).
//!
//! PID 1 inside a Clyde build guest. It mounts its drives, learns what to run
//! over the vsock control channel, runs it as an unprivileged uid, streams the
//! output back, reports what happened, and shuts the VM down.
//!
//! Three things it deliberately does not do:
//!
//! - **It does not learn its job from the kernel command line.** A VM booted
//!   before its job exists cannot carry that job in its boot arguments, and that
//!   is precisely the option a future warm pool needs left open ([D24]).
//! - **It does not write task output to the console.** The 8250 UART is the only
//!   console Firecracker has and it is slow enough to stall the guest; output
//!   leaves over vsock ([R11]).
//! - **It does not exit.** PID 1 exiting is a kernel panic. Every path here ends
//!   in a reset, which is how the VM stops and how Firecracker learns the run is
//!   over.
//!
//! [D24]: ../../../docs/builder/decisions.md
//! [R11]: ../../../docs/builder/decisions.md

mod drives;
mod error;
mod log;
mod task;
mod vsock;

use std::sync::{Arc, Mutex};

use clyde_guest_api::{
    GuestMessage, HELLO_UNKNOWN_RELEASE, Hello, HostMessage, Job, PROTOCOL_VERSION, Report,
    TaskExit, port, read_frame, write_frame,
};

use crate::error::{GuestError, Result};
use crate::log::{LogSink, console};
use crate::vsock::VsockStream;

// Every path ends in a reset, so this never returns: PID 1 returning is a kernel
// panic, and the reset is how Firecracker learns the run is over.
fn main() -> ! {
    if let Err(error) = run() {
        console(&format!("{error}"));
    }
    shutdown()
}

/// The whole guest sequence.
///
/// The control connection is opened before anything can fail interestingly, so
/// that a failure has somewhere to be reported to. Only the pseudo-filesystem
/// mounts happen first, because the connection needs `/dev` and `/proc` to
/// exist.
fn run() -> Result<()> {
    drives::mount_pseudo()?;

    let mut control = VsockStream::connect(clyde_guest_api::HOST_CID, port::CONTROL)
        .map_err(|source| GuestError::io("connecting the control channel", source))?;

    write_frame(
        &mut control,
        &GuestMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            kernel_release: kernel_release(),
        }),
    )?;

    let job = match read_frame::<_, HostMessage>(&mut control)? {
        HostMessage::Job(job) => *job,
        HostMessage::Abort { reason } => return Err(GuestError::Aborted { reason }),
    };

    if job.version != PROTOCOL_VERSION {
        // Fail closed: a job whose shape this guest does not know is a job it
        // cannot honour, and running the parts it recognises would be running
        // something nobody specified.
        let error = GuestError::ProtocolVersion {
            sent: job.version,
            ours: PROTOCOL_VERSION,
        };
        report(&mut control, &failure_report(&error));
        return Err(error);
    }

    match execute(&job) {
        Ok(exit) => {
            let teardown = drives::unmount_writable(&job.mounts);
            // A task that ran is reported as having run even if teardown then
            // failed; the teardown failure is the guest's, and losing the task's
            // exit status to it would be the wrong trade.
            report(&mut control, &Report::Exited { exit });
            teardown
        }
        Err(error) => {
            let _ = drives::unmount_writable(&job.mounts);
            report(&mut control, &failure_report(&error));
            Err(error)
        }
    }
}

/// Mounts the job's drives, runs the task, and drains its output.
fn execute(job: &Job) -> Result<TaskExit> {
    drives::mount_all(&job.mounts, job.user)?;

    // A log channel that cannot be established is not fatal: the task still
    // runs and the host still gets a report. It is reported on the console,
    // which is where a host with no log stream will be looking.
    let sink = match VsockStream::connect(clyde_guest_api::HOST_CID, port::LOG) {
        Ok(stream) => LogSink::new(stream),
        Err(error) => {
            console(&format!(
                "no log channel ({error}); the task runs unwatched"
            ));
            LogSink::detached()
        }
    };
    let sink = Arc::new(Mutex::new(sink));

    let outcome = task::run(job, Arc::clone(&sink));

    if let Ok(mut sink) = sink.lock() {
        let dropped = sink.dropped();
        if dropped > 0 {
            // Truncation has to be visible as truncation. A short log that
            // looks complete is worse than one that says it is short.
            sink.guest(&format!("{dropped} output chunks were dropped"));
            console(&format!("{dropped} output chunks were dropped"));
        }
        sink.close();
    }

    outcome
}

/// Sends the final report, best effort.
///
/// A host that has already gone away cannot be told anything, and the VM is
/// about to stop regardless.
fn report(control: &mut VsockStream, report: &Report) {
    if write_frame(control, &GuestMessage::Report(report.clone())).is_err() {
        console("the control channel closed before the report was sent");
    }
}

fn failure_report(error: &GuestError) -> Report {
    Report::Failed {
        stage: error.stage(),
        detail: error.to_string(),
    }
}

/// The running kernel's release string, for the host's audit record.
fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_else(|_| HELLO_UNKNOWN_RELEASE.to_owned())
}

/// Stops the VM.
///
/// A reset rather than a power-off: Firecracker's i8042 is a reset controller,
/// the boot arguments ask for `reboot=k`, and a guest that halts instead leaves
/// the VM running with nothing in it.
fn shutdown() -> ! {
    rustix::fs::sync();
    let _ = rustix::system::reboot(rustix::system::RebootCommand::Restart);
    // Unreachable on a working kernel. If the reset did not take, spinning is
    // better than returning: PID 1 returning is a panic, and the host deadline
    // will stop the VM.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
