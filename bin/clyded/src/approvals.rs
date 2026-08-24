//! The approval manager (Phase 1 deliverable 8).
//!
//! Approval requests carry a `request_digest` over the exact normalised request,
//! an expiry, and the alternatives the policy engine suggested. Decisions are
//! made only on the admin socket by a human actor.
//!
//! `ApproveOnce` consumption is single-use and transactional: consuming an
//! approval and performing the approved action either both happen or neither
//! does. The store enforces the single-use half; this module enforces the
//! ordering — consume first, act second, and unwind by recording the failure
//! rather than by silently re-arming the approval.

use chrono::{Duration, Utc};
use clyde_api::admin::ApprovalView;
use clyde_core::Digest;
use clyde_core::approval::{ApprovalDecision, ApprovalRequest, ApprovalSubject, Decision};
use clyde_core::audit::AuditEventKind;
use clyde_core::ids::{ActorId, ApprovalId, LeaseId, MissionId};
use clyde_store::ApprovalRecord;

use crate::audit;
use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};

/// How long an approval request stays valid.
///
/// Approvals go stale rather than lingering: a request approved an hour after it
/// was made is being approved against a tree that has probably moved.
pub const DEFAULT_TTL_MINUTES: i64 = 30;

/// What a request needs beyond the identifiers.
#[derive(Debug, Clone)]
pub struct ApprovalContext {
    pub subject: ApprovalSubject,
    pub request_digest: Digest,
    pub reason: String,
    pub alternatives: Vec<String>,
    /// Why the previous attempt failed, where this arose from a denial. A prompt
    /// that does not say what went wrong invites reflexive approval.
    pub prior_failure: Option<String>,
    /// The exact host allowlist this would permit.
    pub egress_hosts: Vec<String>,
    pub egress_profile: String,
    pub credentials: String,
    pub outputs: Vec<String>,
    pub lockfile_change: Option<String>,
    pub inventory_diff: Vec<String>,
    pub task_evidence: Vec<String>,
    pub caveats: Vec<String>,
}

/// Creates an approval request.
pub fn request(
    daemon: &Daemon,
    mission: &MissionId,
    lease: &LeaseId,
    actor: &ActorId,
    context: ApprovalContext,
) -> Result<ApprovalRequest> {
    let now = Utc::now();
    let request = ApprovalRequest {
        id: clyde_core::ids::new::approval_id()?,
        mission: mission.clone(),
        lease: lease.clone(),
        actor: actor.clone(),
        subject: context.subject.clone(),
        request_digest: context.request_digest.clone(),
        reason: context.reason.clone(),
        alternatives: context.alternatives.clone(),
        created_at: now,
        expires_at: now + Duration::minutes(DEFAULT_TTL_MINUTES),
    };
    request.validate()?;
    daemon.store.insert_approval_request(request.clone())?;

    // The rendered context is stored as the audit payload, because the prompt a
    // human saw is part of what a later reviewer needs.
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::ApprovalRequested,
            serde_json::json!({
                "subject": context.subject.kind_name(),
                "reason": context.reason,
                "egress_profile": context.egress_profile,
                "egress_hosts": context.egress_hosts,
                "prior_failure": context.prior_failure,
                "inventory_diff": context.inventory_diff,
                "digest": context.request_digest.to_string(),
            }),
        )
        .mission(mission.clone())
        .lease(lease.clone())
        .actor(actor.clone())
        .approval(request.id.clone()),
    );
    Ok(request)
}

/// Records a decision.
///
/// The human check is here as well as at the transport layer, so a store that
/// somehow held a non-human decision could not be used to authorise anything.
pub fn decide(
    daemon: &Daemon,
    approval: &ApprovalId,
    by: &ActorId,
    decision: Decision,
    note: Option<String>,
) -> Result<ApprovalDecision> {
    if !by.is_human() {
        return Err(DaemonError::invalid(
            "only a human actor on the admin socket can decide an approval",
        ));
    }
    let record = daemon.store.get_approval(approval)?;
    if record.request.is_expired_at(Utc::now()) {
        audit::record(
            daemon.store.as_ref(),
            audit::draft(AuditEventKind::ApprovalExpired, serde_json::Value::Null)
                .mission(record.request.mission.clone())
                .approval(approval.clone()),
        );
        return Err(DaemonError::invalid(
            "this approval request has expired; the actor must request it again",
        ));
    }
    if record.decision.is_some() {
        return Err(DaemonError::invalid(
            "this approval has already been decided",
        ));
    }
    if matches!(decision, Decision::ApproveForMission) {
        let mission = daemon.store.get_mission(&record.request.mission)?;
        if !mission.approval_policy.allow_mission_scoped_approvals {
            return Err(DaemonError::invalid(
                "this mission does not permit mission-scoped approvals",
            ));
        }
    }

    let decided = ApprovalDecision {
        request: approval.clone(),
        decided_by: by.clone(),
        decision,
        decided_at: Utc::now(),
        note,
        consumed_at: None,
    };
    decided.validate()?;
    daemon.store.record_approval_decision(decided.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::ApprovalDecided,
            serde_json::json!({
                "decision": decision.name(),
                "by": by.as_str(),
                "subject": record.request.subject.kind_name(),
            }),
        )
        .mission(record.request.mission.clone())
        .actor(by.clone())
        .approval(approval.clone()),
    );
    Ok(decided)
}

/// Finds an approval that authorises an action with this digest, now.
pub fn find_authorising(
    daemon: &Daemon,
    mission: &MissionId,
    digest: &Digest,
) -> Result<Option<ApprovalRecord>> {
    Ok(daemon
        .store
        .find_authorising_approval(mission, digest, Utc::now())?)
}

/// Consumes a single-use approval.
///
/// Called immediately before the approved action. A failure here means the
/// action must not proceed.
pub fn consume(daemon: &Daemon, approval: &ApprovalId) -> Result<()> {
    daemon.store.consume_approval(approval, Utc::now())?;
    Ok(())
}

/// Renders a request for the operator.
pub fn render(record: &ApprovalRecord, context: Option<&ApprovalContext>) -> ApprovalView {
    let request = &record.request;
    let summary = match &request.subject {
        ApprovalSubject::TaskEscalation { task, egress } => {
            format!("run {} with egress {}", task.name(), egress.name())
        }
        ApprovalSubject::ScopeExpansion { summary }
        | ApprovalSubject::BrokeredOperation { summary }
        | ApprovalSubject::DependencyInventory { summary } => summary.clone(),
        ApprovalSubject::LeaseRenewal { lease } => format!("renew lease {lease}"),
        ApprovalSubject::AccessBaseline { target } => {
            format!("confirm the access baseline for {target}")
        }
    };
    ApprovalView {
        approval: request.id.to_string(),
        mission: request.mission.to_string(),
        actor: request.actor.to_string(),
        subject: request.subject.kind_name().to_owned(),
        summary,
        reason: request.reason.clone(),
        prior_failure: context.and_then(|context| context.prior_failure.clone()),
        alternatives: request.alternatives.clone(),
        egress_hosts: context
            .map(|context| context.egress_hosts.clone())
            .unwrap_or_default(),
        egress_profile: context
            .map(|context| context.egress_profile.clone())
            .unwrap_or_else(|| "none".to_owned()),
        credentials: context
            .map(|context| context.credentials.clone())
            .unwrap_or_else(|| "none".to_owned()),
        outputs: context
            .map(|context| context.outputs.clone())
            .unwrap_or_default(),
        lockfile_change: context.and_then(|context| context.lockfile_change.clone()),
        inventory_diff: context
            .map(|context| context.inventory_diff.clone())
            .unwrap_or_default(),
        task_evidence: context
            .map(|context| context.task_evidence.clone())
            .unwrap_or_default(),
        caveats: context
            .map(|context| context.caveats.clone())
            .unwrap_or_default(),
        expires_at: request.expires_at,
        request_digest: request.request_digest.to_string(),
    }
}

/// The caveats every egress-bearing approval must state.
///
/// Stated rather than implied: allowlisting is by destination, and a prompt that
/// omits what it cannot promise is worse than no prompt.
pub fn egress_caveats() -> Vec<String> {
    vec![
        "allowlisting is by destination host, not by content: an allowlisted host can be sent arbitrary bytes".to_owned(),
        "TLS is end-to-end for this profile, so Clyde sees no payload and cannot filter it".to_owned(),
        "byte budgets bound bulk transfer; they do not detect low-volume signalling".to_owned(),
    ]
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
    use clyde_core::classification::EgressProfile;
    use clyde_core::task::TaskType;

    fn record(subject: ApprovalSubject) -> ApprovalRecord {
        let now = Utc::now();
        ApprovalRecord {
            request: ApprovalRequest {
                id: clyde_core::ids::new::approval_id().unwrap(),
                mission: clyde_core::ids::new::mission_id().unwrap(),
                lease: clyde_core::ids::new::lease_id().unwrap(),
                actor: ActorId::parse("agent:claude").unwrap(),
                subject,
                request_digest: Digest::of_bytes(b"x"),
                reason: "needed".to_owned(),
                alternatives: vec!["narrow the scope".to_owned()],
                created_at: now,
                expires_at: now + Duration::minutes(15),
            },
            decision: None,
        }
    }

    #[test]
    fn an_escalation_renders_the_task_and_its_egress() {
        let record = record(ApprovalSubject::TaskEscalation {
            task: TaskType::RustResolveDeps,
            egress: EgressProfile::RustRegistry,
        });
        let view = render(&record, None);
        assert!(view.summary.contains("rust.resolve-deps"));
        assert!(view.summary.contains("rust-registry"));
        assert_eq!(view.alternatives.len(), 1);
    }

    #[test]
    fn the_rendered_prompt_carries_the_prior_failure_and_the_caveats() {
        let record = record(ApprovalSubject::TaskEscalation {
            task: TaskType::RustResolveDeps,
            egress: EgressProfile::RustRegistry,
        });
        let context = ApprovalContext {
            subject: record.request.subject.clone(),
            request_digest: record.request.request_digest.clone(),
            reason: record.request.reason.clone(),
            alternatives: Vec::new(),
            prior_failure: Some("rust.check failed: serde is not in the bundle".to_owned()),
            egress_hosts: vec!["static.crates.io".to_owned()],
            egress_profile: "rust-registry".to_owned(),
            credentials: "none".to_owned(),
            outputs: vec!["dependency bundle".to_owned()],
            lockfile_change: Some("4 additions".to_owned()),
            inventory_diff: vec!["+ serde_derive 1.0".to_owned()],
            task_evidence: Vec::new(),
            caveats: egress_caveats(),
        };
        let view = render(&record, Some(&context));
        assert!(view.prior_failure.is_some());
        assert_eq!(view.egress_hosts, vec!["static.crates.io".to_owned()]);
        assert!(
            view.caveats
                .iter()
                .any(|caveat| caveat.contains("destination host")),
            "the prompt must state that allowlisting is by destination"
        );
        assert!(!view.inventory_diff.is_empty());
    }

    #[test]
    fn the_egress_caveats_state_what_the_model_does_not_promise() {
        let caveats = egress_caveats();
        assert!(
            caveats
                .iter()
                .any(|caveat| caveat.contains("not by content"))
        );
        assert!(
            caveats
                .iter()
                .any(|caveat| caveat.contains("low-volume signalling"))
        );
    }

    #[test]
    fn a_baseline_confirmation_names_its_target() {
        let record = record(ApprovalSubject::AccessBaseline {
            target: "crates/core".to_owned(),
        });
        assert!(render(&record, None).summary.contains("crates/core"));
    }
}
