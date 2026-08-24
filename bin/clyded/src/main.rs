//! `clyded`: the Clyde control plane daemon.
//!
//! Two sockets, and the difference between them is the security boundary:
//! `clyded.sock` serves actors and may be bind-mounted into a sandbox;
//! `clyded-admin.sock` serves the human and never is.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use clyde_core::audit::AuditEventKind;
use clyde_store::{SqliteStore, Store};
use clyded::daemon::{Daemon, DaemonOptions};
use clyded::{actor_api, admin_api, audit, paths::StatePaths, server};

#[derive(Debug, Parser)]
#[command(
    name = "clyded",
    about = "Clyde control plane",
    version,
    disable_help_subcommand = true
)]
struct Options {
    /// State directory. Defaults to the XDG data directory.
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Log filter, in `tracing` syntax.
    #[arg(long, env = "CLYDE_LOG", default_value = "info")]
    log: String,

    /// Check configuration and host prerequisites, then exit.
    #[arg(long)]
    check: bool,
}

fn main() -> std::process::ExitCode {
    let options = Options::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&options.log))
        // Timestamps and levels only: the daemon's log must never be a place a
        // secret could land, so structured fields are added deliberately.
        .with_target(false)
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("clyded: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(options)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clyded: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> clyded::Result<()> {
    let state_root = options
        .state_dir
        .clone()
        .unwrap_or_else(StatePaths::default_root);
    let paths = StatePaths::new(state_root.clone());
    paths
        .create_all()
        .map_err(|error| clyded::DaemonError::io("creating the state directory", error))?;

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(paths.database())?);
    let daemon = Arc::new(Daemon::new(DaemonOptions::new(state_root), store)?);

    if options.check {
        report_readiness(&daemon);
        return Ok(());
    }

    // A socket inside a registered workspace could be bind-mounted into a
    // sandbox along with the project tree, which would put the admin socket
    // inside the boundary it exists to stay outside of.
    let workspaces: Vec<PathBuf> = daemon
        .store
        .list_workspaces()?
        .into_iter()
        .map(|workspace| workspace.root)
        .collect();

    let actor_listener = server::bind(&daemon.paths.actor_socket(), 0o600, &workspaces).await?;
    let admin_listener = server::bind(&daemon.paths.admin_socket(), 0o600, &workspaces).await?;

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::DaemonStarted,
            serde_json::json!({
                "can_run_workspace": daemon.host.can_run_workspace(),
                "can_run_build": daemon.host.can_run_build(),
                "can_run_microvm": daemon.host.can_run_microvm(),
            }),
        ),
    );
    report_readiness(&daemon);
    tracing::info!(
        actor = %daemon.paths.actor_socket().display(),
        admin = %daemon.paths.admin_socket().display(),
        "clyded is listening"
    );

    let actor_daemon = Arc::clone(&daemon);
    let actor_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = actor_listener.accept().await else {
                continue;
            };
            let daemon = Arc::clone(&actor_daemon);
            tokio::spawn(async move { actor_api::serve(daemon, stream).await });
        }
    });

    let admin_daemon = Arc::clone(&daemon);
    let admin_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = admin_listener.accept().await else {
                continue;
            };
            let daemon = Arc::clone(&admin_daemon);
            tokio::spawn(async move { admin_api::serve(daemon, stream).await });
        }
    });

    // Shutdown is explicit so the audit log records it, and so sockets are
    // removed rather than left as stale files a later start has to reason about.
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("clyded is shutting down");
    actor_task.abort();
    admin_task.abort();
    audit::record(
        daemon.store.as_ref(),
        audit::draft(AuditEventKind::DaemonStopped, serde_json::Value::Null),
    );
    let _ = std::fs::remove_file(daemon.paths.actor_socket());
    let _ = std::fs::remove_file(daemon.paths.admin_socket());
    Ok(())
}

/// Reports what this host can and cannot do, at start.
///
/// Stated up front rather than discovered through a confusing failure later.
fn report_readiness(daemon: &Arc<Daemon>) {
    let host = &daemon.host;
    if !host.can_run_workspace() {
        tracing::warn!(
            detail = host.user_namespaces.detail(),
            remedy = host.user_namespaces.remedy().unwrap_or(""),
            "the workspace environment cannot start on this host"
        );
    }
    if !host.can_run_build() {
        tracing::warn!(
            detail = host.cgroup_delegation.detail(),
            remedy = host.cgroup_delegation.remedy().unwrap_or(""),
            "build and test tasks will be refused on this host (D22)"
        );
    }
    if !host.can_run_microvm() {
        tracing::info!(
            detail = host.kvm.detail(),
            "dependency resolution will be refused on this host: it requires microVM isolation"
        );
    }
    for limitation in &host.known_limitations {
        tracing::info!(limitation = limitation.as_str(), "known limitation");
    }
}
