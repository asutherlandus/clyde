//! Store errors.

use clyde_core::error::{TransitionError, ValidationError};
use clyde_core::ids::{
    ApprovalId, ArtifactId, BrokerOpId, LeaseId, MissionId, SnapshotId, TaskRunId, WorkspaceId,
};

/// What went wrong in the store.
///
/// Constraint violations that express a design invariant have their own variants
/// so callers can render them as policy conditions rather than as database
/// errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("workspace {0} is not registered")]
    UnknownWorkspace(WorkspaceId),
    #[error("mission {0} does not exist")]
    UnknownMission(MissionId),
    #[error("lease {0} does not exist")]
    UnknownLease(LeaseId),
    #[error("task run {0} does not exist")]
    UnknownTaskRun(TaskRunId),
    #[error("snapshot {0} does not exist")]
    UnknownSnapshot(SnapshotId),
    #[error("artifact {0} does not exist")]
    UnknownArtifact(ArtifactId),
    #[error("approval {0} does not exist")]
    UnknownApproval(ApprovalId),
    #[error("brokered operation {0} does not exist")]
    UnknownBrokerOp(BrokerOpId),

    /// One active mission per workspace (D16).
    #[error(
        "workspace {workspace} already has mission {existing} in a non-terminal state; close or revoke it first"
    )]
    MissionAlreadyActive {
        workspace: WorkspaceId,
        existing: MissionId,
    },

    #[error("an entity with this identifier already exists: {0}")]
    AlreadyExists(String),

    /// An `ApproveOnce` decision that has already been used.
    #[error("approval {0} has already been consumed")]
    ApprovalAlreadyConsumed(ApprovalId),

    #[error("approval {0} has no recorded decision")]
    ApprovalUndecided(ApprovalId),

    #[error(transparent)]
    Transition(#[from] TransitionError),

    #[error(transparent)]
    Validation(#[from] ValidationError),

    /// The audit chain does not verify. Treated as an error rather than a
    /// warning, because a log that cannot be trusted is worse than none.
    #[error("audit chain is broken: {0}")]
    AuditChain(#[from] clyde_core::audit::ChainViolation),

    #[error("stored record could not be decoded: {context}: {source}")]
    Decode {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("record could not be encoded: {context}: {source}")]
    Encode {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("database error: {0}")]
    Backend(String),

    #[error("store is unavailable: {0}")]
    Unavailable(&'static str),
}

impl StoreError {
    pub(crate) fn decode(context: &'static str, source: serde_json::Error) -> Self {
        Self::Decode { context, source }
    }

    pub(crate) fn encode(context: &'static str, source: serde_json::Error) -> Self {
        Self::Encode { context, source }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
