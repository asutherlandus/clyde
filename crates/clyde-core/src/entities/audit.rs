//! Audit events.
//!
//! Append-only with a monotonic sequence number and a `prev_hash` chain, which
//! buys tamper-evidence cheaply (decisions: audit chain). The store API has no
//! update or delete path.
//!
//! Payload construction goes through a redaction helper, and there is no path
//! that serialises a credential-bearing type into a payload — enforced by not
//! implementing `Serialize` on those types (schema reference: audit events).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::{CanonicalError, Digest};
use crate::entities::baseline::AccessDrift;
use crate::entities::task::TaskType;
use crate::ids::{
    ActorId, ApprovalId, ArtifactId, BrokerOpId, LeaseId, MissionId, SnapshotId, TaskRunId,
    WorkspaceId,
};

/// The minimum event set for Phases 0-4 (schema reference).
///
/// Variants are explicit rather than a free-text `kind` string so that a new
/// event type is a compile-time change and the set stays reviewable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEventKind {
    WorkspaceRegistered,
    ConfigLoaded,
    MissionProposed,
    MissionApproved,
    MissionDenied,
    MissionActivated,
    MissionPaused,
    MissionResumed,
    MissionClosed,
    MissionRevoked,
    MissionExpired,
    LeaseIssued,
    LeaseDerived,
    LeaseRenewed,
    LeaseExpired,
    LeaseRevoked,
    LeaseExhausted,
    SessionBound,
    SessionRevoked,
    SandboxStarted,
    SandboxTerminated,
    TaskRequested {
        task: TaskType,
    },
    TaskAdmitted {
        task: TaskType,
    },
    TaskDenied {
        task: TaskType,
    },
    TaskStarted {
        task: TaskType,
    },
    TaskFinished {
        task: TaskType,
    },
    SnapshotCreated,
    ArtifactStored,
    EgressAttemptAllowed,
    EgressAttemptDenied,
    ApprovalRequested,
    ApprovalDecided,
    ApprovalExpired,
    BrokerOpRequested,
    BrokerOpExecuted,
    BrokerOpFailed,
    BrokerOpFrozen,
    BaselineProposed,
    BaselineConfirmed,
    BaselineAmended,
    BaselineReset,
    /// Recorded distinctly: a learn run is a wide-scope execution of exactly the
    /// code being constrained (D18).
    LearnModeInitiated,
    AccessDriftDetected {
        drift: AccessDrift,
    },
    PolicyDecisionRecorded,
    DaemonStarted,
    DaemonStopped,
}

impl AuditEventKind {
    /// Stable short name for CLI rendering and for filtering.
    pub fn name(&self) -> &'static str {
        match self {
            Self::WorkspaceRegistered => "workspace.registered",
            Self::ConfigLoaded => "config.loaded",
            Self::MissionProposed => "mission.proposed",
            Self::MissionApproved => "mission.approved",
            Self::MissionDenied => "mission.denied",
            Self::MissionActivated => "mission.activated",
            Self::MissionPaused => "mission.paused",
            Self::MissionResumed => "mission.resumed",
            Self::MissionClosed => "mission.closed",
            Self::MissionRevoked => "mission.revoked",
            Self::MissionExpired => "mission.expired",
            Self::LeaseIssued => "lease.issued",
            Self::LeaseDerived => "lease.derived",
            Self::LeaseRenewed => "lease.renewed",
            Self::LeaseExpired => "lease.expired",
            Self::LeaseRevoked => "lease.revoked",
            Self::LeaseExhausted => "lease.exhausted",
            Self::SessionBound => "session.bound",
            Self::SessionRevoked => "session.revoked",
            Self::SandboxStarted => "sandbox.started",
            Self::SandboxTerminated => "sandbox.terminated",
            Self::TaskRequested { .. } => "task.requested",
            Self::TaskAdmitted { .. } => "task.admitted",
            Self::TaskDenied { .. } => "task.denied",
            Self::TaskStarted { .. } => "task.started",
            Self::TaskFinished { .. } => "task.finished",
            Self::SnapshotCreated => "snapshot.created",
            Self::ArtifactStored => "artifact.stored",
            Self::EgressAttemptAllowed => "egress.allowed",
            Self::EgressAttemptDenied => "egress.denied",
            Self::ApprovalRequested => "approval.requested",
            Self::ApprovalDecided => "approval.decided",
            Self::ApprovalExpired => "approval.expired",
            Self::BrokerOpRequested => "broker.requested",
            Self::BrokerOpExecuted => "broker.executed",
            Self::BrokerOpFailed => "broker.failed",
            Self::BrokerOpFrozen => "broker.frozen",
            Self::BaselineProposed => "baseline.proposed",
            Self::BaselineConfirmed => "baseline.confirmed",
            Self::BaselineAmended => "baseline.amended",
            Self::BaselineReset => "baseline.reset",
            Self::LearnModeInitiated => "baseline.learn_mode_initiated",
            Self::AccessDriftDetected { .. } => "access.drift_detected",
            Self::PolicyDecisionRecorded => "policy.decision",
            Self::DaemonStarted => "daemon.started",
            Self::DaemonStopped => "daemon.stopped",
        }
    }

    /// Whether this event is one a reviewer should always see, regardless of
    /// filters. Used by mission review and by `audit show`.
    pub fn is_high_signal(&self) -> bool {
        matches!(
            self,
            Self::LearnModeInitiated
                | Self::AccessDriftDetected { .. }
                | Self::EgressAttemptDenied
                | Self::BrokerOpExecuted
                | Self::BrokerOpFrozen
                | Self::ApprovalDecided
                | Self::MissionRevoked
        )
    }
}

/// The fields an event is built from, before the chain assigns `seq` and hashes.
///
/// Separating this from [`AuditEvent`] means a caller cannot invent a sequence
/// number or a hash: only the store's append path can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventDraft {
    pub at: DateTime<Utc>,
    pub kind: AuditEventKind,
    pub workspace: Option<WorkspaceId>,
    pub mission: Option<MissionId>,
    pub lease: Option<LeaseId>,
    pub actor: Option<ActorId>,
    pub task_run: Option<TaskRunId>,
    pub snapshot: Option<SnapshotId>,
    pub artifacts: Vec<ArtifactId>,
    pub approval: Option<ApprovalId>,
    pub broker_op: Option<BrokerOpId>,
    /// Redacted at construction; never raw secrets.
    pub payload: serde_json::Value,
}

impl AuditEventDraft {
    /// Builds a draft with only a kind, for events that need no references.
    pub fn new(kind: AuditEventKind) -> Self {
        Self {
            at: Utc::now(),
            kind,
            workspace: None,
            mission: None,
            lease: None,
            actor: None,
            task_run: None,
            snapshot: None,
            artifacts: Vec::new(),
            approval: None,
            broker_op: None,
            payload: serde_json::Value::Null,
        }
    }

    pub fn workspace(mut self, id: WorkspaceId) -> Self {
        self.workspace = Some(id);
        self
    }

    pub fn mission(mut self, id: MissionId) -> Self {
        self.mission = Some(id);
        self
    }

    pub fn lease(mut self, id: LeaseId) -> Self {
        self.lease = Some(id);
        self
    }

    pub fn actor(mut self, id: ActorId) -> Self {
        self.actor = Some(id);
        self
    }

    pub fn task_run(mut self, id: TaskRunId) -> Self {
        self.task_run = Some(id);
        self
    }

    pub fn snapshot(mut self, id: SnapshotId) -> Self {
        self.snapshot = Some(id);
        self
    }

    pub fn artifacts(mut self, ids: Vec<ArtifactId>) -> Self {
        self.artifacts = ids;
        self
    }

    pub fn approval(mut self, id: ApprovalId) -> Self {
        self.approval = Some(id);
        self
    }

    pub fn broker_op(mut self, id: BrokerOpId) -> Self {
        self.broker_op = Some(id);
        self
    }

    /// Attaches a payload.
    ///
    /// The payload must already be redaction-safe: credential-bearing types do
    /// not implement `Serialize`, so this signature cannot accept one.
    pub fn payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// The genesis link, used as `prev_hash` for the first event.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// An appended, hash-chained audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Monotonic and gapless per database, starting at 1.
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: AuditEventKind,
    pub workspace: Option<WorkspaceId>,
    pub mission: Option<MissionId>,
    pub lease: Option<LeaseId>,
    pub actor: Option<ActorId>,
    pub task_run: Option<TaskRunId>,
    pub snapshot: Option<SnapshotId>,
    pub artifacts: Vec<ArtifactId>,
    pub approval: Option<ApprovalId>,
    pub broker_op: Option<BrokerOpId>,
    pub payload: serde_json::Value,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditEvent {
    /// Seals a draft into an event at `seq`, chained to `prev_hash`.
    pub fn seal(
        draft: AuditEventDraft,
        seq: u64,
        prev_hash: impl Into<String>,
    ) -> Result<Self, CanonicalError> {
        let prev_hash = prev_hash.into();
        let mut event = Self {
            seq,
            at: draft.at,
            kind: draft.kind,
            workspace: draft.workspace,
            mission: draft.mission,
            lease: draft.lease,
            actor: draft.actor,
            task_run: draft.task_run,
            snapshot: draft.snapshot,
            artifacts: draft.artifacts,
            approval: draft.approval,
            broker_op: draft.broker_op,
            payload: draft.payload,
            prev_hash,
            hash: String::new(),
        };
        event.hash = event.compute_hash()?.to_string();
        Ok(event)
    }

    /// Recomputes the event's own hash over its canonical encoding.
    pub fn compute_hash(&self) -> Result<Digest, CanonicalError> {
        // `hash` is excluded from its own preimage; everything else is included,
        // so any in-place edit changes the recomputed value.
        let preimage = serde_json::json!({
            "seq": self.seq,
            "at": self.at.to_rfc3339(),
            "kind": self.kind,
            "workspace": self.workspace,
            "mission": self.mission,
            "lease": self.lease,
            "actor": self.actor,
            "task_run": self.task_run,
            "snapshot": self.snapshot,
            "artifacts": self.artifacts,
            "approval": self.approval,
            "broker_op": self.broker_op,
            "payload": self.payload,
            "prev_hash": self.prev_hash,
        });
        Digest::of_canonical("clyde.audit-event.v1", &preimage)
    }
}

/// What is wrong with an audit chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainViolation {
    #[error(
        "event {seq} has hash {found}, but its content hashes to {expected}: it was edited in place"
    )]
    HashMismatch {
        seq: u64,
        expected: String,
        found: String,
    },
    #[error(
        "event {seq} links to {found}, but the previous event hashes to {expected}: an event was removed or reordered"
    )]
    BrokenLink {
        seq: u64,
        expected: String,
        found: String,
    },
    #[error("sequence jumps from {previous} to {found}: events are missing")]
    SequenceGap { previous: u64, found: u64 },
    #[error("the chain starts at {found} rather than 1: events were removed from the head")]
    MissingHead { found: u64 },
    #[error(
        "the chain ends at {found}, but the recorded head is {expected}: events were truncated from the tail"
    )]
    TruncatedTail { expected: u64, found: u64 },
    #[error("the chain ends with hash {found}, but the recorded head hash is {expected}")]
    HeadHashMismatch { expected: String, found: String },
    #[error("event {seq} could not be canonically encoded for verification")]
    NotEncodable { seq: u64 },
}

/// The recorded chain head, stored alongside the log.
///
/// Without it, removing events from the *tail* of a hash chain is undetectable:
/// the remaining prefix is internally consistent. The head is what makes tail
/// truncation visible (Phase 0 exit criterion: a test that detects truncation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditChainHead {
    pub seq: u64,
    pub hash: String,
}

/// Verifies a chain in sequence order.
///
/// `expected_head` is the store's recorded head. Passing `None` verifies
/// internal consistency only, which cannot detect tail truncation.
pub fn verify_chain(
    events: &[AuditEvent],
    expected_head: Option<&AuditChainHead>,
) -> Result<(), ChainViolation> {
    let mut previous: Option<&AuditEvent> = None;
    for event in events {
        let computed = event
            .compute_hash()
            .map_err(|_| ChainViolation::NotEncodable { seq: event.seq })?;
        if computed.as_str() != event.hash {
            return Err(ChainViolation::HashMismatch {
                seq: event.seq,
                expected: computed.to_string(),
                found: event.hash.clone(),
            });
        }
        match previous {
            None => {
                if event.seq != 1 {
                    return Err(ChainViolation::MissingHead { found: event.seq });
                }
                if event.prev_hash != GENESIS_HASH {
                    return Err(ChainViolation::BrokenLink {
                        seq: event.seq,
                        expected: GENESIS_HASH.to_owned(),
                        found: event.prev_hash.clone(),
                    });
                }
            }
            Some(previous) => {
                if event.seq != previous.seq.saturating_add(1) {
                    return Err(ChainViolation::SequenceGap {
                        previous: previous.seq,
                        found: event.seq,
                    });
                }
                if event.prev_hash != previous.hash {
                    return Err(ChainViolation::BrokenLink {
                        seq: event.seq,
                        expected: previous.hash.clone(),
                        found: event.prev_hash.clone(),
                    });
                }
            }
        }
        previous = Some(event);
    }

    match (expected_head, previous) {
        (Some(head), Some(last)) => {
            if head.seq != last.seq {
                return Err(ChainViolation::TruncatedTail {
                    expected: head.seq,
                    found: last.seq,
                });
            }
            if head.hash != last.hash {
                return Err(ChainViolation::HeadHashMismatch {
                    expected: head.hash.clone(),
                    found: last.hash.clone(),
                });
            }
            Ok(())
        }
        (Some(head), None) => Err(ChainViolation::TruncatedTail {
            expected: head.seq,
            found: 0,
        }),
        (None, _) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    fn chain(len: u64) -> Vec<AuditEvent> {
        let mut events = Vec::new();
        let mut prev = GENESIS_HASH.to_owned();
        for seq in 1..=len {
            let draft = AuditEventDraft::new(AuditEventKind::MissionProposed)
                .payload(serde_json::json!({"n": seq}));
            let event = AuditEvent::seal(draft, seq, prev.clone()).unwrap();
            prev = event.hash.clone();
            events.push(event);
        }
        events
    }

    fn head(events: &[AuditEvent]) -> AuditChainHead {
        let last = events.last().unwrap();
        AuditChainHead {
            seq: last.seq,
            hash: last.hash.clone(),
        }
    }

    #[test]
    fn a_well_formed_chain_verifies() {
        let events = chain(5);
        let head = head(&events);
        assert!(verify_chain(&events, Some(&head)).is_ok());
    }

    #[test]
    fn in_place_edits_are_detected() {
        let mut events = chain(3);
        events[1].payload = serde_json::json!({"n": 99});
        let violation = verify_chain(&events, None).expect_err("edit must be detected");
        assert!(matches!(
            violation,
            ChainViolation::HashMismatch { seq: 2, .. }
        ));
    }

    #[test]
    fn removing_a_middle_event_breaks_the_chain() {
        let mut events = chain(4);
        let head = head(&events);
        events.remove(1);
        let violation = verify_chain(&events, Some(&head)).expect_err("gap must be detected");
        assert!(matches!(
            violation,
            ChainViolation::SequenceGap {
                previous: 1,
                found: 3
            }
        ));
    }

    #[test]
    fn tail_truncation_is_detected_against_the_recorded_head() {
        let events = chain(6);
        let head = head(&events);
        let truncated = &events[..4];
        // Internal consistency alone cannot see this, which is exactly why the
        // head is recorded.
        assert!(verify_chain(truncated, None).is_ok());
        let violation =
            verify_chain(truncated, Some(&head)).expect_err("truncation must be detected");
        assert!(matches!(
            violation,
            ChainViolation::TruncatedTail {
                expected: 6,
                found: 4
            }
        ));
    }

    #[test]
    fn head_truncation_is_detected() {
        let events = chain(4);
        let violation = verify_chain(&events[1..], None).expect_err("head removal detected");
        assert!(matches!(
            violation,
            ChainViolation::MissingHead { found: 2 }
        ));
    }

    #[test]
    fn reordering_breaks_the_links() {
        let mut events = chain(3);
        events.swap(0, 1);
        assert!(verify_chain(&events, None).is_err());
    }

    #[test]
    fn a_rewritten_chain_is_caught_by_the_head_hash() {
        let original = chain(3);
        let head = head(&original);
        // An attacker who re-seals the whole chain with different content gets a
        // self-consistent log, but not one matching the recorded head.
        let mut forged = Vec::new();
        let mut prev = GENESIS_HASH.to_owned();
        for seq in 1..=3 {
            let draft = AuditEventDraft::new(AuditEventKind::MissionApproved)
                .payload(serde_json::json!({"n": seq}));
            let event = AuditEvent::seal(draft, seq, prev.clone()).unwrap();
            prev = event.hash.clone();
            forged.push(event);
        }
        assert!(verify_chain(&forged, None).is_ok());
        assert!(matches!(
            verify_chain(&forged, Some(&head)),
            Err(ChainViolation::HeadHashMismatch { .. })
        ));
    }

    #[test]
    fn an_empty_log_with_a_recorded_head_is_a_violation() {
        let events = chain(2);
        let head = head(&events);
        assert!(matches!(
            verify_chain(&[], Some(&head)),
            Err(ChainViolation::TruncatedTail { found: 0, .. })
        ));
        assert!(verify_chain(&[], None).is_ok());
    }

    #[test]
    fn learn_mode_and_drift_are_high_signal() {
        assert!(AuditEventKind::LearnModeInitiated.is_high_signal());
        assert!(
            AuditEventKind::AccessDriftDetected {
                drift: AccessDrift::PathOutsideBaseline {
                    path: crate::repo_path::RepoPath::parse("x").unwrap()
                }
            }
            .is_high_signal()
        );
        assert!(!AuditEventKind::SnapshotCreated.is_high_signal());
    }

    #[test]
    fn event_names_are_stable_and_unique() {
        let kinds = [
            AuditEventKind::MissionProposed,
            AuditEventKind::MissionApproved,
            AuditEventKind::LeaseIssued,
            AuditEventKind::SessionBound,
            AuditEventKind::TaskRequested {
                task: TaskType::RustCheck,
            },
            AuditEventKind::EgressAttemptDenied,
            AuditEventKind::LearnModeInitiated,
        ];
        let mut names: Vec<&str> = kinds.iter().map(AuditEventKind::name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
