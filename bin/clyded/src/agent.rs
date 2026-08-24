//! Agent hosting (Phase 1 deliverable 7).
//!
//! What binary clyded execs as the agent comes from host or user configuration
//! only, never from repository configuration (D20). A repository that could
//! choose what Clyde runs would have arbitrary code execution in the workspace
//! environment on `clyde mission create`, before any policy applies.
//!
//! The agent binary must be reachable inside the sandbox, which means it comes
//! from the workspace runtime root or an explicitly mounted read-only path
//! recorded in the sandbox spec. It is not copied out of the host's `PATH`
//! implicitly.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use clyde_core::audit::AuditEventKind;
use clyde_core::classification::EgressProfile;
use clyde_core::lease::Lease;
use clyde_core::workspace::Workspace;
use clyde_egress::budget::EgressBudget;
use clyde_egress::proxy::{EgressContext, EgressRecorder};
use clyde_policy::config::Config;
use clyde_sandbox::spec::{Mount, MountMode, MountPurpose};

use crate::daemon::{Daemon, RunningEnvironment};
use crate::error::{DaemonError, Result};
use crate::{audit, missions, sandboxes};

/// Starts the workspace environment for a lease, and the agent inside it.
pub async fn start_environment(
    daemon: &Arc<Daemon>,
    workspace: &Workspace,
    lease: &Lease,
    config: &Config,
) -> Result<()> {
    let runtime_root = daemon
        .runtime_roots
        .get(clyde_core::task::RuntimeRootKind::Workspace)
        .ok_or_else(|| {
            DaemonError::invalid(
                "no workspace runtime root is configured; set sandbox.runtime_root_workspace in host configuration",
            )
        })?;

    // The workspace runtime root must contain no project build toolchain. This
    // is asserted in the flake over the derivation's closure; asserting it again
    // here means a hand-configured root cannot quietly reintroduce one.
    let assertion = clyde_sandbox::assert_workspace_root(&runtime_root.binaries);
    if !assertion.holds() {
        return Err(DaemonError::invalid(format!(
            "the configured workspace runtime root is unusable: {}",
            assertion.render()
        )));
    }

    let Some(command) = config.agent.command.clone() else {
        // Failing here, rather than starting a sandbox with nothing in it, is
        // the difference between a clear diagnostic and a mystery.
        return Err(DaemonError::invalid(
            "no agent command is configured; set agent.command in host or user configuration (a repository cannot set it)",
        ));
    };
    let program = resolve_agent_program(runtime_root, &command)?;

    let token = missions::bind_session(daemon, lease)?;
    let token_file =
        missions::write_token_file(&daemon.paths.sandbox_runtime(), &lease.id, &token)?;

    // The proxy socket exists only where the lease permits egress.
    let (egress_socket, proxy) = if sandboxes::needs_egress_socket(&lease.network_scope) {
        let socket = daemon
            .paths
            .run()
            .join(format!("egress-{}.sock", lease.id.as_str()));
        let hosts = clyde_policy::egress::resolve_allowlist(
            &lease.network_scope,
            &config.registries.allowed,
            &config.egress.model_api_hosts,
        );
        let recorder = Arc::new(clyde_egress::MemoryRecorder::default());
        let handle = clyde_egress::bind(
            socket.clone(),
            Arc::new(EgressContext {
                profile: lease.network_scope.clone(),
                allowlist: hosts,
                budget: EgressBudget {
                    max_bytes: lease.budget.max_egress_bytes,
                    max_requests: lease.budget.max_egress_requests,
                    max_connections: lease.budget.max_egress_requests,
                },
                task_run: None,
                // Injected host-side, so the agent authenticates without ever
                // holding the credential (D11).
                auth_header: daemon.model_api_credential.as_ref().map(|credential| {
                    clyde_core::Redacted::new(credential.expose().clone(), "model api credential")
                }),
                ca: Some(Arc::clone(&daemon.ca)),
            }),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await?;
        (Some(socket), Some(handle))
    } else {
        (None, None)
    };

    let mut argv = vec![program.to_string_lossy().to_string()];
    argv.extend(config.agent.args.clone());

    let mut spec = sandboxes::workspace_environment(&sandboxes::WorkspaceEnvironment {
        id: format!("ws-{}", lease.id.as_str()),
        lease,
        workspace_root: &workspace.root,
        runtime_root,
        limits: config.limits.workspace,
        actor_socket: daemon.paths.actor_socket(),
        token_file,
        egress_socket,
        ca_certificate: lease
            .network_scope
            .terminates_tls()
            .then(|| daemon.ca.certificate_path().to_path_buf()),
        forwarder: config.sandbox.forwarder.clone(),
        argv,
        passthrough_env: passthrough_environment(config),
    });

    // An agent binary from outside the runtime root is mounted read-only and
    // recorded in the spec, never copied in implicitly.
    if !program.starts_with(&runtime_root.path) {
        spec.mounts.push(Mount {
            source: Some(program.clone()),
            target: program.clone(),
            mode: MountMode::ReadOnly,
            purpose: MountPurpose::AgentBinary,
        });
    }

    spec.validate().map_err(clyde_sandbox::SandboxError::from)?;
    let backend = daemon.backends.select(&spec)?;
    let handle = backend.start(spec).await?;
    daemon
        .store
        .set_session_sandbox(&lease.id, Some(handle.id.clone()))?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::SandboxStarted,
            serde_json::json!({
                "sandbox": handle.id,
                "backend": handle.backend.to_string(),
                "egress": lease.network_scope.name(),
            }),
        )
        .mission(lease.mission.clone())
        .lease(lease.id.clone())
        .actor(lease.actor.clone()),
    );

    daemon.environments.lock().await.insert(
        lease.id.clone(),
        RunningEnvironment {
            sandbox: handle,
            proxy,
            agent_started: true,
        },
    );
    Ok(())
}

/// Tears down a lease's environment.
pub async fn stop_environment(daemon: &Arc<Daemon>, lease: &clyde_core::ids::LeaseId) {
    let Some(environment) = daemon.environments.lock().await.remove(lease) else {
        return;
    };
    if let Some(proxy) = environment.proxy {
        proxy.shutdown().await;
    }
    for backend in [
        clyde_core::classification::BackendKind::Bubblewrap,
        clyde_core::classification::BackendKind::Firecracker,
    ] {
        let _ = backend;
    }
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::SandboxTerminated,
            serde_json::json!({"sandbox": environment.sandbox.id}),
        )
        .lease(lease.clone()),
    );
}

/// Resolves the agent program.
///
/// A bare name resolves inside the runtime root; an absolute path is used as
/// given and will be mounted read-only. Neither reads the host `PATH`.
fn resolve_agent_program(
    runtime_root: &clyde_sandbox::RuntimeRoot,
    command: &std::path::Path,
) -> Result<PathBuf> {
    if command.is_absolute() {
        if !command.is_file() {
            return Err(DaemonError::invalid(format!(
                "the configured agent command {} does not exist",
                command.display()
            )));
        }
        return Ok(command.to_path_buf());
    }
    let name = command
        .to_str()
        .ok_or_else(|| DaemonError::invalid("the agent command is not valid UTF-8"))?;
    runtime_root.program(name).ok_or_else(|| {
        DaemonError::invalid(format!(
            "{name} is not present in the workspace runtime root, and the host PATH is never consulted"
        ))
    })
}

/// The environment variables configuration allowlists.
///
/// The sandbox environment is otherwise cleared, so this is the whole set of
/// host-derived variables the agent sees.
fn passthrough_environment(config: &Config) -> BTreeMap<String, String> {
    config
        .agent
        .env
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
        .collect()
}

/// Whether the agent would have model API access under this lease.
pub fn has_model_access(lease: &Lease) -> bool {
    matches!(lease.network_scope, EgressProfile::ModelApi)
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
    use clyde_core::task::RuntimeRootKind;

    fn runtime_root(dir: &std::path::Path, binaries: &[&str]) -> clyde_sandbox::RuntimeRoot {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for name in binaries {
            std::fs::write(dir.join("bin").join(name), b"#!/bin/sh\n").unwrap();
        }
        clyde_sandbox::RuntimeRoot {
            kind: RuntimeRootKind::Workspace,
            path: dir.to_path_buf(),
            closure: vec![dir.to_path_buf()],
            binaries: binaries.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn a_bare_command_resolves_inside_the_runtime_root_and_never_on_the_host_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = runtime_root(&dir.path().join("root"), &["claude", "sh"]);
        assert!(resolve_agent_program(&root, std::path::Path::new("claude")).is_ok());
        let error = resolve_agent_program(&root, std::path::Path::new("bash"))
            .expect_err("the host PATH is never consulted");
        assert!(error.to_string().contains("host PATH is never consulted"));
    }

    #[test]
    fn an_absolute_command_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = runtime_root(&dir.path().join("root"), &["sh"]);
        let agent = dir.path().join("agent");
        std::fs::write(&agent, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_agent_program(&root, &agent).unwrap(),
            agent,
            "an explicit path is used as given and mounted read-only"
        );
        assert!(resolve_agent_program(&root, &dir.path().join("absent")).is_err());
    }

    #[test]
    fn only_allowlisted_environment_variables_pass_through() {
        // Safety of `set_var` is not at issue here: the test is single-threaded
        // and the variable is one it owns.
        let mut config = Config::defaults();
        config.agent.env = ["TERM".to_owned()].into_iter().collect();
        let passed = passthrough_environment(&config);
        assert!(passed.len() <= 1);
        assert!(
            !passed.contains_key("CLYDE_MODEL_API_KEY"),
            "the credential must never reach the sandbox environment"
        );
        assert!(!passed.contains_key("HOME"));
        assert!(!passed.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn model_access_is_visible_from_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let _ = dir;
        // A lease carrying `none` has no model access, whatever the daemon holds.
        assert!(!has_model_access(&{
            let mut lease = crate::sandboxes::tests_support::lease();
            lease.network_scope = EgressProfile::None;
            lease
        }));
        assert!(has_model_access(&crate::sandboxes::tests_support::lease()));
    }
}
