//! The operator API, on the admin socket.
//!
//! Every approval decision happens here and nowhere else. The socket is mode
//! `0600`, `SO_PEERCRED`-checked, and never mounted into a sandbox, which is what
//! makes agent self-approval structurally impossible rather than
//! policy-prohibited (D2).

use std::sync::Arc;

use chrono::Utc;
use clyde_api::admin::{self, methods};
use clyde_api::codec::{CodecError, RequestReader, ResponseWriter};
use clyde_api::jsonrpc::{Error as RpcError, Request, Response};
use clyde_api::views::TaskRunView;
use clyde_core::actor::Principal;
use clyde_core::approval::Decision;
use clyde_core::audit::AuditEventKind;
use clyde_core::baseline::BaselineKey;
use clyde_core::ids::{ActorId, ApprovalId, MissionId, TaskRunId, WorkspaceId};
use clyde_core::lease::{Lease, LeaseState};
use clyde_core::mission::{Mission, MissionState};
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;
use clyde_core::workspace::{VcsKind, Workspace};
use clyde_store::AuditFilter;
use tokio::net::UnixStream;

use crate::actor_api::default_options;
use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};
use crate::tasks::TaskContext;
use crate::{access, agent, approvals, audit, missions, publish, review, server, tasks};

/// Serves one operator connection.
pub async fn serve(daemon: Arc<Daemon>, stream: UnixStream) {
    // The socket mode already excludes other users; the peer check is the second
    // one, because a mode is a property of a path and a path can be replaced.
    let peer = match server::peer_of(&stream) {
        Ok(peer) if server::is_admin_peer(peer) => peer,
        _ => {
            tracing::warn!("refused an admin connection from another user");
            return;
        }
    };
    let operator = operator_identity(peer);
    // The principal comes from the peer credential the kernel reported, not from
    // re-parsing the identity string, so there is no path by which a caller
    // influences how its own request is attributed (D25).
    let principal = Principal::Operator { uid: peer.uid };

    let (read_half, write_half) = stream.into_split();
    let mut reader = RequestReader::new(read_half);
    let mut writer = ResponseWriter::new(write_half);

    loop {
        let request = match reader.next().await {
            Ok(request) => request,
            Err(CodecError::Closed) => break,
            Err(error) => {
                if writer
                    .send(&Response::failure(None, error.to_rpc_error()))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };
        if let Err(error) = request.validate() {
            let _ = writer
                .send(&Response::failure(request.id.clone(), error))
                .await;
            continue;
        }
        if request.is_notification() {
            continue;
        }
        let id = request.id.clone();
        let response = match dispatch(&daemon, &operator, &principal, &request).await {
            Ok(value) => Response::success(id, value),
            Err(error) => Response::failure(id, error.to_rpc(&request.method)),
        };
        if writer.send(&response).await.is_err() {
            break;
        }
    }
}

/// The operator's actor identity.
///
/// Derived from the peer's uid rather than from anything the caller supplies, so
/// a decision cannot be attributed to someone else.
fn operator_identity(peer: server::Peer) -> ActorId {
    ActorId::parse(format!("human:uid-{}", peer.uid)).unwrap_or_else(|_| fallback_operator())
}

fn fallback_operator() -> ActorId {
    match ActorId::parse("human:operator") {
        Ok(actor) => actor,
        Err(_) => fallback_operator(),
    }
}

async fn dispatch(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    principal: &Principal,
    request: &Request,
) -> Result<serde_json::Value> {
    match request.method.as_str() {
        methods::WORKSPACE_REGISTER => register_workspace(daemon, request).await,
        methods::WORKSPACE_LIST => list_workspaces(daemon),
        methods::MISSION_CREATE => create_mission(daemon, operator, request).await,
        methods::MISSION_STATUS => mission_status(daemon, request),
        methods::MISSION_LIST => list_missions(daemon),
        methods::MISSION_APPROVE => approve_mission(daemon, operator, request).await,
        methods::MISSION_DENY => deny_mission(daemon, operator, request),
        methods::MISSION_REVOKE => revoke_mission(daemon, operator, request).await,
        methods::MISSION_RENEW => renew_mission(daemon, request),
        methods::MISSION_CLOSE => close_mission(daemon, request).await,
        methods::MISSION_REVIEW => review_mission(daemon, request).await,
        methods::APPROVALS_LIST => list_approvals(daemon),
        methods::APPROVALS_DECIDE => decide_approval(daemon, operator, request).await,
        methods::AUDIT_SHOW => show_audit(daemon, request),
        methods::AUDIT_VERIFY => verify_audit(daemon),
        methods::ACCESS_SHOW => show_access(daemon, request),
        methods::ACCESS_PROPOSE => propose_access(daemon, request),
        methods::ACCESS_LEARN => learn_access(daemon, operator, request),
        methods::ACCESS_REVIEW => show_access(daemon, request),
        methods::ACCESS_CONFIRM => confirm_access(daemon, operator, request),
        methods::ACCESS_RESET => reset_access(daemon, operator, request),
        methods::DEPS_IMPORT => import_bundle(daemon, request),
        methods::DEPS_LIST => list_bundles(daemon),
        methods::DEPS_CONFIRM_INVENTORY => confirm_inventory(daemon, operator, request),
        methods::TASK_RUN => run_task(daemon, principal, request).await,
        methods::TASK_LIST => list_tasks(daemon, request),
        methods::TASK_STATUS => task_status(daemon, request),
        methods::TASK_LOGS => task_logs(daemon, request),
        methods::DOCTOR => doctor(daemon).await,
        other => Err(DaemonError::not_found(format!(
            "{other} is not an operator method"
        ))),
    }
}

async fn register_workspace(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::RegisterWorkspace = request.parse_params().map_err(rpc)?;
    let root = std::path::PathBuf::from(&params.root)
        .canonicalize()
        .map_err(|error| DaemonError::io("resolving the workspace root", error))?;
    if let Some(existing) = daemon.store.find_workspace_by_root(&root)? {
        return Ok(serde_json::json!({"workspace": existing.id.to_string(), "existing": true}));
    }

    let vcs = if daemon.git.is_repository(&root).await {
        VcsKind::Git {
            default_remote: None,
            default_branch: daemon.git.current_branch(&root).await.ok().flatten(),
        }
    } else {
        VcsKind::None
    };
    let workspace = Workspace {
        id: clyde_core::ids::new::workspace_id()?,
        root: root.clone(),
        vcs,
        registered_at: Utc::now(),
        policy_digest: None,
    };
    daemon.store.register_workspace(workspace.clone())?;
    // Loading the repository configuration now surfaces a widening attempt at
    // registration rather than at the first mission.
    let _ = daemon.config_for(&root)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::WorkspaceRegistered,
            serde_json::json!({"root": root.display().to_string()}),
        )
        .workspace(workspace.id.clone()),
    );
    Ok(serde_json::json!({"workspace": workspace.id.to_string(), "existing": false}))
}

fn list_workspaces(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    let workspaces: Vec<admin::WorkspaceSummary> = daemon
        .store
        .list_workspaces()?
        .into_iter()
        .map(|workspace| {
            let active = daemon
                .store
                .active_mission(&workspace.id)
                .ok()
                .flatten()
                .map(|mission| mission.id.to_string());
            admin::WorkspaceSummary {
                workspace: workspace.id.to_string(),
                root: workspace.root.display().to_string(),
                vcs: match workspace.vcs {
                    VcsKind::Git { .. } => "git".to_owned(),
                    VcsKind::None => "none".to_owned(),
                },
                active_mission: active,
            }
        })
        .collect();
    Ok(serde_json::to_value(workspaces).unwrap_or(serde_json::Value::Null))
}

async fn create_mission(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::CreateMission = request.parse_params().map_err(rpc)?;
    let (_, envelope) = missions::propose(daemon, &params, operator)?;
    Ok(serde_json::to_value(envelope).unwrap_or(serde_json::Value::Null))
}

fn mission_status(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let mission = daemon
        .store
        .get_mission(&MissionId::parse(params.mission)?)?;
    let workspace = daemon.store.get_workspace(&mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let envelope = missions::envelope_for(daemon, &mission, &config)?;
    Ok(serde_json::to_value(envelope).unwrap_or(serde_json::Value::Null))
}

fn list_missions(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    let missions: Vec<serde_json::Value> = daemon
        .store
        .list_missions(None)?
        .into_iter()
        .map(|mission| {
            serde_json::json!({
                "mission": mission.id.to_string(),
                "workspace": mission.workspace.to_string(),
                "objective": mission.objective,
                "state": mission.state.to_string(),
                "expires_at": mission.expiry,
            })
        })
        .collect();
    Ok(serde_json::Value::Array(missions))
}

async fn approve_mission(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let id = MissionId::parse(params.mission)?;
    let (mission, lease) = missions::approve(daemon, &id, operator)?;
    let workspace = daemon.store.get_workspace(&mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;

    // Starting the agent is best-effort: a mission is approved and its lease
    // issued regardless, and a failure to start is reported rather than
    // rolling back the approval the human just gave.
    let agent_status = match agent::start_environment(daemon, &workspace, &lease, &config).await {
        Ok(()) => "started".to_owned(),
        Err(error) => format!("not started: {error}"),
    };
    Ok(serde_json::json!({
        "mission": mission.id.to_string(),
        "lease": lease.id.to_string(),
        "state": mission.state.to_string(),
        "agent": agent_status,
    }))
}

fn deny_mission(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let mission = missions::deny(daemon, &MissionId::parse(params.mission)?, operator, None)?;
    Ok(serde_json::json!({"mission": mission.id.to_string(), "state": mission.state.to_string()}))
}

async fn revoke_mission(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let id = MissionId::parse(params.mission)?;
    // Revocation stops all further actor work immediately, including for
    // sub-agents: the store fan-out revokes every lease and session, and the
    // environments are torn down here.
    let leases = daemon.store.list_leases(&id)?;
    let mission = missions::revoke(daemon, &id, operator)?;
    for lease in leases {
        agent::stop_environment(daemon, &lease.id).await;
    }
    Ok(serde_json::json!({"mission": mission.id.to_string(), "state": mission.state.to_string()}))
}

fn renew_mission(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::RenewMission = request.parse_params().map_err(rpc)?;
    let lease = missions::renew(
        daemon,
        &MissionId::parse(params.mission)?,
        &params.extend_by,
        params.additional_task_runs,
    )?;
    Ok(serde_json::json!({
        "lease": lease.id.to_string(),
        "expires_at": lease.expires_at,
    }))
}

async fn close_mission(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let id = MissionId::parse(params.mission)?;
    let mission = daemon.store.get_mission(&id)?;
    let workspace = daemon.store.get_workspace(&mission.workspace)?;

    // The closing diff is computed before the leases are revoked, because it is
    // computed against the live tree the mission was working in.
    let paths: Vec<String> = mission
        .scope
        .edit_paths
        .iter()
        .map(ToString::to_string)
        .collect();
    // A workspace need not be a git repository, and a repository need not have a
    // commit yet. Neither is a reason to refuse to close a mission: the closeout
    // says there was no diff and why, rather than leaving the mission open.
    let (diff, diff_note) =
        match clyde_git::diff::workspace_diff(&daemon.git, &workspace.root, &paths).await {
            Ok(diff) => {
                let note = diff.stat.render();
                (diff, note)
            }
            Err(error) => (
                clyde_git::WorkspaceDiff::default(),
                format!("no closing diff: {error}"),
            ),
        };
    let artifact = if diff.is_empty() {
        None
    } else {
        Some(
            crate::artifacts::store_bytes(
                daemon.store.as_ref(),
                &daemon.paths.blobs(),
                &id,
                clyde_core::artifact::ArtifactKind::Diff,
                clyde_core::classification::TrustClass::T1,
                None,
                diff.patch.as_bytes(),
            )?
            .id,
        )
    };

    let leases = daemon.store.list_leases(&id)?;
    let closed = missions::close(daemon, &id, diff_note.clone(), artifact)?;
    for lease in leases {
        agent::stop_environment(daemon, &lease.id).await;
    }
    Ok(serde_json::json!({
        "mission": closed.id.to_string(),
        "state": closed.state.to_string(),
        "diff": diff_note,
    }))
}

async fn review_mission(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let review = review::build(daemon, &MissionId::parse(params.mission)?).await?;
    Ok(serde_json::to_value(review).unwrap_or(serde_json::Value::Null))
}

fn list_approvals(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    let pending: Vec<serde_json::Value> = daemon
        .store
        .list_pending_approvals(Utc::now())?
        .into_iter()
        .map(|request| {
            let record = clyde_store::ApprovalRecord {
                request,
                decision: None,
            };
            serde_json::to_value(approvals::render(&record, None))
                .unwrap_or(serde_json::Value::Null)
        })
        .collect();
    Ok(serde_json::Value::Array(pending))
}

async fn decide_approval(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::DecideApproval = request.parse_params().map_err(rpc)?;
    let approval = ApprovalId::parse(params.approval)?;
    let decision = match params.decision.as_str() {
        "approve_once" | "approve" => Decision::ApproveOnce,
        "approve_for_mission" => Decision::ApproveForMission,
        "deny" => Decision::Deny,
        other => {
            return Err(DaemonError::invalid(format!(
                "{other} is not a decision; use approve_once, approve_for_mission, or deny"
            )));
        }
    };
    approvals::decide(daemon, &approval, operator, decision, params.note)?;

    // A brokered operation executes as part of the decision, so consumption and
    // execution stay adjacent.
    let record = daemon.store.get_approval(&approval)?;
    let executed = if decision.is_approval()
        && matches!(
            record.request.subject,
            clyde_core::approval::ApprovalSubject::BrokeredOperation { .. }
        ) {
        Some(publish::execute(daemon, &approval).await?)
    } else {
        None
    };

    Ok(serde_json::json!({
        "approval": approval.to_string(),
        "decision": decision.name(),
        "executed": executed,
    }))
}

fn show_audit(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::AuditQuery = request.parse_params().unwrap_or_default();
    let filter = AuditFilter {
        mission: params
            .mission
            .as_ref()
            .and_then(|id| MissionId::parse(id.clone()).ok()),
        workspace: params
            .workspace
            .as_ref()
            .and_then(|id| WorkspaceId::parse(id.clone()).ok()),
        limit: params.limit,
        ..AuditFilter::default()
    };
    let entries: Vec<admin::AuditEntry> = daemon
        .store
        .list_audit(&filter)?
        .into_iter()
        .filter(|event| !params.high_signal_only || event.kind.is_high_signal())
        .map(|event| admin::AuditEntry {
            seq: event.seq,
            at: event.at,
            kind: event.kind.name().to_owned(),
            mission: event.mission.as_ref().map(ToString::to_string),
            actor: event.actor.as_ref().map(ToString::to_string),
            detail: audit::describe(&event),
            high_signal: event.kind.is_high_signal(),
        })
        .collect();
    Ok(serde_json::to_value(entries).unwrap_or(serde_json::Value::Null))
}

fn verify_audit(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    match daemon.store.verify_audit() {
        Ok(()) => {
            let head = daemon.store.audit_head()?;
            Ok(serde_json::json!({
                "intact": true,
                "head": head.map(|head| head.seq),
            }))
        }
        Err(error) => Ok(serde_json::json!({
            "intact": false,
            "violation": error.to_string(),
        })),
    }
}

fn baseline_key(params: &admin::AccessTarget) -> Result<BaselineKey> {
    Ok(BaselineKey {
        workspace: WorkspaceId::parse(params.workspace.clone())?,
        task: TaskType::parse(&params.task)?,
        target: RepoPath::parse(&params.target)?,
    })
}

fn show_access(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::AccessTarget = request.parse_params().map_err(rpc)?;
    let key = baseline_key(&params)?;
    let baseline = daemon.store.get_baseline(&key)?;
    let proposal = daemon.store.get_baseline_proposal(&key)?;
    Ok(
        serde_json::to_value(access::render(&key, baseline.as_ref(), proposal.as_ref()))
            .unwrap_or(serde_json::Value::Null),
    )
}

fn propose_access(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::AccessTarget = request.parse_params().map_err(rpc)?;
    let key = baseline_key(&params)?;
    let mission = daemon.store.active_mission(&key.workspace)?;
    let scope =
        mission
            .map(|mission| mission.scope)
            .unwrap_or_else(|| clyde_core::mission::MissionScope {
                edit_paths: [key.target.clone()].into_iter().collect(),
                read_paths: Default::default(),
            });
    let proposal = access::propose(daemon, &key.workspace, key.task, &key.target, &scope)?;
    Ok(
        serde_json::to_value(access::render(&key, None, Some(&proposal)))
            .unwrap_or(serde_json::Value::Null),
    )
}

/// Records that learn mode was initiated.
///
/// Learn mode is admin-channel only and never reachable by an actor. The run
/// itself is performed by the operator through `task run --learn`; this records
/// the privilege being exercised.
fn learn_access(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::AccessTarget = request.parse_params().map_err(rpc)?;
    let key = baseline_key(&params)?;
    let mission = daemon
        .store
        .active_mission(&key.workspace)?
        .ok_or_else(|| DaemonError::not_found("learn mode needs an active mission"))?;
    access::record_learn_initiated(daemon, &key, operator, &mission.id);
    let proposal = access::propose_from_learn(daemon, &key, &mission.scope, &[])?;
    Ok(serde_json::json!({
        "status": "learn mode recorded; run the task and confirm the resulting proposal",
        "proposal_paths": proposal.paths.len(),
    }))
}

fn confirm_access(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::AccessTarget = request.parse_params().map_err(rpc)?;
    let key = baseline_key(&params)?;
    let baseline = access::confirm(daemon, &key, operator)?;
    Ok(
        serde_json::to_value(access::render(&key, Some(&baseline), None))
            .unwrap_or(serde_json::Value::Null),
    )
}

fn reset_access(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    let params: admin::AccessTarget = request.parse_params().map_err(rpc)?;
    let key = baseline_key(&params)?;
    let removed = access::reset(daemon, &key, operator)?;
    Ok(serde_json::json!({"removed": removed}))
}

fn import_bundle(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::ImportBundle = request.parse_params().map_err(rpc)?;
    let record = daemon.bundles.import(
        std::path::Path::new(&params.source),
        std::path::Path::new(&params.lockfile),
    )?;
    daemon.store.record_bundle(record.clone())?;
    Ok(serde_json::to_value(bundle_view(daemon, &record)).unwrap_or(serde_json::Value::Null))
}

fn list_bundles(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    let bundles: Vec<admin::BundleView> = daemon
        .store
        .list_bundles()?
        .iter()
        .map(|record| bundle_view(daemon, record))
        .collect();
    Ok(serde_json::to_value(bundles).unwrap_or(serde_json::Value::Null))
}

fn bundle_view(daemon: &Arc<Daemon>, record: &clyde_store::BundleRecord) -> admin::BundleView {
    admin::BundleView {
        artifact: record.artifact.to_string(),
        lockfile_digest: record.lockfile_digest.to_string(),
        crate_count: record.crate_count,
        code_executing_crates: record.inventory.entries.len(),
        registries: record.registries.clone(),
        inventory_confirmed: daemon
            .store
            .is_bundle_inventory_confirmed(&record.artifact)
            .unwrap_or(false),
        created_at: record.created_at,
    }
}

/// Confirms a bundle's inventory, which is what lets a build run against it.
fn confirm_inventory(
    daemon: &Arc<Daemon>,
    operator: &ActorId,
    request: &Request,
) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Params {
        artifact: String,
        #[serde(default)]
        target: Option<admin::AccessTarget>,
    }
    let params: Params = request.parse_params().map_err(rpc)?;
    let artifact = clyde_core::ids::ArtifactId::parse(params.artifact)?;
    let record = daemon
        .store
        .list_bundles()?
        .into_iter()
        .find(|record| record.artifact == artifact)
        .ok_or_else(|| DaemonError::not_found("no such dependency bundle"))?;

    daemon
        .store
        .set_bundle_inventory_confirmed(&artifact, operator.clone(), Utc::now())?;
    // Confirming the bundle also amends the baseline's pinned inventory, so the
    // next run compares against what the human just accepted.
    if let Some(target) = params.target {
        let key = baseline_key(&target)?;
        access::amend_inventory(daemon, &key, record.inventory.clone(), operator)?;
    }
    Ok(serde_json::json!({
        "artifact": artifact.to_string(),
        "confirmed": true,
        "code_executing_crates": record.inventory.entries.len(),
    }))
}

fn list_tasks(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    let params: admin::MissionRef = request.parse_params().map_err(rpc)?;
    let runs: Vec<TaskRunView> = daemon
        .store
        .list_task_runs(&MissionId::parse(params.mission)?)?
        .iter()
        .map(TaskRunView::new)
        .collect();
    Ok(serde_json::to_value(runs).unwrap_or(serde_json::Value::Null))
}

fn task_status(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Params {
        task_run: String,
    }
    let params: Params = request.parse_params().map_err(rpc)?;
    let run = daemon
        .store
        .get_task_run(&TaskRunId::parse(params.task_run)?)?;
    Ok(serde_json::to_value(TaskRunView::new(&run)).unwrap_or(serde_json::Value::Null))
}

fn task_logs(daemon: &Arc<Daemon>, request: &Request) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Params {
        task_run: String,
        #[serde(default)]
        stream: Option<String>,
        #[serde(default)]
        offset: Option<u64>,
    }
    let params: Params = request.parse_params().map_err(rpc)?;
    let id = TaskRunId::parse(params.task_run)?;
    let stream = params.stream.unwrap_or_else(|| "stderr".to_owned());
    let (content, truncated) = tasks::read_logs(daemon, &id, &stream, params.offset.unwrap_or(0))?;
    Ok(serde_json::json!({
        "task_run": id.to_string(),
        "stream": stream,
        "content": content,
        "truncated": truncated,
    }))
}

async fn doctor(daemon: &Arc<Daemon>) -> Result<serde_json::Value> {
    let mut report = serde_json::to_value(&daemon.host).unwrap_or(serde_json::Value::Null);
    if let Some(object) = report.as_object_mut() {
        // Posture is a first-class line rather than a note about tooling
        // hygiene: it is the difference between what the builder claims and what
        // D1 promised, and the weaker mode must never be the one you are
        // silently in (D26).
        object.insert(
            "posture".to_owned(),
            serde_json::json!({
                "posture": daemon.posture.name(),
                "reasons": daemon
                    .posture
                    .reasons()
                    .iter()
                    .map(clyde_core::posture::BypassReason::render)
                    .collect::<Vec<_>>(),
            }),
        );
        object.insert(
            "can_run_workspace".to_owned(),
            serde_json::Value::Bool(daemon.host.can_run_workspace()),
        );
        object.insert(
            "can_run_build".to_owned(),
            serde_json::Value::Bool(daemon.host.can_run_build()),
        );
        object.insert(
            "can_run_microvm".to_owned(),
            serde_json::Value::Bool(daemon.host.can_run_microvm()),
        );
        object.insert(
            "broker_reachable".to_owned(),
            serde_json::Value::Bool(daemon.broker.is_available().await),
        );
        object.insert(
            "runtime_root_workspace".to_owned(),
            match daemon.runtime_roots.check_workspace_root() {
                Some(assertion) if assertion.holds() => {
                    serde_json::Value::String("no project build toolchain present".to_owned())
                }
                Some(assertion) => serde_json::Value::String(assertion.render()),
                None => serde_json::Value::String("not configured".to_owned()),
            },
        );
    }
    Ok(report)
}

fn rpc(error: RpcError) -> DaemonError {
    DaemonError::invalid(error.message)
}

/// Runs a task on the operator surface (D25).
///
/// The only difference from the actor surface is who authenticated and what the
/// record says. Admission is the same call, against the same lease, policy,
/// budget, and access baseline — so a task reachable here is reachable there and
/// the reverse, and neither surface has a task type of its own.
async fn run_task(
    daemon: &Arc<Daemon>,
    principal: &Principal,
    request: &Request,
) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Params {
        /// Optional: with one active mission per workspace (D16) there is
        /// usually nothing to disambiguate.
        #[serde(default)]
        mission: Option<String>,
        #[serde(default)]
        workspace: Option<String>,
        task: String,
        path: String,
        #[serde(default)]
        options: serde_json::Value,
        /// An operator asking for stronger isolation than the policy floor.
        ///
        /// Operator-only, and it can only raise (D25).
        #[serde(default)]
        isolation: Option<String>,
    }
    let params: Params = request.parse_params().map_err(rpc)?;
    let task = TaskType::parse(&params.task)?;
    let path = RepoPath::parse(&params.path)?;
    let isolation_floor = params
        .isolation
        .as_deref()
        .map(|text| {
            clyde_core::classification::IsolationLevel::parse(text).ok_or_else(|| {
                // An unrecognised level is a refusal rather than a fallback to
                // the policy floor: a typo must not quietly run the task at
                // weaker isolation than the operator believes they asked for.
                DaemonError::invalid(format!(
                    "{text:?} is not an isolation level; use namespace-sandbox or microvm"
                ))
            })
        })
        .transpose()?;

    let mission = resolve_operator_mission(daemon, params.mission, params.workspace)?;
    let lease = primary_lease(daemon, &mission.id)?;
    let workspace = daemon.store.get_workspace(&mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;

    let options = default_options(task, &params.options)?;
    let context = TaskContext {
        mission,
        lease,
        workspace,
        config,
        isolation_floor,
        principal: principal.clone(),
    };
    let run = tasks::run_task(daemon, &context, task, path, options).await?;
    Ok(serde_json::to_value(TaskRunView::new(&run)).unwrap_or(serde_json::Value::Null))
}

/// Finds the mission an operator meant.
///
/// Named explicitly wins. Otherwise there must be exactly one active mission —
/// ambiguity is an error rather than a guess, because running a task against the
/// wrong mission charges the wrong budget and uses the wrong baseline.
fn resolve_operator_mission(
    daemon: &Arc<Daemon>,
    mission: Option<String>,
    workspace: Option<String>,
) -> Result<Mission> {
    if let Some(id) = mission {
        return Ok(daemon.store.get_mission(&MissionId::parse(id)?)?);
    }
    let wanted = workspace.map(WorkspaceId::parse).transpose()?;
    let mut active: Vec<Mission> = daemon
        .store
        .list_missions(wanted.as_ref())?
        .into_iter()
        .filter(|mission| mission.state == MissionState::Active)
        .collect();
    match active.len() {
        1 => Ok(active.remove(0)),
        0 => Err(DaemonError::not_found(
            "no active mission; create and approve one first",
        )),
        _ => Err(DaemonError::invalid(format!(
            "{} missions are active; name one with --mission: {}",
            active.len(),
            active
                .iter()
                .map(|mission| mission.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The mission's primary lease: the one that is not derived and not superseded.
///
/// An operator drives the primary lease. A derived lease belongs to a sub-agent
/// and is that sub-agent's to spend.
fn primary_lease(daemon: &Arc<Daemon>, mission: &MissionId) -> Result<Lease> {
    daemon
        .store
        .list_leases(mission)?
        .into_iter()
        .find(|lease| lease.parent.is_none() && lease.state != LeaseState::Superseded)
        .ok_or_else(|| DaemonError::not_found("this mission has no active lease"))
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

    #[test]
    fn the_operator_identity_comes_from_the_peer_not_the_request() {
        let actor = operator_identity(server::Peer {
            uid: 1000,
            gid: 1000,
            pid: Some(42),
        });
        assert_eq!(actor.as_str(), "human:uid-1000");
        assert!(
            actor.is_human(),
            "only a human actor may decide an approval"
        );
    }

    #[test]
    fn every_admin_method_is_dispatched() {
        // A method in the surface with no dispatch arm would return "not an
        // operator method" at runtime; this catches it at test time.
        let unhandled: Vec<&str> = methods::ALL
            .into_iter()
            .filter(|method| {
                !matches!(
                    *method,
                    methods::WORKSPACE_REGISTER
                        | methods::WORKSPACE_LIST
                        | methods::MISSION_CREATE
                        | methods::MISSION_STATUS
                        | methods::MISSION_LIST
                        | methods::MISSION_APPROVE
                        | methods::MISSION_DENY
                        | methods::MISSION_REVOKE
                        | methods::MISSION_RENEW
                        | methods::MISSION_CLOSE
                        | methods::MISSION_REVIEW
                        | methods::APPROVALS_LIST
                        | methods::APPROVALS_DECIDE
                        | methods::AUDIT_SHOW
                        | methods::AUDIT_VERIFY
                        | methods::ACCESS_SHOW
                        | methods::ACCESS_PROPOSE
                        | methods::ACCESS_LEARN
                        | methods::ACCESS_REVIEW
                        | methods::ACCESS_CONFIRM
                        | methods::ACCESS_RESET
                        | methods::DEPS_IMPORT
                        | methods::DEPS_LIST
                        | methods::DEPS_CONFIRM_INVENTORY
                        | methods::TASK_RUN
                        | methods::TASK_LIST
                        | methods::TASK_STATUS
                        | methods::TASK_LOGS
                        | methods::DOCTOR
                )
            })
            .collect();
        assert!(unhandled.is_empty(), "undispatched methods: {unhandled:?}");
    }
}
