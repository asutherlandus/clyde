//! Brokered publication (`request_publish`, and the push that follows approval).
//!
//! An agent can ask; only a human can approve; and only the broker holds the
//! credential. The actor-facing operation creates an approval request and does
//! nothing else — it is not a path to the broker.

use std::sync::Arc;

use chrono::Utc;
use clyde_api::mcp::ToolResult;
use clyde_broker_api::GitPushRequest;
use clyde_core::approval::ApprovalSubject;
use clyde_core::audit::AuditEventKind;
use clyde_core::broker::{BrokerOpState, BrokeredKind, BrokeredOperation};
use clyde_core::decision::PolicyReason;
use clyde_core::ids::{ApprovalId, BrokerOpId};
use clyde_store::ResolvedSession;

use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};
use crate::{approvals, audit};

/// Handles a `request_publish` call.
///
/// Creates an approval request bound to the exact commit, remote, and refspec.
/// It never reaches the broker.
pub async fn request(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        commit: String,
        remote: String,
        refspec: String,
        #[serde(default)]
        reason: Option<String>,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;

    if !session.lease.authority.may_request_publish {
        return Err(DaemonError::Denied(vec![
            PolicyReason::AuthorityFlagNotHeld {
                flag: "may_request_publish".to_owned(),
            },
        ]));
    }
    clyde_core::task::validate_git_object_id(&args.commit)?;
    let branch = clyde_git::validate_branch_refspec(&args.refspec)?;

    let workspace = daemon.store.get_workspace(&session.mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;

    // Remote and branch are checked here as well as in the broker. Checking
    // early means the agent gets an actionable denial instead of a human being
    // asked to approve something that would be refused anyway.
    if !config.push.remotes.contains(&args.remote) {
        return Err(DaemonError::Denied(vec![
            PolicyReason::RemoteNotAllowlisted {
                remote: args.remote,
            },
        ]));
    }
    if let Some(pattern) = matching_pattern(&config.push.protected_branch_patterns, &branch) {
        let _ = pattern;
        return Err(DaemonError::Denied(vec![PolicyReason::ProtectedBranch {
            branch,
        }]));
    }
    if matching_pattern(&config.push.branch_patterns, &branch).is_none() {
        return Err(DaemonError::Denied(vec![
            PolicyReason::RemoteNotAllowlisted {
                remote: format!("{}:{branch}", args.remote),
            },
        ]));
    }

    // The tree is part of what is approved, so an approval cannot carry over to
    // a different commit that happens to reuse the same message.
    let tree = daemon
        .git
        .tree_of(&workspace.root, &args.commit)
        .await
        .map_err(|error| DaemonError::invalid(format!("the commit is not usable: {error}")))?;

    let operation = clyde_core::ids::new::broker_op_id()?;
    let push_request = GitPushRequest {
        operation: operation.clone(),
        mission: session.mission.id.clone(),
        lease: session.lease.id.clone(),
        // Filled in once the approval exists.
        approval: clyde_core::ids::new::approval_id()?,
        request_digest: clyde_core::Digest::of_bytes(b"placeholder"),
        workspace: workspace.root.clone(),
        remote: args.remote.clone(),
        remote_url: resolve_remote_url(&config, &args.remote),
        refspec: args.refspec.clone(),
        commit: args.commit.clone(),
        expected_tree: tree.clone(),
    };
    let digest = push_request
        .compute_digest()
        .map_err(|error| DaemonError::internal(error.to_string()))?;

    let evidence: Vec<String> = daemon
        .store
        .passing_task_evidence(&session.mission.id)?
        .into_iter()
        .map(|(task, run)| format!("{} passed ({run})", task.name()))
        .collect();

    let approval = approvals::request(
        daemon,
        &session.mission.id,
        &session.lease.id,
        &session.lease.actor,
        approvals::ApprovalContext {
            subject: ApprovalSubject::BrokeredOperation {
                summary: format!(
                    "push {} to {}:{}",
                    short(&args.commit),
                    args.remote,
                    branch
                ),
            },
            request_digest: digest.clone(),
            reason: args
                .reason
                .unwrap_or_else(|| "the agent requested publication".to_owned()),
            alternatives: Vec::new(),
            prior_failure: None,
            egress_hosts: Vec::new(),
            egress_profile: "broker".to_owned(),
            credentials: "the developer's existing git credential, inside the broker only"
                .to_owned(),
            outputs: vec!["a pushed ref".to_owned()],
            lockfile_change: None,
            inventory_diff: Vec::new(),
            task_evidence: evidence,
            caveats: vec![
                "the credential's own scope is unchanged: Clyde adds approval and audit, it does not reduce what the credential can do".to_owned(),
            ],
        },
    )?;

    let operation_record = BrokeredOperation {
        id: operation.clone(),
        mission: session.mission.id.clone(),
        lease: session.lease.id.clone(),
        approval: approval.id.clone(),
        kind: BrokeredKind::GitPush {
            remote: args.remote,
            refspec: args.refspec,
            commit: args.commit,
        },
        state: BrokerOpState::Requested,
        result_summary: None,
        requested_at: Utc::now(),
        finished_at: None,
    };
    daemon.store.insert_broker_op(operation_record)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BrokerOpRequested,
            serde_json::json!({"commit": short(&tree)}),
        )
        .mission(session.mission.id.clone())
        .lease(session.lease.id.clone())
        .approval(approval.id.clone())
        .broker_op(operation),
    );

    Ok(ToolResult::json(&serde_json::json!({
        "approval": approval.id.to_string(),
        "status": "pending human approval on the admin channel",
        "expires_at": approval.expires_at,
    })))
}

/// Executes an approved push through the broker gateway.
///
/// Called from the admin surface after a human approves. Consumption and
/// execution are ordered so there is no state where the approval is consumed but
/// the push did not run without that being recorded.
pub async fn execute(daemon: &Arc<Daemon>, approval: &ApprovalId) -> Result<String> {
    let record = daemon.store.get_approval(approval)?;
    let mission = daemon.store.get_mission(&record.request.mission)?;
    let operations = daemon.store.list_broker_ops(&mission.id)?;
    let operation = operations
        .into_iter()
        .find(|operation| &operation.approval == approval)
        .ok_or_else(|| DaemonError::not_found("no brokered operation for this approval"))?;

    if !record.authorises(&record.request.request_digest, Utc::now()) {
        return Err(DaemonError::Denied(vec![
            PolicyReason::ApprovalMissingOrStale,
        ]));
    }

    let BrokeredKind::GitPush {
        remote,
        refspec,
        commit,
    } = operation.kind.clone();
    let workspace = daemon.store.get_workspace(&mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let tree = daemon.git.tree_of(&workspace.root, &commit).await?;

    let mut push_request = GitPushRequest {
        operation: operation.id.clone(),
        mission: mission.id.clone(),
        lease: operation.lease.clone(),
        approval: approval.clone(),
        request_digest: record.request.request_digest.clone(),
        workspace: workspace.root.clone(),
        remote: remote.clone(),
        remote_url: resolve_remote_url(&config, &remote),
        refspec: refspec.clone(),
        commit: commit.clone(),
        expected_tree: tree,
    };
    // Recomputed rather than trusted: if the tree moved since the approval, the
    // digest no longer matches and the broker refuses.
    push_request.request_digest = record.request.request_digest.clone();
    if !push_request.digest_matches() {
        return Err(DaemonError::Denied(vec![
            PolicyReason::ApprovalMissingOrStale,
        ]));
    }

    daemon
        .store
        .transition_broker_op(&operation.id, BrokerOpState::Approved, None, Utc::now())?;

    // Consume first: a push that ran under a consumed approval is recoverable,
    // a push that ran under an unconsumed one is a second push waiting to happen.
    approvals::consume(daemon, approval)?;
    daemon
        .store
        .transition_broker_op(&operation.id, BrokerOpState::Executing, None, Utc::now())?;

    match daemon.broker.git_push(push_request).await {
        Ok(Ok(outcome)) => {
            daemon.store.transition_broker_op(
                &operation.id,
                BrokerOpState::Succeeded,
                Some(outcome.summary.clone()),
                Utc::now(),
            )?;
            audit::record(
                daemon.store.as_ref(),
                audit::draft(
                    AuditEventKind::BrokerOpExecuted,
                    serde_json::json!({"summary": outcome.summary}),
                )
                .mission(mission.id.clone())
                .approval(approval.clone())
                .broker_op(operation.id.clone()),
            );
            Ok(outcome.summary)
        }
        Ok(Err(reason)) => {
            record_failure(daemon, &operation.id, &mission.id, &reason.render())?;
            // A broker refusal is policy information, not a transport failure:
            // it is recorded and surfaced verbatim rather than retried.
            Err(DaemonError::invalid(reason.render()))
        }
        Err(error) => {
            record_failure(daemon, &operation.id, &mission.id, &error.to_string())?;
            Err(error)
        }
    }
}

fn record_failure(
    daemon: &Arc<Daemon>,
    operation: &BrokerOpId,
    mission: &clyde_core::ids::MissionId,
    detail: &str,
) -> Result<()> {
    daemon.store.transition_broker_op(
        operation,
        BrokerOpState::Failed,
        Some(detail.to_owned()),
        Utc::now(),
    )?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BrokerOpFailed,
            serde_json::json!({"detail": detail}),
        )
        .mission(mission.clone())
        .broker_op(operation.clone()),
    );
    Ok(())
}

/// Matches a branch against a pattern list.
///
/// Patterns are literal or end in `*`; a full glob language in a security
/// control is a source of surprises.
pub fn matching_pattern<'a>(
    patterns: &'a std::collections::BTreeSet<String>,
    branch: &str,
) -> Option<&'a String> {
    patterns
        .iter()
        .find(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => branch.starts_with(prefix),
            None => pattern.as_str() == branch,
        })
}

/// Resolves a remote name to a URL from configuration.
///
/// Never from the workspace repository's own configuration: that is
/// attacker-controlled content in this threat model.
fn resolve_remote_url(config: &clyde_policy::config::Config, remote: &str) -> String {
    let _ = config;
    // The MVP identifies a remote by name and lets the broker resolve it from
    // its own configuration, so a workspace repository cannot influence where a
    // push lands.
    remote.to_owned()
}

fn short(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
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
    use std::collections::BTreeSet;

    fn patterns(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn branch_patterns_match_literally_or_by_prefix() {
        let allowed = patterns(&["feature/*", "hotfix"]);
        assert!(matching_pattern(&allowed, "feature/x").is_some());
        assert!(matching_pattern(&allowed, "hotfix").is_some());
        assert!(matching_pattern(&allowed, "main").is_none());
        assert!(
            matching_pattern(&allowed, "hotfixes").is_none(),
            "a literal pattern must not match by prefix"
        );
    }

    #[test]
    fn protected_patterns_catch_the_usual_names() {
        let protected = clyde_policy::config::Config::defaults()
            .push
            .protected_branch_patterns;
        for branch in ["main", "master", "production", "release/1.0"] {
            assert!(
                matching_pattern(&protected, branch).is_some(),
                "{branch} must be protected by default"
            );
        }
        assert!(matching_pattern(&protected, "feature/x").is_none());
    }
}
