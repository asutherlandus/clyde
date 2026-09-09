//! Guest-side errors.
//!
//! Every variant maps to a [`FailureStage`], because the host's whole reason for
//! caring which error occurred is to tell a task that failed from a guest that
//! could not run one.

use std::path::PathBuf;

use clyde_guest_api::{FailureStage, Filesystem};

#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("mounting {filesystem} at {target} is not possible: {source}")]
    PseudoMount {
        target: PathBuf,
        filesystem: &'static str,
        #[source]
        source: rustix::io::Errno,
    },

    #[error("mounting {device} ({filesystem}) at {target} failed: {source}", filesystem = filesystem.as_str())]
    Mount {
        target: PathBuf,
        device: PathBuf,
        filesystem: Filesystem,
        #[source]
        source: rustix::io::Errno,
    },

    #[error("unmounting {target} failed: {source}")]
    Unmount {
        target: PathBuf,
        #[source]
        source: rustix::io::Errno,
    },

    #[error(
        "no block device carries the label {label}; visible labels: {}",
        render(available)
    )]
    NoSuchLabel {
        label: String,
        available: Vec<String>,
    },

    #[error("two devices claim the label {label}: {first} and {second}")]
    DuplicateLabel {
        label: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("the host sent protocol version {sent}, and this guest implements {ours}")]
    ProtocolVersion { sent: u32, ours: u32 },

    #[error("the host aborted the job: {reason}")]
    Aborted { reason: String },

    #[error("the job named no command to run")]
    EmptyArgv,

    #[error("executing {program} failed: {source}")]
    Exec {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the task exceeded the guest's own {seconds}s bound")]
    Timeout { seconds: u64 },

    #[error("the control channel failed: {source}")]
    Protocol {
        #[from]
        source: clyde_guest_api::ProtocolError,
    },
}

impl GuestError {
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }

    /// Which stage of the guest's own sequence this failure belongs to.
    pub fn stage(&self) -> FailureStage {
        match self {
            Self::PseudoMount { .. } => FailureStage::Pseudo,
            Self::ProtocolVersion { .. } | Self::Aborted { .. } | Self::Protocol { .. } => {
                FailureStage::Protocol
            }
            Self::NoSuchLabel { .. } | Self::DuplicateLabel { .. } => FailureStage::DriveDiscovery,
            Self::Mount { .. } => FailureStage::Mount,
            Self::EmptyArgv | Self::Exec { .. } => FailureStage::Exec,
            Self::Timeout { .. } => FailureStage::Timeout,
            Self::Unmount { .. } => FailureStage::Teardown,
            Self::Io { .. } => FailureStage::Teardown,
        }
    }
}

fn render(labels: &[String]) -> String {
    if labels.is_empty() {
        "none".to_owned()
    } else {
        labels.join(", ")
    }
}

pub type Result<T> = std::result::Result<T, GuestError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn every_failure_names_the_stage_it_belongs_to() {
        let cases = [
            (
                GuestError::NoSuchLabel {
                    label: "clyde-work".to_owned(),
                    available: vec!["clyde-cache".to_owned()],
                },
                FailureStage::DriveDiscovery,
            ),
            (GuestError::EmptyArgv, FailureStage::Exec),
            (GuestError::Timeout { seconds: 600 }, FailureStage::Timeout),
        ];
        for (error, expected) in cases {
            assert_eq!(error.stage(), expected, "{error}");
        }
    }

    #[test]
    fn a_missing_label_error_lists_what_was_visible() {
        let error = GuestError::NoSuchLabel {
            label: "clyde-work".to_owned(),
            available: vec!["clyde-cache".to_owned(), "clyde-deps".to_owned()],
        };
        let rendered = error.to_string();
        assert!(rendered.contains("clyde-work"));
        assert!(
            rendered.contains("clyde-cache, clyde-deps"),
            "the diagnostic has to say what the guest could see: {rendered}"
        );
    }

    #[test]
    fn no_visible_labels_reads_as_none_rather_than_empty() {
        let error = GuestError::NoSuchLabel {
            label: "clyde-work".to_owned(),
            available: Vec::new(),
        };
        assert!(error.to_string().contains("none"));
    }
}
