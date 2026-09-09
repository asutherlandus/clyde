//! The guest end of the vsock multiplex.
//!
//! A thin wrapper over the `vsock` crate's stream, kept as a type of its own so
//! the rest of the guest talks about "the channel to the host" rather than about
//! CIDs and ports, and so the connect failure carries which port it was.
//!
//! Blocking, single-threaded I/O is deliberate: the guest has one job to run,
//! and an async runtime inside the image would be machinery in service of
//! nothing.

use std::io::{Read, Write};

use vsock::VsockStream as RawStream;

/// A connected vsock stream.
#[derive(Debug)]
pub struct VsockStream {
    stream: RawStream,
}

impl VsockStream {
    /// Connects to a host port.
    ///
    /// The host binds its listener before the VM starts, so a refusal means the
    /// host is not offering that port. For [`clyde_guest_api::port::EGRESS`]
    /// under a profile that permits no egress, that refusal is the design
    /// working (D7 amendment), not a fault.
    pub fn connect(cid: u32, port: u32) -> std::io::Result<Self> {
        RawStream::connect_with_cid_port(cid, port).map(|stream| Self { stream })
    }
}

impl Read for VsockStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl Write for VsockStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}
