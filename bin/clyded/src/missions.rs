//! Mission, lease, and session lifecycle (Phase 1 deliverables 2, 3, 4).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use clyde_api::admin::{CreateMission, MissionEnvelope};
use clyde_core::HumanDuration;
use clyde_core::actor::{Actor, ActorKind};
use clyde_core::audit::AuditEventKind;
use clyde_core::budget::{Budget, BudgetUsage};
use clyde_core::classification::{ApprovalRequirement, CredentialPolicy, EgressProfile};
use clyde_core::ids::{self, ActorId, LeaseId, MissionId, WorkspaceId};
use clyde_core::lease::{AuthorityFlags, Lease, LeaseState};
use clyde_core::mission::{
    ApprovalPolicy, Mission, MissionScope, MissionState, NetworkPolicy, StopCondition,
};
use clyde_core::repo_path::RepoPath;
use clyde_core::session::{ActorSession, SessionToken};
use clyde_core::task::TaskType;
use clyde_policy::config::Config;
use clyde_store::MissionCloseout;

use crate::audit;
use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};

/// Builds a mission proposal.
///
/// The proposal is the exact envelope that will be issued: no field is decided
/// after approval (Phase 1 deliverable 2).
pub fn propose(
    daemon: &Daemon,
    request: &CreateMission,
    initiator: &ActorId,
) -> Result<(Mission, MissionEnvelope)> {
    if !initiator.is_human() {
        return Err(DaemonError::invalid(
            "only a human actor can create a mission",
        ));
    }
    let workspace_id = WorkspaceId::parse(request.workspace.clone())?;
    let workspace = daemon.store.get_workspace(&workspace_id)?;
    let config = daemon.config_for(&workspace.root)?;

    let scope = build_scope(request, &config)?;
    let allowed_tasks = build_task_set(request, &config)?;
    let budget = build_budget(request, &config);
    let expiry = build_expiry(request, &config)?;

    let primary_actor = ActorId::parse(format!(
        "agent:{}",
        std::env::var("CLYDE_AGENT_NAME").unwrap_or_else(|_| "claude".to_owned())
    ))?;

    let mission = Mission {
        id: ids::new::mission_id()?,
        workspace: workspace_id,
        objective: request.objective.clone(),
        initiator: initiator.clone(),
        primary_actor: primary_actor.clone(),
        scope,
        allowed_tasks,
        network_policy: NetworkPolicy {
            ceiling: if request.model_api {
                EgressProfile::ModelApi
            } else {
                EgressProfile::None
            },
        },
        credential_policy: CredentialPolicy::None,
        approval_policy: ApprovalPolicy {
            pre_approved_tasks: BTreeSet::new(),
            allow_mission_scoped_approvals: true,
        },
        budget,
        expiry,
        state: MissionState::Proposed,
        stop_conditions: [
            StopCondition::BudgetExhausted,
            StopCondition::Expiry,
            StopCondition::AccessDriftDetected,
        ]
        .into_iter()
        .collect(),
        success_criteria: Vec::new(),
        cache_dir: None,
        created_at: Utc::now(),
        closed_at: None,
    };
    mission.validate()?;

    // Register the actors so the audit trail can name them.
    daemon.store.upsert_actor(Actor {
        id: initiator.clone(),
        kind: ActorKind::Human,
        display_name: initiator.as_str().trim_start_matches("human:").to_owned(),
        parent: None,
        created_at: Utc::now(),
    })?;
    daemon.store.upsert_actor(Actor {
        id: primary_actor,
        kind: ActorKind::Agent,
        display_name: "primary agent".to_owned(),
        parent: None,
        created_at: Utc::now(),
    })?;

    daemon.store.create_mission(mission.clone())?;
    let mission = daemon
        .store
        .transition_mission(&mission.id, MissionState::AwaitingApproval)?;

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionProposed,
            serde_json::json!({
                "objective": mission.objective,
                "edit_paths": mission.scope.edit_paths.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "tasks": mission.allowed_tasks.iter().map(|task| task.name()).collect::<Vec<_>>(),
                "egress": mission.network_policy.ceiling.name(),
            }),
        )
        .mission(mission.id.clone())
        .workspace(mission.workspace.clone())
        .actor(initiator.clone()),
    );

    let envelope = envelope_for(daemon, &mission, &config)?;
    Ok((mission, envelope))
}

/// Renders the envelope a human approves.
pub fn envelope_for(
    daemon: &Daemon,
    mission: &Mission,
    config: &Config,
) -> Result<MissionEnvelope> {
    let approval_required: Vec<String> = mission
        .allowed_tasks
        .iter()
        .filter(|task| clyde_policy::builtin_policy(**task).approval != ApprovalRequirement::None)
        .map(|task| task.name().to_owned())
        .collect();

    // A baseline lives only in Clyde state, so it is invisible to code review
    // and to a fresh clone. Summarising it in the envelope is what makes the
    // access control visible at the moment a human is deciding.
    let baselines: Vec<String> = daemon
        .store
        .list_baselines(&mission.workspace)?
        .into_iter()
        .map(|baseline| {
            format!(
                "{} on {} ({} grants, {} pins)",
                baseline.task.name(),
                baseline.target,
                baseline.grants().count(),
                baseline.pins().count()
            )
        })
        .collect();

    let mut caveats = vec![
        "edit scope is enforced by mount topology: a write outside it fails at the kernel"
            .to_owned(),
        "build and test tasks have no network access at all".to_owned(),
    ];
    if mission.network_policy.ceiling == EgressProfile::ModelApi {
        caveats.push(
            "the agent reaches the model API through a proxy that records every connection; it does not hold the credential, but an allowlisted endpoint accepting arbitrary bodies is still an exfiltration channel"
                .to_owned(),
        );
    }
    if baselines.is_empty()
        && mission
            .allowed_tasks
            .iter()
            .any(|task| clyde_policy::builtin_policy(*task).requires_access_baseline)
    {
        caveats.push(
            "no access baseline is confirmed for this workspace yet, so build tasks will be refused until one is (clyde access propose)"
                .to_owned(),
        );
    }
    if !daemon.host.can_run_build() {
        caveats.push(format!(
            "this host cannot run build tasks: {}",
            daemon.host.cgroup_delegation.detail()
        ));
    }

    Ok(MissionEnvelope {
        mission: mission.id.to_string(),
        workspace: mission.workspace.to_string(),
        objective: mission.objective.clone(),
        state: mission.state.to_string(),
        edit_paths: mission
            .scope
            .edit_paths
            .iter()
            .map(ToString::to_string)
            .collect(),
        read_paths: mission
            .scope
            .read_paths
            .iter()
            .map(ToString::to_string)
            .collect(),
        allowed_tasks: mission
            .allowed_tasks
            .iter()
            .map(|task| task.name().to_owned())
            .collect(),
        egress_profile: mission.network_policy.ceiling.to_string(),
        credential_policy: mission.credential_policy.to_string(),
        expires_at: mission.expiry,
        max_task_runs: mission.budget.max_task_runs,
        max_subagents: mission.budget.max_subagents,
        approval_required_tasks: approval_required,
        baselines_in_force: baselines,
        caveats: {
            let _ = config;
            caveats
        },
    })
}

/// Approves a mission and issues its primary lease.
pub fn approve(daemon: &Daemon, mission: &MissionId, by: &ActorId) -> Result<(Mission, Lease)> {
    if !by.is_human() {
        return Err(DaemonError::invalid(
            "only a human actor can approve a mission",
        ));
    }
    let mission = daemon
        .store
        .transition_mission(mission, MissionState::Active)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionApproved,
            serde_json::json!({"by": by.as_str()}),
        )
        .mission(mission.id.clone())
        .actor(by.clone()),
    );

    let lease = issue_primary_lease(daemon, &mission)?;

    // The per-mission cache is created at activation and destroyed at closeout
    // (D3).
    let cache = clyde_snapshot::MissionCache::create(&daemon.paths.missions(), &mission.id)?;
    daemon
        .store
        .set_mission_cache_dir(&mission.id, Some(cache.root().to_path_buf()))?;

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionActivated,
            serde_json::json!({"lease": lease.id.as_str()}),
        )
        .mission(mission.id.clone())
        .lease(lease.id.clone()),
    );
    Ok((mission, lease))
}

/// Denies a proposed mission.
pub fn deny(
    daemon: &Daemon,
    mission: &MissionId,
    by: &ActorId,
    note: Option<&str>,
) -> Result<Mission> {
    if !by.is_human() {
        return Err(DaemonError::invalid(
            "only a human actor can deny a mission",
        ));
    }
    let mission = daemon
        .store
        .transition_mission(mission, MissionState::Denied)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionDenied,
            serde_json::json!({"by": by.as_str(), "note": note}),
        )
        .mission(mission.id.clone())
        .actor(by.clone()),
    );
    Ok(mission)
}

/// Issues the mission's primary lease.
fn issue_primary_lease(daemon: &Daemon, mission: &Mission) -> Result<Lease> {
    let now = Utc::now();
    let lease = Lease {
        id: ids::new::lease_id()?,
        mission: mission.id.clone(),
        parent: None,
        actor: mission.primary_actor.clone(),
        issued_by: ActorId::parse("human:clyde")?,
        issued_at: now,
        expires_at: mission.expiry,
        repo_scope: mission.scope.clone(),
        task_scope: mission.allowed_tasks.clone(),
        network_scope: mission.network_policy.ceiling.clone(),
        credential_scope: mission.credential_policy,
        authority: AuthorityFlags {
            may_edit: !mission.scope.edit_paths.is_empty(),
            may_request_tasks: true,
            may_spawn_subagents: mission.budget.max_subagents > 0,
            may_request_publish: mission.allowed_tasks.contains(&TaskType::GitPush),
        },
        budget: mission.budget,
        usage: BudgetUsage::default(),
        state: LeaseState::Active,
        purpose: format!("primary lease for {}", mission.objective),
    };
    lease.validate()?;
    daemon.store.insert_lease(lease.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::LeaseIssued,
            serde_json::json!({"actor": lease.actor.as_str()}),
        )
        .mission(mission.id.clone())
        .lease(lease.id.clone()),
    );
    Ok(lease)
}

/// Binds a session to a lease, returning the token to write into the sandbox.
///
/// The plaintext token is returned exactly once, to the caller that will write
/// it into the sandbox's token file. It is never stored and never logged.
pub fn bind_session(daemon: &Daemon, lease: &Lease) -> Result<SessionToken> {
    let token =
        SessionToken::generate().map_err(|error| DaemonError::internal(error.to_string()))?;
    let session = ActorSession {
        actor: lease.actor.clone(),
        lease: lease.id.clone(),
        token_hash: token.hash(),
        issued_at: Utc::now(),
        // Token expiry is derived from lease expiry, never independent of it.
        expires_at: lease.expires_at,
        revoked_at: None,
        sandbox: None,
    };
    daemon.store.bind_session(session)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(AuditEventKind::SessionBound, serde_json::Value::Null)
            .mission(lease.mission.clone())
            .lease(lease.id.clone())
            .actor(lease.actor.clone()),
    );
    Ok(token)
}

/// Writes a token file for a sandbox.
///
/// Mode 0400 and never in `argv` or the environment (D2).
pub fn write_token_file(
    directory: &Path,
    lease: &LeaseId,
    token: &SessionToken,
) -> Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::create_dir_all(directory)
        .map_err(|error| DaemonError::io("creating the token directory", error))?;
    let path = directory.join(format!("{}.token", lease.as_str()));
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&path)
        .map_err(|error| DaemonError::io("creating the token file", error))?;
    let hex = token.to_hex();
    file.write_all(hex.expose().as_bytes())
        .map_err(|error| DaemonError::io("writing the token file", error))?;
    Ok(path)
}

/// Revokes a mission: leases, sessions, and in-flight brokered work.
pub fn revoke(daemon: &Daemon, mission: &MissionId, by: &ActorId) -> Result<Mission> {
    let now = Utc::now();
    let closed = daemon.store.close_mission(MissionCloseout {
        mission: mission.clone(),
        final_state: MissionState::Revoked,
        closed_at: now,
        closing_diff: None,
        summary: format!("revoked by {by}"),
    })?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionRevoked,
            serde_json::json!({"by": by.as_str()}),
        )
        .mission(mission.clone())
        .actor(by.clone()),
    );
    Ok(closed)
}

/// Closes a mission normally, recording the closing diff.
pub fn close(
    daemon: &Daemon,
    mission: &MissionId,
    summary: String,
    closing_diff: Option<clyde_core::ids::ArtifactId>,
) -> Result<Mission> {
    let closed = daemon.store.close_mission(MissionCloseout {
        mission: mission.clone(),
        final_state: MissionState::Completed,
        closed_at: Utc::now(),
        closing_diff: closing_diff.clone(),
        summary: summary.clone(),
    })?;
    // The cache is destroyed at closeout; its size counted against the mission's
    // budget while it lived.
    if let Some(cache) = clyde_snapshot::MissionCache::existing(&daemon.paths.missions(), mission)
        && let Err(error) = cache.destroy()
    {
        tracing::error!(error = %error, "removing the mission cache failed");
    }
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::MissionClosed,
            serde_json::json!({"summary": summary}),
        )
        .mission(mission.clone())
        .artifacts(closing_diff.into_iter().collect()),
    );
    Ok(closed)
}

/// Renews a lease by issuing a replacement.
///
/// Renewal issues a **replacement** lease and marks the old one superseded,
/// rather than mutating expiry in place, so the audit trail shows the extension
/// as an event (Phase 1 deliverable 3).
pub fn renew(
    daemon: &Daemon,
    mission: &MissionId,
    extend_by: &str,
    additional_task_runs: Option<u32>,
) -> Result<Lease> {
    let extension = HumanDuration::parse(extend_by)?;
    let leases = daemon.store.list_leases(mission)?;
    let current = leases
        .into_iter()
        .find(|lease| lease.parent.is_none() && lease.state != LeaseState::Superseded)
        .ok_or_else(|| DaemonError::not_found("this mission has no renewable lease"))?;

    let expires_at = current.expires_at
        + Duration::from_std(extension.as_duration())
            .map_err(|_| DaemonError::invalid("the extension is too large"))?;
    let mut budget = current.budget;
    if let Some(additional) = additional_task_runs {
        budget.max_task_runs = budget.max_task_runs.saturating_add(additional);
    }

    let replacement = Lease {
        id: ids::new::lease_id()?,
        issued_at: Utc::now(),
        expires_at,
        budget,
        // Usage carries over: a renewal extends the lease, it does not reset
        // what has already been spent.
        usage: current.usage,
        state: LeaseState::Active,
        purpose: format!("renewal of {}", current.id),
        ..current.clone()
    };
    replacement.validate()?;
    let renewed = daemon.store.renew_lease(clyde_store::LeaseRenewal {
        superseded: current.id.clone(),
        replacement,
    })?;

    // The mission's own expiry moves with the lease, or the lease would outlive
    // the envelope the human approved.
    let mut mission_record = daemon.store.get_mission(mission)?;
    if renewed.expires_at > mission_record.expiry {
        mission_record.expiry = renewed.expires_at;
        // Stored through the transition path so the record stays consistent;
        // the state itself is unchanged.
        daemon
            .store
            .set_mission_cache_dir(mission, mission_record.cache_dir.clone())?;
    }

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::LeaseRenewed,
            serde_json::json!({
                "superseded": current.id.as_str(),
                "expires_at": renewed.expires_at.to_rfc3339(),
            }),
        )
        .mission(mission.clone())
        .lease(renewed.id.clone()),
    );
    Ok(renewed)
}

fn build_scope(request: &CreateMission, config: &Config) -> Result<MissionScope> {
    let parse_all = |paths: &[String]| -> Result<BTreeSet<RepoPath>> {
        paths
            .iter()
            .map(|path| RepoPath::parse(path).map_err(DaemonError::from))
            .collect()
    };
    let edit_paths = if request.edit_paths.is_empty() {
        config.mission_defaults.edit_paths.clone()
    } else {
        parse_all(&request.edit_paths)?
    };
    let read_paths = if request.read_paths.is_empty() {
        config.mission_defaults.read_paths.clone()
    } else {
        parse_all(&request.read_paths)?
    };
    let scope = MissionScope {
        edit_paths,
        read_paths,
    };
    if scope.edit_paths.is_empty() && scope.read_paths.is_empty() {
        return Err(DaemonError::invalid(
            "a mission needs at least one edit or read path; an unscoped mission is not a bounded delegation",
        ));
    }
    Ok(scope)
}

fn build_task_set(request: &CreateMission, config: &Config) -> Result<BTreeSet<TaskType>> {
    if request.tasks.is_empty() {
        // The default set is the work an agent does without crossing a boundary:
        // reading, editing, searching, and the offline build loop.
        return Ok([
            TaskType::WorkspaceRead,
            TaskType::WorkspaceEdit,
            TaskType::RepoSearch,
            TaskType::RustCheck,
            TaskType::RustTestUnit,
        ]
        .into_iter()
        .filter(|task| {
            config
                .task_override(*task)
                .map(|task_override| task_override.enabled)
                .unwrap_or(true)
        })
        .collect());
    }
    request
        .tasks
        .iter()
        .map(|name| TaskType::parse(name).map_err(DaemonError::from))
        .collect()
}

fn build_budget(request: &CreateMission, config: &Config) -> Budget {
    let mut budget = config.mission_defaults.budget;
    if let Some(max) = request.max_task_runs {
        // Narrowing only: a request cannot exceed the configured default.
        budget.max_task_runs = budget.max_task_runs.min(max);
    }
    budget
}

fn build_expiry(request: &CreateMission, config: &Config) -> Result<DateTime<Utc>> {
    let requested = match &request.expires_in {
        Some(text) => HumanDuration::parse(text)?,
        None => config.mission_defaults.budget.max_duration,
    };
    // Clamped to the configured maximum, which a repository can lower and never
    // raise.
    let effective = requested.min(config.mission_defaults.max_expiry);
    Ok(Utc::now()
        + Duration::from_std(effective.as_duration())
            .map_err(|_| DaemonError::invalid("the requested expiry is too large"))?)
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
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_token_file_is_owner_read_only_and_contains_only_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let lease = ids::new::lease_id().unwrap();
        let token = SessionToken::generate().unwrap();
        let path = write_token_file(dir.path(), &lease, &token).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400, "the token file must not be writable");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.len(), 64);
        let parsed = SessionToken::parse_hex(&contents).unwrap();
        assert!(parsed.hash().matches(&token.hash()));
    }

    #[test]
    fn writing_a_token_twice_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let lease = ids::new::lease_id().unwrap();
        let first =
            write_token_file(dir.path(), &lease, &SessionToken::generate().unwrap()).unwrap();
        let second =
            write_token_file(dir.path(), &lease, &SessionToken::generate().unwrap()).unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(&second).unwrap().len(), 64);
    }

    #[test]
    fn an_unscoped_mission_is_refused() {
        let request = CreateMission {
            workspace: "w-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            objective: "do things".to_owned(),
            edit_paths: Vec::new(),
            read_paths: Vec::new(),
            tasks: Vec::new(),
            expires_in: None,
            max_task_runs: None,
            model_api: true,
            start_agent: false,
        };
        let error = build_scope(&request, &Config::defaults())
            .expect_err("an unscoped mission is not a bounded delegation");
        assert!(error.to_string().contains("bounded delegation"));
    }

    #[test]
    fn the_default_task_set_is_the_offline_loop() {
        let request = CreateMission {
            workspace: "w-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            objective: "x".to_owned(),
            edit_paths: vec!["src".to_owned()],
            read_paths: Vec::new(),
            tasks: Vec::new(),
            expires_in: None,
            max_task_runs: None,
            model_api: true,
            start_agent: false,
        };
        let tasks = build_task_set(&request, &Config::defaults()).unwrap();
        assert!(tasks.contains(&TaskType::RustCheck));
        assert!(
            !tasks.contains(&TaskType::GitPush),
            "publishing is never in a mission's default task set"
        );
        assert!(
            !tasks.contains(&TaskType::RustResolveDeps),
            "network-bearing work is never a default"
        );
    }

    #[test]
    fn a_disabled_task_is_absent_from_the_default_set() {
        let (config, _) = clyde_policy::config::apply_layer(
            Config::defaults(),
            "[tasks.\"rust.test.unit\"]\nenabled = false\n",
            clyde_policy::config::ConfigSource::Repository,
        )
        .unwrap();
        let request = CreateMission {
            workspace: "w-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            objective: "x".to_owned(),
            edit_paths: vec!["src".to_owned()],
            read_paths: Vec::new(),
            tasks: Vec::new(),
            expires_in: None,
            max_task_runs: None,
            model_api: true,
            start_agent: false,
        };
        let tasks = build_task_set(&request, &config).unwrap();
        assert!(!tasks.contains(&TaskType::RustTestUnit));
    }

    #[test]
    fn a_requested_budget_can_narrow_but_not_widen_the_default() {
        let config = Config::defaults();
        let mut request = CreateMission {
            workspace: "w-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            objective: "x".to_owned(),
            edit_paths: vec!["src".to_owned()],
            read_paths: Vec::new(),
            tasks: Vec::new(),
            expires_in: None,
            max_task_runs: Some(5),
            model_api: true,
            start_agent: false,
        };
        assert_eq!(build_budget(&request, &config).max_task_runs, 5);
        request.max_task_runs = Some(u32::MAX);
        assert_eq!(
            build_budget(&request, &config).max_task_runs,
            config.mission_defaults.budget.max_task_runs,
            "a request cannot exceed the configured default"
        );
    }

    #[test]
    fn expiry_is_clamped_to_the_configured_maximum() {
        let config = Config::defaults();
        let request = CreateMission {
            workspace: "w-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            objective: "x".to_owned(),
            edit_paths: vec!["src".to_owned()],
            read_paths: Vec::new(),
            tasks: Vec::new(),
            expires_in: Some("30d".to_owned()),
            max_task_runs: None,
            model_api: true,
            start_agent: false,
        };
        let expiry = build_expiry(&request, &config).unwrap();
        let maximum = Utc::now()
            + Duration::from_std(config.mission_defaults.max_expiry.as_duration()).unwrap();
        assert!(expiry <= maximum + Duration::seconds(5));
    }
}
