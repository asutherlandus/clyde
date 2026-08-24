//! Error types for `clyde-core`.
//!
//! Validation errors are structured rather than stringly typed so that callers
//! can render actionable diagnostics and tests can assert on the specific
//! failure rather than on message text (AGENTS.md: error handling).

use std::fmt;

/// A validation failure on construction of a domain type.
///
/// Every entity in this crate validates on construction and returns this error
/// rather than panicking, so malformed external input can never take a panic
/// path (AGENTS.md: production code must not panic).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("{kind} identifier must not be empty")]
    EmptyId { kind: &'static str },

    #[error("{kind} identifier {value:?} is malformed: expected {expected}")]
    MalformedId {
        kind: &'static str,
        value: String,
        expected: &'static str,
    },

    #[error("{kind} identifier exceeds the {max}-byte limit")]
    IdTooLong { kind: &'static str, max: usize },

    #[error("repository path must not be empty")]
    EmptyRepoPath,

    #[error("repository path {path:?} is absolute; paths are workspace-root-relative")]
    AbsoluteRepoPath { path: String },

    #[error("repository path {path:?} escapes the workspace root")]
    RepoPathEscapesRoot { path: String },

    #[error("repository path {path:?} contains an invalid component: {reason}")]
    InvalidRepoPathComponent { path: String, reason: &'static str },

    #[error("field {field} must not be empty")]
    EmptyField { field: &'static str },

    #[error("field {field} exceeds its {max}-byte limit")]
    FieldTooLong { field: &'static str, max: usize },

    #[error("duration {value:?} is malformed: expected a value like \"45m\" or \"10s\"")]
    MalformedDuration { value: String },

    #[error("duration must be greater than zero")]
    ZeroDuration,

    #[error("{field} must be greater than zero")]
    ZeroValue { field: &'static str },

    #[error("digest {value:?} is not a lowercase hex blake3 digest")]
    MalformedDigest { value: String },

    #[error("actor {actor} is not a human actor, which this operation requires")]
    NotHumanActor { actor: String },

    #[error("expiry {expires_at} is not after issue time {issued_at}")]
    ExpiryNotAfterIssue {
        issued_at: String,
        expires_at: String,
    },
}

/// An illegal state-machine transition.
///
/// State machines are explicit transition functions returning `Result`, never
/// mutable field assignment, so an illegal transition is a value a caller must
/// handle (Phase 0 deliverable 4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{entity} cannot move from {from} to {to}: {reason}")]
pub struct TransitionError {
    pub entity: &'static str,
    pub from: String,
    pub to: String,
    pub reason: TransitionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    /// The source state is terminal and nothing leaves it.
    Terminal,
    /// The transition is not part of the entity's state machine.
    NotPermitted,
}

impl fmt::Display for TransitionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal => f.write_str("the source state is terminal"),
            Self::NotPermitted => f.write_str("that transition is not in the state machine"),
        }
    }
}

impl TransitionError {
    pub(crate) fn terminal(
        entity: &'static str,
        from: impl fmt::Display,
        to: impl fmt::Display,
    ) -> Self {
        Self {
            entity,
            from: from.to_string(),
            to: to.to_string(),
            reason: TransitionReason::Terminal,
        }
    }

    pub(crate) fn not_permitted(
        entity: &'static str,
        from: impl fmt::Display,
        to: impl fmt::Display,
    ) -> Self {
        Self {
            entity,
            from: from.to_string(),
            to: to.to_string(),
            reason: TransitionReason::NotPermitted,
        }
    }
}
