//! The actor API: MCP over the actor socket (D10, D19).
//!
//! **Authentication.** The token is presented once, in `initialize`, and binds
//! the connection. Every subsequent request re-resolves token → session → lease
//! → mission before any other check, so expiry and revocation take effect
//! immediately rather than at the next connection. The socket is bind-mounted
//! only into the sandbox the token belongs to, so the connection and the token
//! identify the same actor.
//!
//! An unknown, expired, or revoked token is rejected identically, with no
//! information about which.

use std::sync::Arc;

use clyde_api::codec::{RequestReader, ResponseWriter};
use clyde_api::jsonrpc::{Error as RpcError, Id, Request, Response};
use clyde_api::mcp::{
    self, Content, InitializeResult, ServerCapabilities, ServerInfo, Tool, ToolResult,
    ToolsCapability,
};
use clyde_api::views::{
    ArtifactView, CapabilityView, DenialView, LogView, MissionView, SubagentView, TaskRunView,
    capability_for,
};
use clyde_core::classification::EgressProfile;
use clyde_core::ids::TaskRunId;
use clyde_core::repo_path::RepoPath;
use clyde_core::session::{SessionToken, TokenHash};
use clyde_core::task::{TaskOptions, TaskType};
use clyde_policy::resolve::{AdmissionInput, ApprovalState};
use clyde_store::ResolvedSession;
use tokio::net::UnixStream;

use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};
use crate::tasks::TaskContext;
use crate::{approvals, artifacts, subagents, tasks};

/// Serves one actor connection.
pub async fn serve(daemon: Arc<Daemon>, stream: UnixStream) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = RequestReader::new(read_half);
    let mut writer = ResponseWriter::new(write_half);
    let mut bound: Option<TokenHash> = None;

    loop {
        let request = match reader.next().await {
            Ok(request) => request,
            Err(clyde_api::codec::CodecError::Closed) => break,
            Err(error) => {
                let response = Response::failure(None, error.to_rpc_error());
                if writer.send(&response).await.is_err() {
                    break;
                }
                continue;
            }
        };
        if let Err(error) = request.validate() {
            let response = Response::failure(request.id.clone(), error);
            if writer.send(&response).await.is_err() {
                break;
            }
            continue;
        }
        if request.is_notification() {
            // `notifications/initialized` and friends expect no response.
            continue;
        }

        let response = handle(&daemon, &request, &mut bound).await;
        if writer.send(&response).await.is_err() {
            break;
        }
    }
}

async fn handle(
    daemon: &Arc<Daemon>,
    request: &Request,
    bound: &mut Option<TokenHash>,
) -> Response {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => match initialize(daemon, request, bound) {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::failure(id, error),
        },
        "tools/list" => {
            if bound.is_none() {
                return Response::failure(id, RpcError::unauthenticated());
            }
            Response::success(
                id,
                serde_json::json!({"tools": mcp::tools().into_iter().collect::<Vec<Tool>>()}),
            )
        }
        "tools/call" => call_tool(daemon, request, bound.as_ref(), id).await,
        "ping" => Response::success(id, serde_json::json!({})),
        other => Response::failure(id, RpcError::method_not_found(other)),
    }
}

/// Binds the connection to a session.
fn initialize(
    daemon: &Arc<Daemon>,
    request: &Request,
    bound: &mut Option<TokenHash>,
) -> std::result::Result<serde_json::Value, RpcError> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Params {
        /// The token, or the path to read it from inside the sandbox.
        token: Option<String>,
        token_file: Option<String>,
    }
    let params: Params = request.parse_params().unwrap_or_default();
    let raw = match (params.token, params.token_file) {
        (Some(token), _) => token,
        (None, Some(path)) => {
            std::fs::read_to_string(path).map_err(|_| RpcError::unauthenticated())?
        }
        (None, None) => return Err(RpcError::unauthenticated()),
    };
    let token = SessionToken::parse_hex(&raw).map_err(|_| RpcError::unauthenticated())?;
    let hash = token.hash();
    // Resolved once here to reject an unusable token at connection time, and
    // again on every request so revocation is immediate.
    resolve(daemon, &hash).map_err(|_| RpcError::unauthenticated())?;
    *bound = Some(hash);

    let result = InitializeResult {
        protocol_version: mcp::PROTOCOL_VERSION.to_owned(),
        capabilities: ServerCapabilities {
            tools: ToolsCapability {
                list_changed: false,
            },
        },
        server_info: ServerInfo {
            name: "clyded".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        instructions: Some(mcp::instructions()),
    };
    serde_json::to_value(result).map_err(|error| RpcError::internal(error.to_string()))
}

/// Resolves a token to its session, lease, and mission.
fn resolve(daemon: &Arc<Daemon>, hash: &TokenHash) -> Result<ResolvedSession> {
    daemon
        .store
        .resolve_token(hash, chrono::Utc::now())?
        .ok_or_else(|| DaemonError::invalid("no valid session"))
}

async fn call_tool(
    daemon: &Arc<Daemon>,
    request: &Request,
    bound: Option<&TokenHash>,
    id: Option<Id>,
) -> Response {
    let Some(hash) = bound else {
        return Response::failure(id, RpcError::unauthenticated());
    };
    // Re-resolved on every call: a revoked lease stops working immediately, not
    // at the next connection.
    let Ok(session) = resolve(daemon, hash) else {
        return Response::failure(id, RpcError::unauthenticated());
    };

    #[derive(serde::Deserialize)]
    struct Params {
        name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    }
    let params: Params = match request.parse_params() {
        Ok(params) => params,
        Err(error) => return Response::failure(id, error),
    };

    let outcome = dispatch(daemon, &session, &params.name, &params.arguments).await;
    let result = match outcome {
        Ok(result) => result,
        Err(DaemonError::Denied(reasons)) => {
            ToolResult::denial(&DenialView::new(params.name.clone(), &reasons))
        }
        Err(DaemonError::ApprovalRequired(message)) => ToolResult::error(message),
        Err(error) => ToolResult::error(error.to_string()),
    };
    match serde_json::to_value(result) {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::failure(id, RpcError::internal(error.to_string())),
    }
}

async fn dispatch(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    name: &str,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    match name {
        "mission_status" => Ok(ToolResult::json(&MissionView::new(
            &session.mission,
            &session.lease,
        ))),
        "list_capabilities" => Ok(ToolResult::json(&capabilities(daemon, session)?)),
        "run_task" => run_task(daemon, session, arguments).await,
        "task_status" => task_status(daemon, session, arguments),
        "task_logs" => task_logs(daemon, session, arguments),
        "list_artifacts" => {
            let artifacts: Vec<ArtifactView> = daemon
                .store
                .list_artifacts(&session.mission.id)?
                .iter()
                .map(ArtifactView::new)
                .collect();
            Ok(ToolResult::json(&artifacts))
        }
        "request_escalation" => request_escalation(daemon, session, arguments),
        "request_subagent" => subagents::request(daemon, session, arguments).await,
        "request_publish" => crate::publish::request(daemon, session, arguments).await,
        "commit_prepare" => commit_prepare(daemon, session, arguments).await,
        other => Ok(ToolResult::error(format!(
            "{other} is not a tool this server provides"
        ))),
    }
}

/// What this lease may do now, and what would need an escalation.
fn capabilities(daemon: &Arc<Daemon>, session: &ResolvedSession) -> Result<Vec<CapabilityView>> {
    let workspace = daemon.store.get_workspace(&session.mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let target = session
        .lease
        .repo_scope
        .edit_paths
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(RepoPath::root);

    let mut views = Vec::new();
    for task in TaskType::ALL {
        let policy = clyde_policy::builtin_policy(task);
        let admission = clyde_policy::validate_action(AdmissionInput {
            task,
            path: &target,
            mission: &session.mission,
            lease: &session.lease,
            config: &config,
            now: chrono::Utc::now(),
            host: daemon.host_capabilities(),
            baseline_confirmed: crate::access::confirmed(
                daemon,
                &clyde_core::baseline::BaselineKey {
                    workspace: workspace.id.clone(),
                    task,
                    target: target.clone(),
                },
            )?
            .is_some(),
            approval: ApprovalState::None,
        });
        let reason = admission.reasons.first();
        // An escalation is possible where the mission's envelope permits the
        // task at all; a task outside the envelope needs a new mission.
        let escalation_possible = session.mission.allowed_tasks.contains(&task)
            && session.lease.authority.may_request_tasks;
        views.push(capability_for(
            task,
            admission.is_allowed(),
            escalation_possible,
            policy.egress.to_string(),
            admission.needs_approval()
                || policy.approval != clyde_core::classification::ApprovalRequirement::None,
            reason,
        ));
    }
    Ok(views)
}

async fn run_task(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        task: String,
        path: String,
        #[serde(default)]
        options: serde_json::Value,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;
    let task = TaskType::parse(&args.task)?;
    let path = RepoPath::parse(&args.path)?;
    let options = default_options(task, &args.options)?;

    let workspace = daemon.store.get_workspace(&session.mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let context = TaskContext {
        mission: session.mission.clone(),
        lease: session.lease.clone(),
        workspace,
        config,
    };
    let run = tasks::run_task(daemon, &context, task, path, options).await?;
    Ok(ToolResult::json(&TaskRunView::new(&run)))
}

/// Builds task options, defaulting where the agent gave none.
fn default_options(task: TaskType, provided: &serde_json::Value) -> Result<TaskOptions> {
    if !provided.is_null() && provided.as_object().is_some_and(|map| !map.is_empty()) {
        let mut value = provided.clone();
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "task".to_owned(),
                serde_json::Value::String(task.name().to_owned()),
            );
        }
        return serde_json::from_value(value)
            .map_err(|error| DaemonError::invalid(error.to_string()));
    }
    Ok(match task {
        TaskType::WorkspaceRead => TaskOptions::WorkspaceRead { max_bytes: None },
        TaskType::WorkspaceEdit => TaskOptions::WorkspaceEdit {
            summary: "agent edit".to_owned(),
        },
        TaskType::RepoSearch => TaskOptions::RepoSearch {
            pattern: ".".to_owned(),
        },
        TaskType::RustCheck => TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
        TaskType::RustTestUnit => TaskOptions::RustTestUnit {
            package: None,
            filter: None,
        },
        TaskType::RustResolveDeps => {
            return Err(DaemonError::invalid(
                "rust.resolve-deps needs the lockfile digest it is fetching for",
            ));
        }
        TaskType::GitCommitPrepare => {
            return Err(DaemonError::invalid("commit_prepare needs a message"));
        }
        TaskType::GitPush => {
            return Err(DaemonError::invalid(
                "use request_publish rather than run_task for a push",
            ));
        }
    })
}

fn task_status(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    let id = task_run_argument(arguments)?;
    let run = daemon.store.get_task_run(&id)?;
    // An actor sees its own mission's tasks and nothing else.
    ensure_own_mission(daemon, session, &run)?;
    Ok(ToolResult::json(&TaskRunView::new(&run)))
}

fn task_logs(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        task_run: String,
        #[serde(default)]
        stream: Option<String>,
        #[serde(default)]
        offset: Option<u64>,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;
    let id = TaskRunId::parse(args.task_run)?;
    let run = daemon.store.get_task_run(&id)?;
    ensure_own_mission(daemon, session, &run)?;
    let stream = args.stream.unwrap_or_else(|| "stderr".to_owned());
    let (content, truncated) = tasks::read_logs(daemon, &id, &stream, args.offset.unwrap_or(0))?;
    Ok(ToolResult::json(&LogView {
        task_run: id.to_string(),
        stream,
        offset: args.offset.unwrap_or(0),
        content,
        truncated,
        complete: run.state.is_terminal(),
    }))
}

/// Refuses a request for another mission's data.
fn ensure_own_mission(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    run: &clyde_core::task::TaskRun,
) -> Result<()> {
    let lease = daemon.store.get_lease(&run.request.lease)?;
    if lease.mission != session.mission.id {
        // Reported as "not found" rather than "forbidden": an actor should not
        // learn that another mission's task exists.
        return Err(DaemonError::not_found("no such task run"));
    }
    Ok(())
}

fn request_escalation(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        task: String,
        reason: String,
        #[serde(default)]
        alternatives: Vec<String>,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;
    let task = TaskType::parse(&args.task)?;
    let policy = clyde_policy::builtin_policy(task);
    let workspace = daemon.store.get_workspace(&session.mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let hosts = clyde_policy::egress::resolve_allowlist(
        &policy.egress,
        &config.registries.allowed,
        &config.egress.model_api_hosts,
    );

    // The digest binds the approval to this task for this lease, so approving it
    // does not approve a different one later.
    let digest = clyde_core::Digest::of_canonical(
        "clyde.escalation.v1",
        &serde_json::json!({
            "lease": session.lease.id.as_str(),
            "task": task.name(),
        }),
    )
    .map_err(|error| DaemonError::internal(error.to_string()))?;

    let request = approvals::request(
        daemon,
        &session.mission.id,
        &session.lease.id,
        &session.lease.actor,
        approvals::ApprovalContext {
            subject: clyde_core::approval::ApprovalSubject::TaskEscalation {
                task,
                egress: policy.egress.clone(),
            },
            request_digest: digest,
            reason: args.reason,
            alternatives: args.alternatives,
            prior_failure: last_failure(daemon, session),
            egress_hosts: hosts.iter().map(ToString::to_string).collect(),
            egress_profile: policy.egress.to_string(),
            credentials: policy.credentials.to_string(),
            outputs: policy
                .outputs
                .iter()
                .map(|output| format!("{output:?}"))
                .collect(),
            lockfile_change: None,
            inventory_diff: Vec::new(),
            task_evidence: Vec::new(),
            caveats: if policy.egress.is_none() {
                Vec::new()
            } else {
                approvals::egress_caveats()
            },
        },
    )?;

    Ok(ToolResult::json(&clyde_api::views::EscalationView {
        approval: request.id.to_string(),
        subject: request.subject.kind_name().to_owned(),
        summary: format!(
            "{} with egress {} is pending human approval",
            task.name(),
            policy.egress.name()
        ),
        expires_at: request.expires_at,
    }))
}

/// The most recent failure in this mission, so the prompt can say what went
/// wrong.
///
/// Deliberately not filtered to the escalated task: an escalation is usually
/// asked for *because* a different task failed — a fetch is requested because a
/// check could not proceed offline — and the failure the human needs to see is
/// the one that motivated the request.
fn last_failure(daemon: &Arc<Daemon>, session: &ResolvedSession) -> Option<String> {
    daemon
        .store
        .list_task_runs(&session.mission.id)
        .ok()?
        .into_iter()
        .filter_map(|run| run.outcome.map(|outcome| (run.request.task, outcome)))
        .rfind(|(_, outcome)| !outcome.classification.is_success())
        .map(|(task, outcome)| {
            format!(
                "{} failed with {}: {}",
                task.name(),
                outcome.classification.name(),
                outcome.summary
            )
        })
}

async fn commit_prepare(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        message: String,
        #[serde(default)]
        paths: Vec<String>,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;
    let workspace = daemon.store.get_workspace(&session.mission.workspace)?;
    let config = daemon.config_for(&workspace.root)?;
    let context = TaskContext {
        mission: session.mission.clone(),
        lease: session.lease.clone(),
        workspace,
        config,
    };
    let _ = args.paths;
    let run = tasks::run_task(
        daemon,
        &context,
        TaskType::GitCommitPrepare,
        RepoPath::root(),
        TaskOptions::GitCommitPrepare {
            message: args.message,
        },
    )
    .await?;
    Ok(ToolResult::json(&TaskRunView::new(&run)))
}

fn task_run_argument(arguments: &serde_json::Value) -> Result<TaskRunId> {
    let raw = arguments
        .get("task_run")
        .and_then(|value| value.as_str())
        .ok_or_else(|| DaemonError::invalid("task_run is required"))?;
    Ok(TaskRunId::parse(raw)?)
}

/// The subagent view, shared with the subagent module.
pub fn subagent_view(lease: &clyde_core::lease::Lease) -> SubagentView {
    SubagentView {
        actor: lease.actor.to_string(),
        lease: lease.id.to_string(),
        edit_paths: lease
            .repo_scope
            .edit_paths
            .iter()
            .map(ToString::to_string)
            .collect(),
        allowed_tasks: lease
            .task_scope
            .iter()
            .map(|task| task.name().to_owned())
            .collect(),
        expires_at: lease.expires_at,
    }
}

/// Reads an artifact for the actor surface.
pub fn read_artifact(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    id: &clyde_core::ids::ArtifactId,
) -> Result<Vec<u8>> {
    let artifact = daemon.store.get_artifact(id)?;
    if artifact.mission != session.mission.id {
        return Err(DaemonError::not_found("no such artifact"));
    }
    artifacts::read(daemon.store.as_ref(), id)
}

/// The egress profile an actor's lease permits, for display.
pub fn lease_profile(session: &ResolvedSession) -> EgressProfile {
    session.lease.network_scope.clone()
}

/// Renders a tool result as text, for the CLI.
pub fn render_tool_result(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| match content {
            Content::Text { text } => text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    fn default_options_are_produced_for_the_offline_loop() {
        assert!(matches!(
            default_options(TaskType::RustCheck, &serde_json::Value::Null).unwrap(),
            TaskOptions::RustCheck { .. }
        ));
        assert!(matches!(
            default_options(TaskType::WorkspaceEdit, &serde_json::json!({})).unwrap(),
            TaskOptions::WorkspaceEdit { .. }
        ));
    }

    #[test]
    fn provided_options_are_used_and_validated() {
        let options = default_options(
            TaskType::RustCheck,
            &serde_json::json!({"package": "clyde-core", "all_targets": true}),
        )
        .unwrap();
        match options {
            TaskOptions::RustCheck {
                package,
                all_targets,
            } => {
                assert_eq!(package.as_deref(), Some("clyde-core"));
                assert!(all_targets);
            }
            other => panic!("unexpected options: {other:?}"),
        }
    }

    #[test]
    fn a_push_cannot_be_requested_through_run_task() {
        let error = default_options(TaskType::GitPush, &serde_json::Value::Null)
            .expect_err("a push goes through request_publish");
        assert!(error.to_string().contains("request_publish"));
    }

    #[test]
    fn a_fetch_needs_its_lockfile_digest() {
        assert!(default_options(TaskType::RustResolveDeps, &serde_json::Value::Null).is_err());
    }

    #[test]
    fn rendering_a_tool_result_joins_its_text() {
        let result = ToolResult::error("denied");
        assert_eq!(render_tool_result(&result), "denied");
    }
}
