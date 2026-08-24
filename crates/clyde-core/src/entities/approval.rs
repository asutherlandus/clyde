//! Approvals.
//!
//! An approval is bound to a `request_digest` over the exact normalised request.
//! An approved push is approved for one commit, one remote, one refspec — a later
//! request that differs in any of those does not match (schema reference:
//! Approval invariants).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::entities::classification::EgressProfile;
use crate::entities::task::TaskType;
use crate::error::ValidationError;
use crate::ids::{ActorId, ApprovalId, LeaseId, MissionId};

/// What is being approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", rename_all = "snake_case")]
pub enum ApprovalSubject {
    /// A task beyond the lease's current authority.
    TaskEscalation {
        task: TaskType,
        egress: EgressProfile,
    },
    /// A widening of repo or task scope.
    ScopeExpansion {
        summary: String,
    },
    LeaseRenewal {
        lease: LeaseId,
    },
    /// A brokered operation such as a push.
    BrokeredOperation {
        summary: String,
    },
    /// Confirmation of an access baseline or an amendment to one (D18).
    AccessBaseline {
        target: String,
    },
    /// A dependency bundle's code-execution inventory change (Phase 3).
    DependencyInventory {
        summary: String,
    },
}

impl ApprovalSubject {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::TaskEscalation { .. } => "task_escalation",
            Self::ScopeExpansion { .. } => "scope_expansion",
            Self::LeaseRenewal { .. } => "lease_renewal",
            Self::BrokeredOperation { .. } => "brokered_operation",
            Self::AccessBaseline { .. } => "access_baseline",
            Self::DependencyInventory { .. } => "dependency_inventory",
        }
    }
}

/// A pending approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub mission: MissionId,
    pub lease: LeaseId,
    /// The requesting actor.
    pub actor: ActorId,
    pub subject: ApprovalSubject,
    /// Digest over the exact normalised request this approval covers.
    pub request_digest: Digest,
    pub reason: String,
    /// Narrower options the policy engine suggested, shown to the human.
    pub alternatives: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Approvals go stale rather than lingering.
    pub expires_at: DateTime<Utc>,
}

impl ApprovalRequest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.reason.trim().is_empty() {
            return Err(ValidationError::EmptyField { field: "reason" });
        }
        if self.reason.len() > 4096 {
            return Err(ValidationError::FieldTooLong {
                field: "reason",
                max: 4096,
            });
        }
        if self.expires_at <= self.created_at {
            return Err(ValidationError::ExpiryNotAfterIssue {
                issued_at: self.created_at.to_rfc3339(),
                expires_at: self.expires_at.to_rfc3339(),
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// The decision a human made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Single-use, consumed transactionally with the approved action.
    ApproveOnce,
    /// A policy relaxation scoped to one mission, shown as such in review.
    ApproveForMission,
    Deny,
}

impl Decision {
    pub fn is_approval(self) -> bool {
        matches!(self, Self::ApproveOnce | Self::ApproveForMission)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ApproveOnce => "approve_once",
            Self::ApproveForMission => "approve_for_mission",
            Self::Deny => "deny",
        }
    }
}

/// A recorded decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub request: ApprovalId,
    /// Must be a human, authenticated on the admin socket. Enforced at the
    /// transport layer, not by checking this field — but validated here too, so
    /// a store that somehow held a non-human decision cannot be used.
    pub decided_by: ActorId,
    pub decision: Decision,
    pub decided_at: DateTime<Utc>,
    pub note: Option<String>,
    /// Set when an `ApproveOnce` decision has been consumed.
    pub consumed_at: Option<DateTime<Utc>>,
}

impl ApprovalDecision {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.decided_by.is_human() {
            return Err(ValidationError::NotHumanActor {
                actor: self.decided_by.to_string(),
            });
        }
        Ok(())
    }

    /// Whether this decision can authorise an action with `digest` now.
    ///
    /// Fails closed: an expired request, a consumed single-use approval, a
    /// denial, or a digest mismatch all yield `false`.
    pub fn authorises(
        &self,
        request: &ApprovalRequest,
        digest: &Digest,
        now: DateTime<Utc>,
    ) -> bool {
        if !self.decision.is_approval() {
            return false;
        }
        if request.is_expired_at(now) {
            return false;
        }
        if !request.request_digest.matches(digest) {
            return false;
        }
        match self.decision {
            Decision::ApproveOnce => self.consumed_at.is_none(),
            Decision::ApproveForMission => true,
            Decision::Deny => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::ids;

    fn request(digest: Digest, created: DateTime<Utc>) -> ApprovalRequest {
        ApprovalRequest {
            id: ids::new::approval_id().unwrap(),
            mission: ids::new::mission_id().unwrap(),
            lease: ids::new::lease_id().unwrap(),
            actor: ActorId::parse("agent:claude").unwrap(),
            subject: ApprovalSubject::BrokeredOperation {
                summary: "push to origin".to_owned(),
            },
            request_digest: digest,
            reason: "needs review".to_owned(),
            alternatives: vec![],
            created_at: created,
            expires_at: created + chrono::Duration::minutes(15),
        }
    }

    fn decision(kind: Decision, by: &str) -> ApprovalDecision {
        ApprovalDecision {
            request: ids::new::approval_id().unwrap(),
            decided_by: ActorId::parse(by).unwrap(),
            decision: kind,
            decided_at: Utc::now(),
            note: None,
            consumed_at: None,
        }
    }

    #[test]
    fn only_a_human_decision_validates() {
        assert!(
            decision(Decision::ApproveOnce, "human:andrew")
                .validate()
                .is_ok()
        );
        assert!(matches!(
            decision(Decision::ApproveOnce, "agent:claude").validate(),
            Err(ValidationError::NotHumanActor { .. })
        ));
    }

    #[test]
    fn a_different_request_does_not_match_the_approval() {
        let now = Utc::now();
        let approved = Digest::of_bytes(b"push origin refs/heads/feature abc");
        let tampered = Digest::of_bytes(b"push origin refs/heads/main abc");
        let request = request(approved.clone(), now);
        let decision = decision(Decision::ApproveOnce, "human:andrew");
        assert!(decision.authorises(&request, &approved, now));
        assert!(
            !decision.authorises(&request, &tampered, now),
            "an altered request must not be covered by the approval"
        );
    }

    #[test]
    fn expired_requests_cannot_be_consumed() {
        let now = Utc::now();
        let digest = Digest::of_bytes(b"x");
        let request = request(digest.clone(), now - chrono::Duration::hours(1));
        let decision = decision(Decision::ApproveOnce, "human:andrew");
        assert!(!decision.authorises(&request, &digest, now));
    }

    #[test]
    fn approve_once_is_single_use_but_for_mission_is_not() {
        let now = Utc::now();
        let digest = Digest::of_bytes(b"x");
        let request = request(digest.clone(), now);
        let consumed = ApprovalDecision {
            consumed_at: Some(now),
            ..decision(Decision::ApproveOnce, "human:andrew")
        };
        assert!(!consumed.authorises(&request, &digest, now));
        let for_mission = ApprovalDecision {
            consumed_at: Some(now),
            ..decision(Decision::ApproveForMission, "human:andrew")
        };
        assert!(for_mission.authorises(&request, &digest, now));
    }

    #[test]
    fn a_denial_never_authorises() {
        let now = Utc::now();
        let digest = Digest::of_bytes(b"x");
        let request = request(digest.clone(), now);
        assert!(!decision(Decision::Deny, "human:andrew").authorises(&request, &digest, now));
    }

    #[test]
    fn request_validation_bounds_reason_and_expiry() {
        let now = Utc::now();
        let mut r = request(Digest::of_bytes(b"x"), now);
        assert!(r.validate().is_ok());
        r.reason = String::new();
        assert!(r.validate().is_err());
        let mut r = request(Digest::of_bytes(b"x"), now);
        r.expires_at = r.created_at;
        assert!(r.validate().is_err());
    }
}
