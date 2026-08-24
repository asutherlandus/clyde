//! Access baseline management (D18).
//!
//! A task with no confirmed baseline for its `(workspace, task, target)` key is
//! refused, with the static proposal offered. There is no implicit wide-scope
//! first run, and there is no path by which an actor confirms a baseline.

use std::collections::BTreeSet;

use chrono::Utc;
use clyde_api::admin::{AccessBaselineView, AccessPinView};
use clyde_core::audit::AuditEventKind;
use clyde_core::baseline::{
    AccessBaseline, AccessDrift, BaselineKey, BaselinePathEntry, BaselineProposal,
    CodeExecInventory,
};
use clyde_core::ids::{ActorId, MissionId, WorkspaceId};
use clyde_core::mission::MissionScope;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;
use clyde_snapshot::SnapshotRequest;

use crate::audit;
use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};

/// Computes and stores a static baseline proposal.
///
/// This is the normal path. It exists so that learn mode is rarely needed, since
/// a learn run is a wide-scope execution of exactly the code being constrained.
pub fn propose(
    daemon: &Daemon,
    workspace: &WorkspaceId,
    task: TaskType,
    target: &RepoPath,
    scope: &MissionScope,
) -> Result<BaselineProposal> {
    let record = daemon.store.get_workspace(workspace)?;
    let closure = clyde_snapshot::compute_closure(&record.root, target)?;
    let proposal = clyde_snapshot::propose_from_closure(
        workspace.clone(),
        task,
        target.clone(),
        scope,
        &closure,
    );
    daemon.store.put_baseline_proposal(proposal.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BaselineProposed,
            serde_json::json!({
                "task": task.name(),
                "target": target.as_str(),
                "grants": proposal.paths.iter().filter(|entry| !entry.is_drift_sensitive()).count(),
                "pins": proposal.paths.iter().filter(|entry| entry.is_drift_sensitive()).count(),
                "origin": format!("{:?}", proposal.origin),
            }),
        )
        .workspace(workspace.clone()),
    );
    Ok(proposal)
}

/// Confirms a stored proposal, producing a baseline that is actually in force.
///
/// Rejects a non-human confirmer. An actor must never be able to confirm the
/// change that would let its own build read further.
pub fn confirm(daemon: &Daemon, key: &BaselineKey, by: &ActorId) -> Result<AccessBaseline> {
    if !by.is_human() {
        return Err(DaemonError::invalid(
            "only a human can confirm an access baseline, and an actor can never confirm its own",
        ));
    }
    let proposal = daemon
        .store
        .get_baseline_proposal(key)?
        .ok_or_else(|| DaemonError::not_found("no proposal exists for this target"))?;
    let baseline = proposal.confirm(by.clone(), Utc::now())?;
    daemon.store.put_baseline(baseline.clone())?;
    daemon.store.delete_baseline_proposal(key)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BaselineConfirmed,
            serde_json::json!({
                "task": key.task.name(),
                "target": key.target.as_str(),
                "by": by.as_str(),
                "digest": baseline.digest().map(|digest| digest.to_string()).unwrap_or_default(),
            }),
        )
        .workspace(key.workspace.clone())
        .actor(by.clone()),
    );
    Ok(baseline)
}

/// Removes a baseline, so the next task is refused until a new one is confirmed.
pub fn reset(daemon: &Daemon, key: &BaselineKey, by: &ActorId) -> Result<bool> {
    let removed = daemon.store.delete_baseline(key)?;
    daemon.store.delete_baseline_proposal(key)?;
    if removed {
        audit::record(
            daemon.store.as_ref(),
            audit::draft(
                AuditEventKind::BaselineReset,
                serde_json::json!({"task": key.task.name(), "target": key.target.as_str()}),
            )
            .workspace(key.workspace.clone())
            .actor(by.clone()),
        );
    }
    Ok(removed)
}

/// Records that a learn-mode run was initiated.
///
/// Recorded distinctly because a learn run is a wide-scope execution of exactly
/// the code being constrained, and a reviewer should be able to find every one.
pub fn record_learn_initiated(
    daemon: &Daemon,
    key: &BaselineKey,
    by: &ActorId,
    mission: &MissionId,
) {
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::LearnModeInitiated,
            serde_json::json!({
                "task": key.task.name(),
                "target": key.target.as_str(),
                "by": by.as_str(),
                "note": "a learn run executes the code being constrained with a wide read scope",
            }),
        )
        .workspace(key.workspace.clone())
        .mission(mission.clone())
        .actor(by.clone()),
    );
}

/// Turns observed reads into an amended proposal.
pub fn propose_from_learn(
    daemon: &Daemon,
    key: &BaselineKey,
    scope: &MissionScope,
    observed: &[RepoPath],
) -> Result<BaselineProposal> {
    let base = match daemon.store.get_baseline_proposal(key)? {
        Some(existing) => existing,
        None => propose(daemon, &key.workspace, key.task, &key.target, scope)?,
    };
    let learned = clyde_snapshot::propose_from_observation(base, observed);
    daemon.store.put_baseline_proposal(learned.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BaselineProposed,
            serde_json::json!({
                "origin": "learned",
                "observed": observed.len(),
            }),
        )
        .workspace(key.workspace.clone()),
    );
    Ok(learned)
}

/// The confirmed baseline for a target, if one exists.
pub fn confirmed(daemon: &Daemon, key: &BaselineKey) -> Result<Option<AccessBaseline>> {
    Ok(daemon.store.get_baseline(key)?)
}

/// The snapshot request a baseline implies.
///
/// Enforcement cannot drift from the record because the snapshot is built from
/// the baseline rather than from the mission's scope.
pub fn snapshot_request(
    daemon: &Daemon,
    mission: &MissionId,
    baseline: &AccessBaseline,
    exclusions: BTreeSet<String>,
) -> Result<SnapshotRequest> {
    let workspace = daemon.store.get_workspace(&baseline.workspace)?;
    Ok(SnapshotRequest::from_baseline(
        baseline.workspace.clone(),
        mission.clone(),
        workspace.root,
        baseline,
        exclusions,
    ))
}

/// Checks a bundle's inventory against the pinned baseline, **before** the
/// sandbox starts.
///
/// Pre-execution ordering is the point: a newly arrived build script is caught
/// before it runs, not after.
pub fn inventory_drift(baseline: &AccessBaseline, current: &CodeExecInventory) -> Vec<AccessDrift> {
    clyde_policy::access::inventory_drift(&baseline.inventory, current)
}

/// Records detected drift, by class.
pub fn record_drift(daemon: &Daemon, mission: &MissionId, drift: &[AccessDrift]) {
    for item in drift {
        audit::record(
            daemon.store.as_ref(),
            audit::draft(
                AuditEventKind::AccessDriftDetected {
                    drift: item.clone(),
                },
                serde_json::json!({
                    "rendered": item.render(),
                    "dependency_drift": item.is_dependency_drift(),
                    "tampering_signal": item.is_tampering_signal(),
                }),
            )
            .mission(mission.clone()),
        );
    }
}

/// Amends a baseline's inventory after a human confirms a dependency change.
pub fn amend_inventory(
    daemon: &Daemon,
    key: &BaselineKey,
    inventory: CodeExecInventory,
    by: &ActorId,
) -> Result<AccessBaseline> {
    if !by.is_human() {
        return Err(DaemonError::invalid(
            "only a human can confirm a dependency inventory change",
        ));
    }
    let mut baseline = daemon
        .store
        .get_baseline(key)?
        .ok_or_else(|| DaemonError::not_found("no confirmed baseline exists for this target"))?;
    baseline.inventory = inventory.canonicalised();
    baseline.origin = clyde_core::baseline::BaselineOrigin::Amended;
    baseline.confirmed_by = by.clone();
    baseline.confirmed_at = Utc::now();
    daemon.store.put_baseline(baseline.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::BaselineAmended,
            serde_json::json!({
                "task": key.task.name(),
                "target": key.target.as_str(),
                "entries": baseline.inventory.entries.len(),
            }),
        )
        .workspace(key.workspace.clone())
        .actor(by.clone()),
    );
    Ok(baseline)
}

/// Renders a baseline or proposal for review.
pub fn render(
    key: &BaselineKey,
    baseline: Option<&AccessBaseline>,
    proposal: Option<&BaselineProposal>,
) -> AccessBaselineView {
    let (paths, inventory, origin, confirmed_by, rationale, digest) = match (baseline, proposal) {
        (Some(baseline), _) => (
            &baseline.paths,
            &baseline.inventory,
            format!("{:?}", baseline.origin),
            Some(baseline.confirmed_by.to_string()),
            Vec::new(),
            baseline.digest().map(|digest| digest.to_string()).ok(),
        ),
        (None, Some(proposal)) => (
            &proposal.paths,
            &proposal.inventory,
            format!("{:?}", proposal.origin),
            None,
            proposal.rationale.clone(),
            None,
        ),
        (None, None) => {
            return AccessBaselineView {
                workspace: key.workspace.to_string(),
                task: key.task.name().to_owned(),
                target: key.target.to_string(),
                origin: "none".to_owned(),
                confirmed: false,
                confirmed_by: None,
                grants: Vec::new(),
                pins: Vec::new(),
                inventory_entries: Vec::new(),
                rationale: Vec::new(),
                digest: None,
            };
        }
    };

    AccessBaselineView {
        workspace: key.workspace.to_string(),
        task: key.task.name().to_owned(),
        target: key.target.to_string(),
        origin,
        confirmed: baseline.is_some(),
        confirmed_by,
        grants: paths
            .iter()
            .filter_map(|entry| match entry {
                BaselinePathEntry::SubtreeGrant { path, source } => {
                    Some(format!("{path} ({source:?})"))
                }
                _ => None,
            })
            .collect(),
        pins: paths
            .iter()
            .filter_map(|entry| match entry {
                BaselinePathEntry::FilePin { path, reason, .. }
                | BaselinePathEntry::SubtreePin { path, reason } => Some(AccessPinView {
                    path: path.to_string(),
                    reason: reason.clone(),
                }),
                BaselinePathEntry::SubtreeGrant { .. } => None,
            })
            .collect(),
        inventory_entries: inventory
            .entries
            .iter()
            .map(|entry| format!("{} {} ({})", entry.crate_name, entry.version, entry.kind))
            .collect(),
        rationale,
        digest,
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
    use clyde_core::baseline::{BaselineOrigin, CodeExecKind, GrantSource};
    use clyde_core::ids;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn baseline(entries: Vec<clyde_core::baseline::CodeExecEntry>) -> AccessBaseline {
        AccessBaseline {
            workspace: ids::new::workspace_id().unwrap(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths: vec![BaselinePathEntry::SubtreeGrant {
                path: path("crates/core"),
                source: GrantSource::MissionEditScope,
            }],
            inventory: CodeExecInventory {
                lockfile_digest: None,
                entries,
            }
            .canonicalised(),
            origin: BaselineOrigin::StaticClosure,
            confirmed_by: ActorId::parse("human:andrew").unwrap(),
            confirmed_at: Utc::now(),
        }
    }

    fn entry(name: &str, version: &str, content: &str) -> clyde_core::baseline::CodeExecEntry {
        clyde_core::baseline::CodeExecEntry {
            crate_name: name.to_owned(),
            version: version.to_owned(),
            kind: CodeExecKind::BuildScript,
            source_blake3: clyde_core::Digest::of_bytes(content.as_bytes()),
        }
    }

    #[test]
    fn a_new_build_script_is_drift_against_the_pinned_inventory() {
        let baseline = baseline(vec![entry("ring", "0.17.8", "a")]);
        let current = CodeExecInventory {
            lockfile_digest: None,
            entries: vec![entry("ring", "0.17.8", "a"), entry("new", "1.0.0", "b")],
        }
        .canonicalised();
        let drift = inventory_drift(&baseline, &current);
        assert_eq!(drift.len(), 1);
        assert!(drift[0].is_dependency_drift());
    }

    #[test]
    fn rendering_separates_grants_from_pins() {
        let mut baseline = baseline(vec![entry("ring", "0.17.8", "a")]);
        baseline.paths.push(BaselinePathEntry::FilePin {
            path: path("docs/schema.sql"),
            blake3: None,
            reason: "include_str! target".to_owned(),
        });
        let view = render(&baseline.key(), Some(&baseline), None);
        assert_eq!(view.grants.len(), 1);
        assert_eq!(view.pins.len(), 1);
        assert_eq!(view.pins[0].reason, "include_str! target");
        assert!(view.confirmed);
        assert_eq!(view.inventory_entries.len(), 1);
        assert!(view.digest.is_some());
    }

    #[test]
    fn rendering_a_missing_baseline_says_so_rather_than_failing() {
        let key = BaselineKey {
            workspace: ids::new::workspace_id().unwrap(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
        };
        let view = render(&key, None, None);
        assert!(!view.confirmed);
        assert_eq!(view.origin, "none");
        assert!(view.grants.is_empty());
    }

    #[test]
    fn a_proposal_renders_with_its_rationale_and_is_not_confirmed() {
        let proposal = BaselineProposal {
            workspace: ids::new::workspace_id().unwrap(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths: vec![BaselinePathEntry::SubtreePin {
                path: path("vendor/thing"),
                reason: "outside the approved scope".to_owned(),
            }],
            inventory: CodeExecInventory::default(),
            origin: BaselineOrigin::StaticClosure,
            proposed_at: Utc::now(),
            rationale: vec!["vendor/thing is OUTSIDE the approved scope".to_owned()],
        };
        let key = BaselineKey {
            workspace: proposal.workspace.clone(),
            task: proposal.task,
            target: proposal.target.clone(),
        };
        let view = render(&key, None, Some(&proposal));
        assert!(!view.confirmed);
        assert_eq!(view.pins.len(), 1);
        assert!(view.rationale[0].contains("OUTSIDE"));
    }
}
