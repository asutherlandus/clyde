//! Mission review (Phase 4 deliverable 8).
//!
//! The end-to-end surface: objective, files changed, tasks run with pass/fail
//! and policy digests, escalations and their outcomes, approvals granted, egress
//! attempts, brokered operations, budget consumed, and the closing diff. It is
//! the last MVP deliverable for a reason — everything before it feeds it.

use std::sync::Arc;

use clyde_api::admin::{MissionReview, TaskSummary};
use clyde_core::ids::MissionId;
use clyde_store::AuditFilter;

use crate::daemon::Daemon;
use crate::error::Result;

/// Builds the review for a mission.
pub async fn build(daemon: &Arc<Daemon>, mission: &MissionId) -> Result<MissionReview> {
    let record = daemon.store.get_mission(mission)?;
    let workspace = daemon.store.get_workspace(&record.workspace)?;

    let paths: Vec<String> = record
        .scope
        .edit_paths
        .iter()
        .map(ToString::to_string)
        .collect();
    // Computed against the live tree, which is what the mission was working in.
    // A closed mission's diff is the one stored at closeout; this recomputation
    // is what makes an open mission reviewable too.
    let diff = clyde_git::diff::workspace_diff(&daemon.git, &workspace.root, &paths)
        .await
        .unwrap_or_default();

    let tasks: Vec<TaskSummary> = daemon
        .store
        .list_task_runs(mission)?
        .into_iter()
        .map(|run| TaskSummary {
            task_run: run.id.to_string(),
            task: run.request.task.name().to_owned(),
            state: run.state.to_string(),
            classification: run
                .outcome
                .as_ref()
                .map(|outcome| outcome.classification.name().to_owned()),
            policy_digest: run.policy_digest.to_string(),
            principal: run.request.principal.kind_name().to_owned(),
            posture: run.posture.name().to_owned(),
        })
        .collect();

    let approvals = daemon.store.list_approvals(mission)?;
    let escalations: Vec<String> = approvals
        .iter()
        .filter(|record| {
            matches!(
                record.request.subject,
                clyde_core::approval::ApprovalSubject::TaskEscalation { .. }
            )
        })
        .map(|record| {
            format!(
                "{}: {} — {}",
                record.request.id,
                record.request.reason,
                record
                    .decision
                    .as_ref()
                    .map(|decision| decision.decision.name())
                    .unwrap_or("undecided")
            )
        })
        .collect();
    let granted: Vec<String> = approvals
        .iter()
        .filter_map(|record| {
            record.decision.as_ref().map(|decision| {
                format!(
                    "{} {} by {}",
                    record.request.subject.kind_name(),
                    decision.decision.name(),
                    decision.decided_by
                )
            })
        })
        .collect();

    // Refusals are surfaced prominently rather than buried: a refused attempt is
    // a first-class signal.
    let mut egress: Vec<String> = daemon
        .store
        .list_mission_egress_attempts(mission)?
        .into_iter()
        .map(|attempt| {
            format!(
                "{} {} {} in={} out={}{}",
                attempt.profile,
                attempt.host,
                if attempt.was_allowed() {
                    "allowed"
                } else {
                    "REFUSED"
                },
                attempt.bytes_in,
                attempt.bytes_out,
                attempt
                    .denial_reason
                    .map(|reason| format!(" ({})", reason.render()))
                    .unwrap_or_default()
            )
        })
        .collect();
    egress.sort_by_key(|line| !line.contains("REFUSED"));

    let brokered: Vec<String> = daemon
        .store
        .list_broker_ops(mission)?
        .into_iter()
        .map(|operation| {
            format!(
                "{} {} {}",
                operation.kind.name(),
                operation.state,
                operation.result_summary.unwrap_or_default()
            )
        })
        .collect();

    let leases = daemon.store.list_leases(mission)?;
    let budget_consumed = leases
        .first()
        .map(|lease| {
            format!(
                "{} of {} task runs, {} sub-agents, {} egress requests",
                lease.usage.task_runs,
                lease.budget.max_task_runs,
                lease.usage.subagents,
                lease.usage.egress_requests
            )
        })
        .unwrap_or_else(|| "no lease was issued".to_owned());

    // The audit chain is verified as part of review: a review over a log that
    // does not verify is worth less than one that says so.
    let audit_intact = daemon.store.verify_audit().is_ok();
    let _ = daemon
        .store
        .list_audit(&AuditFilter::for_mission(mission.clone()))?;

    let closing_diff = daemon
        .store
        .list_artifacts(mission)?
        .into_iter()
        .find(|artifact| artifact.kind == clyde_core::artifact::ArtifactKind::Diff)
        .map(|artifact| artifact.id.to_string());

    // Distinct, in the order first seen. If a mission spans a posture change,
    // review must show both rather than the current one (D26).
    let mut postures: Vec<String> = Vec::new();
    for task in &tasks {
        if !postures.contains(&task.posture) {
            postures.push(task.posture.clone());
        }
    }
    if postures.is_empty() {
        // No task ran, so nothing happened under a posture. Report the one in
        // force now rather than an empty list, which would read as "unknown".
        postures.push(daemon.posture.name().to_owned());
    }

    Ok(MissionReview {
        mission: record.id.to_string(),
        objective: record.objective,
        state: record.state.to_string(),
        files_changed: {
            let mut files = diff.changed_paths.clone();
            files.extend(diff.untracked_paths.clone());
            files.sort();
            files.dedup();
            files
        },
        diff_stat: diff.stat.render(),
        tasks,
        escalations,
        approvals: granted,
        egress,
        brokered_operations: brokered,
        budget_consumed,
        postures,
        closing_diff,
        audit_intact,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    #[test]
    fn refusals_sort_before_allowed_attempts() {
        let mut lines = [
            "rust-registry a allowed in=1 out=1".to_owned(),
            "rust-registry b REFUSED in=0 out=0".to_owned(),
            "rust-registry c allowed in=1 out=1".to_owned(),
        ];
        lines.sort_by_key(|line| !line.contains("REFUSED"));
        assert!(
            lines[0].contains("REFUSED"),
            "a refused attempt must not be buried below the successful ones"
        );
    }
}
