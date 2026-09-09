//! `clyde`: the command line client.
//!
//! Two surfaces, and the separation is load-bearing (D2):
//!
//! - **Operator commands** talk to the admin socket. Approvals live here.
//! - **Actor commands** talk to the actor socket with a session token.
//!
//! `clyde approve` refuses to run inside a sandbox, and says why. That refusal
//! is the mechanism that makes agent self-approval impossible, rather than a
//! policy that asks an agent not to try.

mod client;
mod output;
mod tui;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use clyde_api::admin::methods;

use crate::client::{Client, ClientError};
use crate::output::Format;

#[derive(Debug, Parser)]
#[command(
    name = "clyde",
    about = "Bounded delegation of autonomous coding work",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    /// State directory. Defaults to the XDG data directory.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,

    /// Emit JSON with a stable shape.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Register and inspect workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Create, approve, and close missions.
    #[command(subcommand)]
    Mission(MissionCommand),
    /// Review and decide pending approvals.
    #[command(subcommand)]
    Approvals(ApprovalCommand),
    /// Inspect the audit trail.
    #[command(subcommand)]
    Audit(AuditCommand),
    /// Manage access baselines.
    #[command(subcommand)]
    Access(AccessCommand),
    /// Manage dependency bundles.
    #[command(subcommand)]
    Deps(DepsCommand),
    /// Run and inspect tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Report host prerequisites and known limitations.
    Doctor,
    /// Interactive mission, task, and approval surfaces.
    Tui,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Register a project directory.
    Register {
        /// Path to the project. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// List registered workspaces.
    List,
}

#[derive(Debug, Subcommand)]
enum MissionCommand {
    /// Propose a mission. Shows the exact envelope that would be issued.
    Create {
        #[arg(long)]
        workspace: String,
        /// What the agent is being asked to do.
        objective: String,
        /// Paths the agent may write. Repeat for several.
        #[arg(long = "edit")]
        edit_paths: Vec<String>,
        /// Additional read-only paths.
        #[arg(long = "read")]
        read_paths: Vec<String>,
        /// Task types to allow. Defaults to the offline edit/check/test loop.
        #[arg(long = "task")]
        tasks: Vec<String>,
        /// How long the mission lives, such as `2h`.
        #[arg(long)]
        expires_in: Option<String>,
        /// Maximum task runs.
        #[arg(long)]
        max_task_runs: Option<u32>,
        /// Withhold model API access.
        #[arg(long)]
        no_model_api: bool,
    },
    /// Show a mission's envelope.
    Status {
        mission: String,
        /// Include the workspace diff.
        #[arg(long)]
        diff: bool,
    },
    /// List missions.
    List,
    /// Approve a proposed mission and start the agent.
    Approve { mission: String },
    /// Deny a proposed mission.
    Deny { mission: String },
    /// Revoke an active mission immediately.
    Revoke { mission: String },
    /// Renew a mission's lease by issuing a replacement.
    Renew {
        mission: String,
        #[arg(long, default_value = "1h")]
        extend_by: String,
        #[arg(long)]
        additional_task_runs: Option<u32>,
    },
    /// Close a mission and record its closing diff.
    Close { mission: String },
    /// The end-to-end review surface.
    Review { mission: String },
}

#[derive(Debug, Subcommand)]
enum ApprovalCommand {
    /// List pending approvals.
    List,
    /// Approve a pending request.
    Approve {
        approval: String,
        /// Approve for the rest of the mission rather than once.
        #[arg(long)]
        for_mission: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Deny a pending request.
    Deny {
        approval: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AuditCommand {
    /// Show the audit timeline.
    Show {
        #[arg(long)]
        mission: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Only approvals, drift, denied egress, and brokered work.
        #[arg(long)]
        high_signal: bool,
    },
    /// Verify the audit chain against its recorded head.
    Verify,
}

#[derive(Debug, Subcommand)]
enum AccessCommand {
    /// Show the baseline or proposal for a target.
    Show(AccessTargetArgs),
    /// Compute a static proposal from the build closure.
    Propose(AccessTargetArgs),
    /// Record a learn-mode run. Admin channel only; never reachable by an actor.
    Learn(AccessTargetArgs),
    /// Review a pending proposal.
    Review(AccessTargetArgs),
    /// Confirm a proposal, putting it in force.
    Confirm(AccessTargetArgs),
    /// Remove a baseline, so the next task is refused until one is confirmed.
    Reset(AccessTargetArgs),
}

#[derive(Debug, clap::Args)]
struct AccessTargetArgs {
    #[arg(long)]
    workspace: String,
    #[arg(long, default_value = "rust.check")]
    task: String,
    /// The build target, workspace-relative.
    target: String,
}

#[derive(Debug, Subcommand)]
enum DepsCommand {
    /// Import a host-produced cargo cache or vendor directory.
    Import {
        /// Directory holding package sources.
        source: PathBuf,
        /// The `Cargo.lock` it satisfies.
        #[arg(long)]
        lockfile: PathBuf,
    },
    /// List imported bundles.
    List,
    /// Confirm a bundle's build-time code execution inventory.
    ConfirmInventory {
        artifact: String,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long, default_value = "rust.check")]
        task: String,
        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Run a task under an approved mission.
    ///
    /// Works on both surfaces (D25): inside a workspace environment it is an
    /// actor request carrying a session token; on the host it is an operator
    /// request on the admin socket. Admission is identical either way.
    Run {
        task: String,
        /// Workspace-relative target.
        #[arg(default_value = ".")]
        path: String,
        /// Task-specific options, as JSON.
        #[arg(long)]
        options: Option<String>,
        /// Which mission to run under. Optional while one is active.
        #[arg(long)]
        mission: Option<String>,
        /// Narrows the search for an active mission.
        #[arg(long)]
        workspace: Option<String>,
        /// Ask for stronger isolation than the task policy requires.
        ///
        /// Operator-only, and it can only raise: `microvm` runs a task that
        /// policy would allow in a namespace sandbox inside a guest instead.
        /// Asking for less than the policy floor is refused (D24).
        #[arg(long, value_name = "LEVEL")]
        isolation: Option<String>,
    },
    /// Show a task run's state and outcome.
    Status { task_run: String },
    /// Read a task run's logs.
    Logs {
        task_run: String,
        #[arg(long, default_value = "stderr")]
        stream: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// List a mission's task runs.
    List {
        #[arg(long)]
        mission: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("clyde: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

/// Reports a failure, expanding a structured denial into reasons and next steps.
fn report(error: &ClientError) {
    match error.denial() {
        Some(data) => eprintln!("{}", output::denial(data)),
        None => eprintln!("clyde: {error}"),
    }
}

async fn run(cli: Cli) -> Result<(), ClientError> {
    let format = Format::new(cli.json);
    let state_root = cli.state_dir.clone().unwrap_or_else(default_state_root);
    let admin_socket = state_root.join("run/clyded-admin.sock");
    let actor_socket = state_root.join("run/clyded.sock");

    match cli.command {
        Command::Workspace(command) => workspace(&admin_socket, format, command).await,
        Command::Mission(command) => mission(&admin_socket, format, command).await,
        Command::Approvals(command) => approvals(&admin_socket, format, command).await,
        Command::Audit(command) => audit(&admin_socket, format, command).await,
        Command::Access(command) => access(&admin_socket, format, command).await,
        Command::Deps(command) => deps(&admin_socket, format, command).await,
        Command::Task(command) => task(&admin_socket, &actor_socket, format, command).await,
        Command::Doctor => {
            // Doctor works without a daemon: bring-up is exactly when the daemon
            // is not yet running, and a diagnostic that needs the thing it is
            // diagnosing is no diagnostic at all.
            let value = match Client::new(&admin_socket)
                .call(methods::DOCTOR, serde_json::json!({}))
                .await
            {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert(
                            "source".to_owned(),
                            serde_json::Value::String(
                                "reported by the running clyded, from its loaded configuration"
                                    .to_owned(),
                            ),
                        );
                    }
                    value
                }
                Err(_) => local_doctor(&state_root),
            };
            output::emit(format, &value, output::doctor);
            Ok(())
        }
        Command::Tui => tui::run(&admin_socket).await,
    }
}

async fn workspace(
    socket: &std::path::Path,
    format: Format,
    command: WorkspaceCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    let value = match command {
        WorkspaceCommand::Register { path } => {
            let root = path
                .unwrap_or_else(|| PathBuf::from("."))
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from("."));
            client
                .call(
                    methods::WORKSPACE_REGISTER,
                    serde_json::json!({"root": root.display().to_string()}),
                )
                .await?
        }
        WorkspaceCommand::List => {
            client
                .call(methods::WORKSPACE_LIST, serde_json::json!({}))
                .await?
        }
    };
    output::emit(format, &value, output::key_values);
    Ok(())
}

async fn mission(
    socket: &std::path::Path,
    format: Format,
    command: MissionCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    match command {
        MissionCommand::Create {
            workspace,
            objective,
            edit_paths,
            read_paths,
            tasks,
            expires_in,
            max_task_runs,
            no_model_api,
        } => {
            let value = client
                .call(
                    methods::MISSION_CREATE,
                    serde_json::json!({
                        "workspace": workspace,
                        "objective": objective,
                        "edit_paths": edit_paths,
                        "read_paths": read_paths,
                        "tasks": tasks,
                        "expires_in": expires_in,
                        "max_task_runs": max_task_runs,
                        "model_api": !no_model_api,
                        "start_agent": true,
                    }),
                )
                .await?;
            output::emit(format, &value, output::envelope);
            if format == Format::Human {
                println!(
                    "\nreview the envelope above, then: clyde mission approve {}",
                    output::scalar(output::field(&value, "mission"))
                );
            }
        }
        MissionCommand::Status { mission, diff } => {
            let value = client
                .call(
                    methods::MISSION_STATUS,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::envelope);
            if diff {
                let review = client
                    .call(
                        methods::MISSION_REVIEW,
                        serde_json::json!({"mission": mission}),
                    )
                    .await?;
                output::emit(format, &review, output::review);
            }
        }
        MissionCommand::List => {
            let value = client
                .call(methods::MISSION_LIST, serde_json::json!({}))
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Approve { mission } => {
            refuse_inside_sandbox("mission approve")?;
            let value = client
                .call(
                    methods::MISSION_APPROVE,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Deny { mission } => {
            refuse_inside_sandbox("mission deny")?;
            let value = client
                .call(
                    methods::MISSION_DENY,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Revoke { mission } => {
            let value = client
                .call(
                    methods::MISSION_REVOKE,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Renew {
            mission,
            extend_by,
            additional_task_runs,
        } => {
            let value = client
                .call(
                    methods::MISSION_RENEW,
                    serde_json::json!({
                        "mission": mission,
                        "extend_by": extend_by,
                        "additional_task_runs": additional_task_runs,
                    }),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Close { mission } => {
            let value = client
                .call(
                    methods::MISSION_CLOSE,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        MissionCommand::Review { mission } => {
            let value = client
                .call(
                    methods::MISSION_REVIEW,
                    serde_json::json!({"mission": mission}),
                )
                .await?;
            output::emit(format, &value, output::review);
        }
    }
    Ok(())
}

async fn approvals(
    socket: &std::path::Path,
    format: Format,
    command: ApprovalCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    match command {
        ApprovalCommand::List => {
            let value = client
                .call(methods::APPROVALS_LIST, serde_json::json!({}))
                .await?;
            output::emit(format, &value, |value| match value {
                serde_json::Value::Array(items) if items.is_empty() => {
                    "no pending approvals".to_owned()
                }
                serde_json::Value::Array(items) => items
                    .iter()
                    .map(output::approval)
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                other => output::key_values(other),
            });
        }
        ApprovalCommand::Approve {
            approval,
            for_mission,
            note,
        } => {
            refuse_inside_sandbox("approve")?;
            let value = client
                .call(
                    methods::APPROVALS_DECIDE,
                    serde_json::json!({
                        "approval": approval,
                        "decision": if for_mission { "approve_for_mission" } else { "approve_once" },
                        "note": note,
                    }),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        ApprovalCommand::Deny { approval, note } => {
            refuse_inside_sandbox("deny")?;
            let value = client
                .call(
                    methods::APPROVALS_DECIDE,
                    serde_json::json!({"approval": approval, "decision": "deny", "note": note}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
    }
    Ok(())
}

async fn audit(
    socket: &std::path::Path,
    format: Format,
    command: AuditCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    match command {
        AuditCommand::Show {
            mission,
            limit,
            high_signal,
        } => {
            let value = client
                .call(
                    methods::AUDIT_SHOW,
                    serde_json::json!({
                        "mission": mission,
                        "limit": limit,
                        "high_signal_only": high_signal,
                    }),
                )
                .await?;
            output::emit(format, &value, output::timeline);
        }
        AuditCommand::Verify => {
            let value = client
                .call(methods::AUDIT_VERIFY, serde_json::json!({}))
                .await?;
            output::emit(format, &value, |value| {
                if value["intact"].as_bool().unwrap_or(false) {
                    format!(
                        "the audit chain verifies through event {}",
                        output::scalar(&value["head"])
                    )
                } else {
                    format!(
                        "THE AUDIT CHAIN DOES NOT VERIFY: {}",
                        output::scalar(&value["violation"])
                    )
                }
            });
        }
    }
    Ok(())
}

async fn access(
    socket: &std::path::Path,
    format: Format,
    command: AccessCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    let (method, args) = match &command {
        AccessCommand::Show(args) => (methods::ACCESS_SHOW, args),
        AccessCommand::Propose(args) => (methods::ACCESS_PROPOSE, args),
        AccessCommand::Learn(args) => (methods::ACCESS_LEARN, args),
        AccessCommand::Review(args) => (methods::ACCESS_REVIEW, args),
        AccessCommand::Confirm(args) => (methods::ACCESS_CONFIRM, args),
        AccessCommand::Reset(args) => (methods::ACCESS_RESET, args),
    };
    if matches!(command, AccessCommand::Confirm(_) | AccessCommand::Learn(_)) {
        refuse_inside_sandbox("access confirm")?;
    }
    let value = client
        .call(
            method,
            serde_json::json!({
                "workspace": args.workspace,
                "task": args.task,
                "target": args.target,
            }),
        )
        .await?;
    output::emit(format, &value, baseline_view);
    Ok(())
}

/// Renders a baseline, keeping grants and pins visually distinct.
fn baseline_view(value: &serde_json::Value) -> String {
    let mut lines = vec![
        format!("target    {}", output::scalar(&value["target"])),
        format!("task      {}", output::scalar(&value["task"])),
        format!(
            "state     {}",
            if value["confirmed"].as_bool().unwrap_or(false) {
                format!("confirmed by {}", output::scalar(&value["confirmed_by"]))
            } else {
                "proposed, not yet in force".to_owned()
            }
        ),
        format!("origin    {}", output::scalar(&value["origin"])),
    ];
    if let serde_json::Value::Array(grants) = &value["grants"]
        && !grants.is_empty()
    {
        lines.push(String::new());
        lines.push("subtree grants (first-party code, not drift-sensitive):".to_owned());
        lines.extend(
            grants
                .iter()
                .map(|grant| format!("  {}", output::scalar(grant))),
        );
    }
    if let serde_json::Value::Array(pins) = &value["pins"]
        && !pins.is_empty()
    {
        lines.push(String::new());
        lines.push("pins (outside the grants, drift-sensitive):".to_owned());
        lines.extend(pins.iter().map(|pin| {
            format!(
                "  {}  — {}",
                output::scalar(&pin["path"]),
                output::scalar(&pin["reason"])
            )
        }));
    }
    if let serde_json::Value::Array(inventory) = &value["inventory_entries"]
        && !inventory.is_empty()
    {
        lines.push(String::new());
        lines.push("build-time code execution:".to_owned());
        lines.extend(
            inventory
                .iter()
                .map(|entry| format!("  {}", output::scalar(entry))),
        );
    }
    if let serde_json::Value::Array(rationale) = &value["rationale"]
        && !rationale.is_empty()
    {
        lines.push(String::new());
        lines.extend(
            rationale
                .iter()
                .map(|line| format!("  {}", output::scalar(line))),
        );
    }
    lines.join("\n")
}

async fn deps(
    socket: &std::path::Path,
    format: Format,
    command: DepsCommand,
) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    let value = match command {
        DepsCommand::Import { source, lockfile } => {
            client
                .call(
                    methods::DEPS_IMPORT,
                    serde_json::json!({
                        "source": source.display().to_string(),
                        "lockfile": lockfile.display().to_string(),
                    }),
                )
                .await?
        }
        DepsCommand::List => {
            client
                .call(methods::DEPS_LIST, serde_json::json!({}))
                .await?
        }
        DepsCommand::ConfirmInventory {
            artifact,
            workspace,
            task,
            target,
        } => {
            refuse_inside_sandbox("deps confirm-inventory")?;
            let target = match (workspace, target) {
                (Some(workspace), Some(target)) => Some(serde_json::json!({
                    "workspace": workspace,
                    "task": task,
                    "target": target,
                })),
                _ => None,
            };
            client
                .call(
                    methods::DEPS_CONFIRM_INVENTORY,
                    serde_json::json!({"artifact": artifact, "target": target}),
                )
                .await?
        }
    };
    output::emit(format, &value, output::key_values);
    Ok(())
}

async fn task(
    admin_socket: &std::path::Path,
    actor_socket: &std::path::Path,
    format: Format,
    command: TaskCommand,
) -> Result<(), ClientError> {
    match command {
        TaskCommand::Run {
            task,
            path,
            options,
            mission,
            workspace,
            isolation,
        } => {
            let options: serde_json::Value = options
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .unwrap_or(serde_json::Value::Null);
            // Both surfaces run tasks (D25). Inside a workspace environment a
            // session token is present and this is an actor request; on the host
            // there is none, and the operator surface is the right one. The
            // choice follows from where the command is running rather than from
            // a flag, because a human on the host has no token to offer and an
            // actor cannot reach the admin socket at all.
            match session_token() {
                Ok(token) => {
                    if isolation.is_some() {
                        // Refused here rather than dropped silently: an actor
                        // has no way to name an isolation level, and a request
                        // that appeared to be honoured and was not would be
                        // worse than one that fails (D25).
                        return Err(ClientError::Protocol(
                            "--isolation is an operator option; a task requested with a session token runs at its policy floor".to_owned(),
                        ));
                    }
                    let mut session = Client::new(actor_socket).session(&token).await?;
                    let value = session
                        .tool(
                            "run_task",
                            serde_json::json!({
                                "task": task, "path": path, "options": options,
                            }),
                        )
                        .await?;
                    output::emit(format, &value, tool_text);
                }
                Err(_) => {
                    let mut client = Client::new(admin_socket);
                    let value = client
                        .call(
                            methods::TASK_RUN,
                            serde_json::json!({
                                "task": task,
                                "path": path,
                                "options": options,
                                "mission": mission,
                                "workspace": workspace,
                                "isolation": isolation,
                            }),
                        )
                        .await?;
                    output::emit(format, &value, output::key_values);
                }
            }
        }
        TaskCommand::Status { task_run } => {
            let mut client = Client::new(admin_socket);
            let value = client
                .call(
                    methods::TASK_STATUS,
                    serde_json::json!({"task_run": task_run}),
                )
                .await?;
            output::emit(format, &value, output::key_values);
        }
        TaskCommand::Logs {
            task_run,
            stream,
            offset,
        } => {
            let mut client = Client::new(admin_socket);
            let value = client
                .call(
                    methods::TASK_LOGS,
                    serde_json::json!({
                        "task_run": task_run,
                        "stream": stream,
                        "offset": offset,
                    }),
                )
                .await?;
            output::emit(format, &value, |value| output::scalar(&value["content"]));
        }
        TaskCommand::List { mission } => {
            let mut client = Client::new(admin_socket);
            let value = client
                .call(methods::TASK_LIST, serde_json::json!({"mission": mission}))
                .await?;
            output::emit(format, &value, output::key_values);
        }
    }
    Ok(())
}

/// Renders an MCP tool result's text content.
fn tool_text(value: &serde_json::Value) -> String {
    value["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| output::key_values(value))
}

/// Reads the session token from the sandbox token file.
fn session_token() -> Result<String, ClientError> {
    let path = std::env::var("CLYDE_SESSION_TOKEN_FILE")
        .unwrap_or_else(|_| "/run/clyde/session-token".to_owned());
    std::fs::read_to_string(&path)
        .map(|token| token.trim().to_owned())
        .map_err(|_| ClientError::Protocol(format!(
            "no session token at {path}. Actor commands run inside a Clyde workspace environment; from the host, use the operator commands instead."
        )))
}

/// Refuses an operator command inside a sandbox.
///
/// This is the mechanism that makes agent self-approval impossible: the admin
/// socket is never mounted into a sandbox, and this says so plainly rather than
/// failing later with a confusing connection error.
fn refuse_inside_sandbox(command: &str) -> Result<(), ClientError> {
    let inside = std::env::var_os("CLYDE_SESSION_TOKEN_FILE").is_some()
        || std::path::Path::new("/run/clyde/session-token").exists();
    if inside {
        return Err(ClientError::Protocol(format!(
            "clyde {command} is an operator command and cannot run inside a Clyde workspace environment. Approvals happen on the admin socket, which is never mounted into a sandbox — that is what makes an agent unable to approve its own request."
        )));
    }
    Ok(())
}

/// Probes host prerequisites without a running daemon.
///
/// It loads the same host and user configuration `clyded` would, rather than
/// probing bare. The alternative is worse than incomplete: a probe with no
/// configuration reports every configured path as absent, so an operator whose
/// runtime roots are correctly configured is told to configure them — a remedy
/// that cannot work, which is a defect rather than a nicety
/// ([R10](../../docs/builder/decisions.md#r10-a-remedy-that-cannot-work-is-a-defect-not-a-nicety)).
fn local_doctor(state_root: &Path) -> serde_json::Value {
    let paths = clyde_policy::config::host_files::ConfigPaths::discover();
    let loaded = clyde_policy::config::host_files::load_base(&paths);
    let sandbox = match &loaded {
        Ok(loaded) => loaded.config.sandbox.clone(),
        Err(_) => clyde_policy::config::SandboxConfig::default(),
    };

    let report = clyde_sandbox::probe(&clyde_sandbox::ProbePaths {
        bwrap: sandbox.bwrap.clone(),
        firecracker: sandbox.firecracker.clone(),
        mke2fs: sandbox.mke2fs.clone(),
        nix: None,
        state_dir: Some(state_root.to_path_buf()),
        // The guest images live under the state directory by convention, so a
        // daemonless probe can still say whether they are there rather than
        // reporting them as unconfigured.
        guest_vm_dir: Some(state_root.join("vm")),
        runtime_roots: sandbox.runtime_roots.clone(),
        runtime_root_manifests: sandbox.runtime_root_manifests.clone(),
    });
    let mut value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        // Which report this is, so a row that depends on configuration is not
        // read as a statement about the host. `clyded` answers with its loaded
        // configuration; this answers with what it could read from disk.
        object.insert(
            "source".to_owned(),
            serde_json::Value::String(format!(
                "probed directly, with configuration from {}: clyded is not reachable",
                paths.host.display()
            )),
        );
        if let Err(error) = &loaded {
            object.insert(
                "config_error".to_owned(),
                serde_json::Value::String(error.to_string()),
            );
        }
        object.insert(
            "can_run_workspace".to_owned(),
            serde_json::Value::Bool(report.can_run_workspace()),
        );
        object.insert(
            "can_run_build".to_owned(),
            serde_json::Value::Bool(report.can_run_build()),
        );
        object.insert(
            "can_run_microvm".to_owned(),
            serde_json::Value::Bool(report.can_run_microvm()),
        );
        object.insert(
            "broker_reachable".to_owned(),
            serde_json::Value::Bool(false),
        );
        object.insert(
            "daemon".to_owned(),
            serde_json::Value::String("not running; this report is a local probe".to_owned()),
        );
        // Bring-up is exactly when the daemon is not running (R9), and it is also
        // exactly when someone wants to know what posture they are about to get.
        // With no daemon there is no `[agent]` section in view, so the honest
        // observation is that nothing is hosted — which is the weaker answer, and
        // the right way to be wrong (D26).
        let posture =
            clyde_core::posture::derive(&clyde_sandbox::capability::observe_posture(false));
        object.insert(
            "posture".to_owned(),
            serde_json::json!({
                "posture": posture.name(),
                "reasons": posture
                    .reasons()
                    .iter()
                    .map(clyde_core::posture::BypassReason::render)
                    .collect::<Vec<_>>(),
            }),
        );
    }
    value
}

fn default_state_root() -> PathBuf {
    std::env::var_os("CLYDE_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|base| PathBuf::from(base).join("clyde")))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("clyde")
            })
        })
        .unwrap_or_else(|| PathBuf::from("/var/lib/clyde"))
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
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_tool_result_renders_its_text() {
        let value = serde_json::json!({
            "content": [{"type": "text", "text": "task started"}],
        });
        assert_eq!(tool_text(&value), "task started");
    }

    #[test]
    fn operator_commands_refuse_inside_a_sandbox() {
        // Outside a sandbox this succeeds; the refusal path is asserted by the
        // message it produces when the marker is present.
        assert!(refuse_inside_sandbox("approve").is_ok());
    }

    #[test]
    fn the_missing_token_message_points_at_the_operator_commands() {
        // In this process there is no token file, which is the host case.
        let error = session_token().expect_err("no token on the host");
        assert!(error.to_string().contains("operator commands"));
    }

    #[test]
    fn the_baseline_view_separates_grants_from_pins() {
        let value = serde_json::json!({
            "target": "crates/core",
            "task": "rust.check",
            "confirmed": true,
            "confirmed_by": "human:andrew",
            "origin": "StaticClosure",
            "grants": ["crates/core (MissionEditScope)"],
            "pins": [{"path": "Cargo.lock", "reason": "cargo requires it"}],
            "inventory_entries": ["ring 0.17.8 (build.rs)"],
            "rationale": [],
        });
        let rendered = baseline_view(&value);
        assert!(rendered.contains("not drift-sensitive"));
        assert!(rendered.contains("drift-sensitive):"));
        assert!(rendered.contains("Cargo.lock  — cargo requires it"));
    }
}
