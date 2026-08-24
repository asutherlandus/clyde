//! `clyde-core`: identifiers, entities, state machines, validation, and error
//! types for Clyde.
//!
//! This crate is the bottom of the dependency graph. It performs no I/O, holds
//! no global state, and depends on nothing else in the workspace, so every type
//! and transition here is testable in isolation (Phase 0 deliverable 3).
//!
//! Three conventions run through it:
//!
//! - **Validate on construction.** A value in hand is well formed. Parsing
//!   returns [`error::ValidationError`] rather than panicking, so malformed
//!   external input can never take a panic path.
//! - **State machines are functions.** Transitions return `Result`, so an
//!   illegal transition is a value the caller must handle rather than a field
//!   assignment that silently succeeds.
//! - **Redaction is structural.** Credential-bearing types do not implement
//!   `Serialize`, so they cannot reach a log line or an audit payload by
//!   accident (see [`redact::Redacted`]).

pub mod digest;
pub mod duration;
pub mod entities;
pub mod error;
pub mod ids;
pub mod redact;
pub mod repo_path;

pub use digest::Digest;
pub use duration::HumanDuration;
pub use error::{TransitionError, TransitionReason, ValidationError};
pub use ids::{
    ActorId, ApprovalId, ArtifactId, BrokerOpId, LeaseId, MissionId, PolicyDecisionId, SnapshotId,
    TaskRunId, WorkspaceId,
};
pub use redact::Redacted;
pub use repo_path::RepoPath;

/// Re-exports so downstream crates can `use clyde_core::budget::Budget` and
/// friends without reaching through `entities::`.
pub use entities::classification;
pub use entities::{
    actor, approval, artifact, audit, baseline, broker, budget, decision, egress, lease, mission,
    session, snapshot, task, workspace,
};
