//! `clyde-guest-api`: the microVM job contract.
//!
//! A **versioned type shared between host and guest** rather than an ad hoc
//! encoding ([D24](../../../docs/builder/decisions.md)), carried over the vsock
//! multiplex ([D7 amendment]). The guest learns what to run from this contract
//! and reports back through it; nothing about a job travels on the kernel
//! command line, because a VM booted before its job exists cannot carry that job
//! in its boot arguments, and that is the option a future warm pool needs left
//! open.
//!
//! Three properties this crate exists to hold:
//!
//! - **Drives are addressed by filesystem label, never by device name.** Guest
//!   device names are positional and `drive_id` is host-side metadata the guest
//!   cannot read, so a positional contract breaks silently the first time the
//!   mount table changes shape.
//! - **A task exit is a different value from a guest failure.** [`Report`]
//!   distinguishes them, which is what keeps `ProjectCodeError` and
//!   `SandboxFailure` apart when the only other signal is a VM exit code.
//! - **Everything the guest emits is bounded.** Frames carry a length the reader
//!   checks against [`MAX_FRAME_BYTES`] before allocating, because the writer is
//!   a VM that has executed project code ([R11]).
//!
//! [D7 amendment]: ../../../docs/builder/decisions.md
//! [R11]: ../../../docs/builder/decisions.md

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The contract version. A guest that does not recognise the host's version
/// refuses the job rather than guessing at its shape.
pub const PROTOCOL_VERSION: u32 = 1;

/// Firecracker's fixed host CID.
pub const HOST_CID: u32 = 2;

/// The CID Clyde assigns every guest. A VM has exactly one peer.
pub const GUEST_CID: u32 = 3;

/// vsock ports on the single guest channel ([D7 amendment]).
///
/// The device exists in every configuration because logs have nowhere else to
/// go; what varies by egress profile is whether the host binds a listener on
/// [`port::EGRESS`].
///
/// [D7 amendment]: ../../../docs/builder/decisions.md
pub mod port {
    /// Job control: the guest's hello, the job itself, and the final report.
    pub const CONTROL: u32 = 1024;
    /// Task output, streamed while the task runs.
    pub const LOG: u32 = 1025;
    /// The egress bridge. A host listener exists here only when the task's
    /// egress profile permits egress.
    pub const EGRESS: u32 = 1080;
}

/// What the guest reports as its kernel release when `/proc` cannot be read.
///
/// A string rather than an `Option`, because the host records it either way and
/// "unknown" is the honest value.
pub const HELLO_UNKNOWN_RELEASE: &str = "unknown";

/// The largest frame either side will read.
///
/// Log volume is chunked well below this; the bound exists because the guest is
/// untrusted and a declared length is an allocation request.
pub const MAX_FRAME_BYTES: u32 = 4 << 20;

/// Which filesystem a drive carries.
///
/// Closed rather than a string: a guest that mounts an image with a filesystem
/// the host did not name is a guest doing something the contract did not ask
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filesystem {
    /// The read-only runtime root ([R12]).
    ///
    /// [R12]: ../../../docs/builder/decisions.md
    Erofs,
    /// Source snapshots, the mission cache, and dependency bundles.
    Ext4,
}

impl Filesystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Erofs => "erofs",
            Self::Ext4 => "ext4",
        }
    }
}

/// A drive the guest must mount, addressed by filesystem label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveMount {
    /// The filesystem label set at `mkfs` time. The guest matches this against
    /// the labels it reads from the block devices it can see.
    pub label: String,
    /// Where it belongs in the guest.
    pub target: PathBuf,
    pub filesystem: Filesystem,
    /// Whether the guest mounts it read-write. A drive the host attached
    /// read-only cannot be made writable here — this only decides whether the
    /// guest asks.
    pub writable: bool,
}

/// The uid and gid the task runs as.
///
/// The task process drops out of guest root before exec: guest root is
/// defensible because the VM is the boundary, but a guest kernel exploit then
/// needs two steps rather than one (D24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUser {
    pub uid: u32,
    pub gid: u32,
}

/// The egress bridge, present only for a profile that permits egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressBridge {
    /// The loopback port the in-guest forwarder listens on.
    pub listen_port: u16,
    /// The host vsock port it bridges to. Always [`port::EGRESS`]; carried
    /// explicitly so the guest never has to assume a constant the host chose.
    pub host_port: u32,
}

/// What the guest must run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub version: u32,
    /// The sandbox identifier, for correlating guest diagnostics with the task
    /// run that produced them.
    pub id: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Drives to mount before the task starts, in the order given.
    pub mounts: Vec<DriveMount>,
    pub user: TaskUser,
    /// Absent for every profile that does not permit egress, which is every
    /// build task except dependency resolution.
    pub egress: Option<EgressBridge>,
    /// The guest's own bound on the task, a backstop for the host deadline
    /// rather than a replacement for it.
    pub timeout_seconds: u64,
}

/// What the guest says first, before it has a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    /// The guest kernel release, so a mismatch between the image the host
    /// thinks it attached and the one that booted is visible in the audit
    /// record rather than inferred from behaviour.
    pub kernel_release: String,
}

/// Where in the guest's own sequence something failed.
///
/// Every variant means the task did not run, or did not run as specified, which
/// is a `SandboxFailure` on the host side rather than a project error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    /// Mounting `/proc`, `/sys`, `/dev`, or the scratch filesystems.
    Pseudo,
    /// A job whose protocol version this guest does not implement.
    Protocol,
    /// No block device carried a label the job named.
    DriveDiscovery,
    /// The device was found and the mount was refused.
    Mount,
    /// Dropping to the task uid.
    Privilege,
    /// The task binary could not be executed at all.
    Exec,
    /// The guest's own timeout fired.
    Timeout,
    /// Anything after the task finished: sync, unmount, or reporting.
    Teardown,
}

impl FailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pseudo => "pseudo-filesystems",
            Self::Protocol => "protocol",
            Self::DriveDiscovery => "drive discovery",
            Self::Mount => "mount",
            Self::Privilege => "privilege drop",
            Self::Exec => "exec",
            Self::Timeout => "guest timeout",
            Self::Teardown => "teardown",
        }
    }
}

/// How the task itself finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// The guest's final word on a job.
///
/// The distinction between the two variants is the whole point of the type: a
/// task that ran and failed is the project's business, and a guest that could
/// not run the task is Clyde's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Report {
    /// The task ran to completion, whatever its exit status.
    Exited { exit: TaskExit },
    /// The guest could not run the task as specified.
    Failed { stage: FailureStage, detail: String },
}

/// Which of the task's output streams a log chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
    /// The guest's own diagnostics, kept separate from the task's output so a
    /// chatty init cannot corrupt a cargo JSON stream.
    Guest,
}

/// One chunk of task output.
///
/// Bytes are base64 in JSON rather than a string, because a build's output is
/// not guaranteed to be UTF-8 and a lossy conversion in the guest would corrupt
/// diagnostics the host is about to classify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    pub stream: Stream,
    #[serde(with = "base64_bytes")]
    pub bytes: Vec<u8>,
}

/// Messages the guest sends on [`port::CONTROL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum GuestMessage {
    Hello(Hello),
    /// Sent once the drives are mounted and the task is about to be executed,
    /// so a hang is attributable to the task rather than to the guest's setup.
    Started,
    Report(Report),
}

/// Messages the host sends on [`port::CONTROL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum HostMessage {
    Job(Box<Job>),
    /// Tear down without running anything. Sent when the host has already given
    /// up on this VM.
    Abort {
        reason: String,
    },
}

/// Framing and transport errors.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("the channel closed before a complete frame arrived")]
    Closed,
    #[error("a frame declared {declared} bytes, above the {MAX_FRAME_BYTES}-byte limit")]
    FrameTooLarge { declared: u32 },
    #[error("a frame could not be decoded: {source}")]
    Decode {
        #[source]
        source: serde_json::Error,
    },
    #[error("a frame could not be encoded: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl ProtocolError {
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Encodes one frame: a big-endian length followed by JSON.
///
/// Big-endian because this is a wire format and byte order should not depend on
/// the architecture either side happens to be built for.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(value).map_err(|source| ProtocolError::Encode { source })?;
    let length = u32::try_from(payload.len())
        .map_err(|_| ProtocolError::FrameTooLarge { declared: u32::MAX })?;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge { declared: length });
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Writes one frame to a blocking writer. The guest half of the protocol.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let frame = encode_frame(value)?;
    writer
        .write_all(&frame)
        .map_err(|source| ProtocolError::io("writing a frame", source))?;
    writer
        .flush()
        .map_err(|source| ProtocolError::io("flushing a frame", source))
}

/// Reads one frame from a blocking reader. The guest half of the protocol.
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut R) -> Result<T> {
    let mut header = [0_u8; 4];
    read_exact(reader, &mut header)?;
    let declared = u32::from_be_bytes(header);
    if declared > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge { declared });
    }
    let mut payload = vec![0_u8; declared as usize];
    read_exact(reader, &mut payload)?;
    serde_json::from_slice(&payload).map_err(|source| ProtocolError::Decode { source })
}

/// `read_exact` that reports a clean close as [`ProtocolError::Closed`] rather
/// than as an I/O error, because a guest that powered off mid-frame is an
/// expected outcome and not a fault to log as one.
fn read_exact<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        let Some(slice) = buffer.get_mut(filled..) else {
            return Err(ProtocolError::Closed);
        };
        match reader.read(slice) {
            Ok(0) => return Err(ProtocolError::Closed),
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(ProtocolError::io("reading a frame", source)),
        }
    }
    Ok(())
}

/// Base64 for log payloads, implemented here rather than pulled in as a
/// dependency: the guest image contains everything this crate compiles to, and
/// a codec this small is cheaper to read than to audit as a supply-chain entry.
mod base64_bytes {
    use serde::{Deserialize as _, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        decode(&text).ok_or_else(|| serde::de::Error::custom("malformed base64 in a log chunk"))
    }

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let (b0, b1, b2) = (
                *chunk.first().unwrap_or(&0),
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            );
            let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            for index in 0..4 {
                let take = match chunk.len() {
                    1 => index < 2,
                    2 => index < 3,
                    _ => true,
                };
                if take {
                    let shift = 18 - index * 6;
                    let position = ((triple >> shift) & 0x3F) as usize;
                    out.push(char::from(*ALPHABET.get(position).unwrap_or(&b'A')));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let mut accumulator: u32 = 0;
        let mut bits = 0_u32;
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        for byte in text.bytes() {
            if byte == b'=' {
                break;
            }
            let value = ALPHABET.iter().position(|candidate| *candidate == byte)?;
            accumulator = (accumulator << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                let shifted = u8::try_from((accumulator >> bits) & 0xFF).ok()?;
                out.push(shifted);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    #[test]
    fn a_job_round_trips_through_a_frame() {
        let frame = encode_frame(&HostMessage::Job(Box::new(job()))).unwrap();
        let mut cursor = std::io::Cursor::new(frame);
        let decoded: HostMessage = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded, HostMessage::Job(Box::new(job())));
    }

    #[test]
    fn a_task_exit_and_a_guest_failure_are_different_values() {
        let exited = Report::Exited {
            exit: TaskExit {
                code: Some(101),
                signal: None,
            },
        };
        let failed = Report::Failed {
            stage: FailureStage::Mount,
            detail: "no device carried label clyde-work".to_owned(),
        };
        assert_ne!(exited, failed);
        // The wire form must keep them apart too: this is what stops a project
        // compile error from being recorded as a sandbox failure.
        let exited_json = serde_json::to_string(&exited).unwrap();
        let failed_json = serde_json::to_string(&failed).unwrap();
        assert!(exited_json.contains("\"outcome\":\"exited\""));
        assert!(failed_json.contains("\"outcome\":\"failed\""));
    }

    #[test]
    fn a_frame_larger_than_the_limit_is_refused_before_allocating() {
        let mut oversized = Vec::new();
        oversized.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
        let mut cursor = std::io::Cursor::new(oversized);
        let error = read_frame::<_, HostMessage>(&mut cursor).expect_err("refused");
        assert!(matches!(error, ProtocolError::FrameTooLarge { .. }));
    }

    #[test]
    fn a_truncated_frame_reads_as_a_close_not_a_fault() {
        let mut truncated = encode_frame(&GuestMessage::Started).unwrap();
        truncated.pop();
        let mut cursor = std::io::Cursor::new(truncated);
        let error = read_frame::<_, GuestMessage>(&mut cursor).expect_err("incomplete");
        assert!(matches!(error, ProtocolError::Closed));
    }

    #[test]
    fn log_bytes_survive_content_that_is_not_utf8() {
        let chunk = LogChunk {
            stream: Stream::Stdout,
            bytes: vec![0xFF, 0x00, 0x41, 0xC3, 0x28],
        };
        let frame = encode_frame(&chunk).unwrap();
        let mut cursor = std::io::Cursor::new(frame);
        let decoded: LogChunk = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded, chunk);
    }

    #[test]
    fn base64_round_trips_every_chunk_length() {
        for length in 0..64_usize {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 7 % 251) as u8).collect();
            let encoded = base64_bytes::encode(&bytes);
            assert_eq!(
                base64_bytes::decode(&encoded).unwrap(),
                bytes,
                "length {length}"
            );
        }
    }
}

/// Reading an ext filesystem label out of a superblock.
///
/// Shared by both ends on purpose. The guest finds its drives by label because
/// device names are positional (D24); the host writes those labels at `mkfs`
/// time. One implementation means the host can verify that an image it built
/// carries the label the job will ask for, rather than the two sides agreeing
/// by convention and diverging silently.
pub mod ext4 {
    /// Where the superblock starts within the device.
    pub const SUPERBLOCK_OFFSET: u64 = 1024;
    /// How much of it needs reading.
    pub const SUPERBLOCK_BYTES: usize = 256;
    /// `s_magic`, relative to the superblock.
    const MAGIC_OFFSET: usize = 0x38;
    /// `s_volume_name`, relative to the superblock.
    const LABEL_OFFSET: usize = 0x78;
    /// ext2/3/4 volume labels are 16 bytes. A longer label is truncated at
    /// `mkfs` time, and the guest would then never find the drive.
    pub const LABEL_LENGTH: usize = 16;
    const EXT_MAGIC: u16 = 0xEF53;

    /// The label in a superblock, or `None` if this is not an ext filesystem.
    ///
    /// Not an error: the runtime root is erofs and has no ext superblock, and a
    /// device the job did not ask about is not this function's business.
    pub fn label_from_superblock(superblock: &[u8]) -> Option<String> {
        let magic = u16::from_le_bytes([
            *superblock.get(MAGIC_OFFSET)?,
            *superblock.get(MAGIC_OFFSET + 1)?,
        ]);
        if magic != EXT_MAGIC {
            return None;
        }
        let raw = superblock.get(LABEL_OFFSET..LABEL_OFFSET + LABEL_LENGTH)?;
        let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
        let label = std::str::from_utf8(raw.get(..end)?).ok()?;
        (!label.is_empty()).then(|| label.to_owned())
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
        use super::*;

        fn superblock(label: &str) -> Vec<u8> {
            let mut bytes = vec![0_u8; SUPERBLOCK_BYTES];
            bytes[MAGIC_OFFSET..MAGIC_OFFSET + 2].copy_from_slice(&EXT_MAGIC.to_le_bytes());
            let raw = label.as_bytes();
            bytes[LABEL_OFFSET..LABEL_OFFSET + raw.len()].copy_from_slice(raw);
            bytes
        }

        #[test]
        fn a_label_round_trips() {
            assert_eq!(
                label_from_superblock(&superblock("clyde-work")).as_deref(),
                Some("clyde-work")
            );
        }

        #[test]
        fn a_non_ext_superblock_has_no_label() {
            assert_eq!(label_from_superblock(&vec![0_u8; SUPERBLOCK_BYTES]), None);
        }

        #[test]
        fn a_short_read_is_not_a_panic() {
            assert_eq!(label_from_superblock(&[0_u8; 4]), None);
        }
    }
}
