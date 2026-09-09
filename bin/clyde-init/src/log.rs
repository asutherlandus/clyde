//! The log sink: task output on its way to the host.
//!
//! A sink that has lost its connection keeps accepting chunks and drops them.
//! That is deliberate. Killing a running build because the host stopped reading
//! would turn a host-side problem into a failed task, and the host already
//! treats missing output as its own to notice.

use std::io::Write;

use clyde_guest_api::{LogChunk, Stream, write_frame};

use crate::vsock::VsockStream;

/// Where task output goes.
#[derive(Debug)]
pub struct LogSink {
    stream: Option<VsockStream>,
    /// Chunks dropped after the channel failed, reported in the guest's own
    /// diagnostics so a truncated log is visible as truncation.
    dropped: u64,
}

impl LogSink {
    pub fn new(stream: VsockStream) -> Self {
        Self {
            stream: Some(stream),
            dropped: 0,
        }
    }

    /// A sink with nowhere to write. Used when the log channel could not be
    /// established at all, so the task still runs and the host still gets a
    /// report.
    pub fn detached() -> Self {
        Self {
            stream: None,
            dropped: 0,
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Sends one chunk, dropping it if the channel has failed.
    pub fn send(&mut self, chunk: &LogChunk) {
        let Some(stream) = self.stream.as_mut() else {
            self.dropped += 1;
            return;
        };
        if write_frame(stream, chunk).is_err() {
            // One failure closes the channel: a stream that has failed once
            // will fail for every subsequent chunk, and retrying per chunk
            // would spend a build's worth of time on a dead socket.
            self.stream = None;
            self.dropped += 1;
        }
    }

    /// Sends a line of the guest's own diagnostics.
    pub fn guest(&mut self, message: &str) {
        self.send(&LogChunk {
            stream: Stream::Guest,
            bytes: format!("{message}\n").into_bytes(),
        });
    }

    /// Closes the channel, so the host's reader sees the end of the stream
    /// rather than waiting for one.
    pub fn close(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.flush();
        }
        self.stream = None;
    }
}

/// Writes a line to the guest console.
///
/// The console is a diagnostic of last resort — for failures that happen before
/// the log channel exists, or that break it. It is never the log transport
/// (R11): the 8250 UART is slow enough that build output through it stalls the
/// guest.
pub fn console(message: &str) {
    if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(console, "clyde-init: {message}");
    }
}
