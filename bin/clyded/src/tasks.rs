//! The task execution pipeline (Phase 2a deliverable 7).
//!
//! Admission (lease, policy, budget) → snapshot → sandbox spec → start → collect
//! outputs → classify → store artifacts → record audit events.
//!
//! Budget is charged at admission, before execution, so a daemon crash cannot
//! lose the charge.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use clyde_core::artifact::ArtifactKind;
use clyde_core::audit::AuditEventKind;
use clyde_core::baseline::BaselineKey;
use clyde_core::classification::{BackendKind, EgressProfile, TrustClass};
use clyde_core::decision::{PolicyDecision, PolicyOutcome, PolicyReason, PolicySubject};
use clyde_core::ids::{self, ArtifactId, TaskRunId};
use clyde_core::lease::Lease;
use clyde_core::mission::Mission;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::{
    TaskFailureClass, TaskOptions, TaskOutcome, TaskRequest, TaskRun, TaskRunState, TaskType,
};
use clyde_core::workspace::Workspace;
use clyde_egress::budget::EgressBudget;
use clyde_egress::proxy::{EgressContext, EgressRecorder};
use clyde_policy::config::Config;
use clyde_policy::resolve::{AdmissionInput, ApprovalState};
use clyde_snapshot::{BuiltSnapshot, ExitSummary};

use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};
use crate::{access, approvals, artifacts, audit, sandboxes};

/// Everything a task run needs about its context.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub mission: Mission,
    pub lease: Lease,
    pub workspace: Workspace,
    pub config: Config,
}

/// Admits and runs a task.
pub async fn run_task(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    task: TaskType,
    path: RepoPath,
    options: TaskOptions,
) -> Result<TaskRun> {
    // Mission and lease are re-read from the store rather than taken from the
    // caller's context. A revocation that happened after the context was built
    // must stop this request, not the next one.
    let context = &TaskContext {
        mission: daemon.store.get_mission(&context.mission.id)?,
        lease: daemon.store.get_lease(&context.lease.id)?,
        workspace: context.workspace.clone(),
        config: context.config.clone(),
    };
    let request = TaskRequest {
        id: ids::new::task_run_id()?,
        lease: context.lease.id.clone(),
        actor: context.lease.actor.clone(),
        task,
        path: path.clone(),
        options,
        requested_at: Utc::now(),
    };
    request.validate()?;

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::TaskRequested { task },
            serde_json::json!({"path": path.as_str()}),
        )
        .mission(context.mission.id.clone())
        .lease(context.lease.id.clone())
        .actor(request.actor.clone())
        .task_run(request.id.clone()),
    );

    let digest = request
        .digest()
        .map_err(|error| DaemonError::internal(error.to_string()))?;
    let approval = match approvals::find_authorising(daemon, &context.mission.id, &digest)? {
        Some(_) => ApprovalState::Present,
        None => ApprovalState::None,
    };

    let baseline_key = BaselineKey {
        workspace: context.workspace.id.clone(),
        task,
        target: path.clone(),
    };
    let baseline = access::confirmed(daemon, &baseline_key)?;

    let admission = clyde_policy::validate_action(AdmissionInput {
        task,
        path: &path,
        mission: &context.mission,
        lease: &context.lease,
        config: &context.config,
        now: Utc::now(),
        host: daemon.host_capabilities(),
        baseline_confirmed: baseline.is_some(),
        approval,
    });

    record_decision(daemon, &request, &admission)?;

    if admission.outcome == PolicyOutcome::Denied {
        audit::record(
            daemon.store.as_ref(),
            audit::draft(
                AuditEventKind::TaskDenied { task },
                serde_json::json!({
                    "reasons": admission.reasons.iter().map(PolicyReason::render).collect::<Vec<_>>(),
                }),
            )
            .mission(context.mission.id.clone())
            .lease(context.lease.id.clone())
            .task_run(request.id.clone()),
        );
        // A baseline-missing denial offers the proposal, so the operator's next
        // step is one command rather than a search.
        if admission
            .reasons
            .iter()
            .any(|reason| matches!(reason, PolicyReason::AccessBaselineMissing { .. }))
        {
            let _ = access::propose(
                daemon,
                &context.workspace.id,
                task,
                &path,
                &context.mission.scope,
            );
        }
        return Err(DaemonError::Denied(admission.reasons));
    }

    if admission.outcome == PolicyOutcome::AllowedWithApproval {
        let request_view = create_approval(daemon, context, &request, &digest, &admission)?;
        return Err(DaemonError::ApprovalRequired(request_view));
    }

    let Some(policy) = admission.policy.clone() else {
        return Err(DaemonError::internal(
            "an admitted task has no resolved policy",
        ));
    };
    let policy_digest = policy
        .digest()
        .map_err(|error| DaemonError::internal(error.to_string()))?;

    // Charged before execution: a crash must not lose the charge.
    if let Some(cost) = admission.cost {
        daemon.store.charge_lease(&context.lease.id, &cost)?;
    }

    let backend_kind = if matches!(
        policy.environment,
        clyde_core::classification::Environment::ControlPlane
    ) {
        BackendKind::ControlPlane
    } else if task == TaskType::GitPush {
        BackendKind::Broker
    } else {
        BackendKind::Bubblewrap
    };

    let run = TaskRun {
        id: request.id.clone(),
        request: request.clone(),
        policy_digest,
        snapshot: None,
        dependency_bundle: None,
        backend: backend_kind,
        state: TaskRunState::Requested,
        started_at: None,
        finished_at: None,
        outcome: None,
        artifacts: Vec::new(),
    };
    daemon.store.insert_task_run(run)?;
    daemon
        .store
        .transition_task_run(&request.id, TaskRunState::Admitted)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::TaskAdmitted { task },
            serde_json::json!({"policy_digest": policy_digest_string(daemon, &request.id)}),
        )
        .mission(context.mission.id.clone())
        .lease(context.lease.id.clone())
        .task_run(request.id.clone()),
    );

    let outcome = match policy.environment {
        clyde_core::classification::Environment::ControlPlane => {
            run_control_plane(daemon, context, &request).await
        }
        clyde_core::classification::Environment::Workspace => {
            run_workspace_activity(daemon, context, &request).await
        }
        clyde_core::classification::Environment::Build => {
            run_build(daemon, context, &request, &policy, baseline.as_ref()).await
        }
        clyde_core::classification::Environment::Broker => Err(DaemonError::invalid(
            "brokered operations are requested through request_publish, not run_task",
        )),
    };

    finish(daemon, context, &request, outcome).await
}

/// Records the decision, whether it allows or denies.
fn record_decision(
    daemon: &Arc<Daemon>,
    request: &TaskRequest,
    admission: &clyde_policy::Admission,
) -> Result<()> {
    let decision = PolicyDecision {
        id: ids::new::policy_decision_id()?,
        subject: PolicySubject::TaskRequest {
            task: request.task,
            run: Some(request.id.clone()),
        },
        outcome: admission.outcome,
        reasons: admission.reasons.clone(),
        resolved_policy_digest: admission
            .policy
            .as_ref()
            .and_then(|policy| policy.digest().ok()),
        suggested_alternative: admission.alternatives.first().cloned(),
        decided_at: Utc::now(),
    };
    daemon.store.record_policy_decision(decision)?;
    Ok(())
}

/// Creates the approval request an `AllowedWithApproval` outcome implies.
fn create_approval(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
    digest: &clyde_core::Digest,
    admission: &clyde_policy::Admission,
) -> Result<String> {
    let policy = admission
        .policy
        .clone()
        .unwrap_or_else(|| clyde_policy::builtin_policy(request.task));
    let hosts = clyde_policy::egress::resolve_allowlist(
        &policy.egress,
        &context.config.registries.allowed,
        &context.config.egress.model_api_hosts,
    );
    let created = approvals::request(
        daemon,
        &context.mission.id,
        &context.lease.id,
        &request.actor,
        approvals::ApprovalContext {
            subject: clyde_core::approval::ApprovalSubject::TaskEscalation {
                task: request.task,
                egress: policy.egress.clone(),
            },
            request_digest: digest.clone(),
            reason: format!(
                "{} on {} requires human approval",
                request.task.name(),
                request.path
            ),
            alternatives: admission.alternatives.clone(),
            prior_failure: None,
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
    Ok(format!(
        "{} requires human approval; request {} is pending on the admin channel",
        request.task.name(),
        created.id
    ))
}

/// Trusted control-plane work: commit preparation.
async fn run_control_plane(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
) -> Result<Completed> {
    match &request.options {
        TaskOptions::GitCommitPrepare { message } => {
            let identity = daemon.commit_identity.clone().ok_or_else(|| {
                DaemonError::invalid(
                    "no commit identity is configured; set CLYDE_GIT_AUTHOR_NAME and CLYDE_GIT_AUTHOR_EMAIL",
                )
            })?;
            let paths: Vec<String> = context
                .lease
                .repo_scope
                .edit_paths
                .iter()
                .map(ToString::to_string)
                .collect();
            let proposal = clyde_git::prepare_commit_proposal(
                &daemon.git,
                &context.workspace.root,
                &daemon.paths.broker_scratch(),
                message,
                &identity,
                &paths,
            )
            .await?;
            let encoded = serde_json::to_vec_pretty(&proposal)
                .map_err(|error| DaemonError::internal(error.to_string()))?;
            let artifact = artifacts::store_bytes(
                daemon.store.as_ref(),
                &daemon.paths.blobs(),
                &context.mission.id,
                ArtifactKind::CommitProposal,
                TrustClass::T1,
                Some(request.id.clone()),
                &encoded,
            )?;
            Ok(Completed {
                outcome: TaskOutcome::new(
                    Some(0),
                    TaskFailureClass::Success,
                    format!(
                        "prepared a commit proposal over {} file(s): {}",
                        proposal.files.len(),
                        proposal.stat.render()
                    ),
                ),
                artifacts: vec![artifact.id],
                snapshot: None,
                bundle: None,
            })
        }
        other => Err(DaemonError::invalid(format!(
            "{} is not a control-plane task",
            other.task().name()
        ))),
    }
}

/// Workspace activity: recorded and diffed, not sandboxed.
///
/// The agent already runs inside a workspace environment (D1), so these task
/// types describe a class of activity for audit and policy purposes rather than
/// a Clyde-launched sandbox (D17). What Clyde contributes is the record: a diff
/// computed at the boundary.
async fn run_workspace_activity(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
) -> Result<Completed> {
    let paths: Vec<String> = context
        .lease
        .repo_scope
        .edit_paths
        .iter()
        .map(ToString::to_string)
        .collect();
    let diff =
        clyde_git::diff::workspace_diff(&daemon.git, &context.workspace.root, &paths).await?;
    let mut produced = Vec::new();
    if request.task == TaskType::WorkspaceEdit && !diff.is_empty() {
        let artifact = artifacts::store_bytes(
            daemon.store.as_ref(),
            &daemon.paths.blobs(),
            &context.mission.id,
            ArtifactKind::Diff,
            TrustClass::T1,
            Some(request.id.clone()),
            diff.patch.as_bytes(),
        )?;
        produced.push(artifact.id);
    }
    let summary = match request.task {
        TaskType::WorkspaceEdit => format!("recorded edit activity: {}", diff.stat.render()),
        TaskType::WorkspaceRead => "recorded read activity".to_owned(),
        TaskType::RepoSearch => "recorded search activity".to_owned(),
        other => format!("recorded {} activity", other.name()),
    };
    Ok(Completed {
        outcome: TaskOutcome::new(Some(0), TaskFailureClass::Success, summary),
        artifacts: produced,
        snapshot: None,
        bundle: None,
    })
}

/// A finished execution, before it is recorded.
#[derive(Debug)]
struct Completed {
    outcome: TaskOutcome,
    artifacts: Vec<ArtifactId>,
    snapshot: Option<clyde_core::ids::SnapshotId>,
    bundle: Option<ArtifactId>,
}

/// Build, test, and fetch tasks.
async fn run_build(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
    policy: &clyde_core::task::TaskPolicy,
    baseline: Option<&clyde_core::baseline::AccessBaseline>,
) -> Result<Completed> {
    let Some(baseline) = baseline else {
        // Unreachable for baselined tasks, which admission refuses; a fetch has
        // no baseline and builds its own manifest-only snapshot.
        return run_fetch(daemon, context, request, policy).await;
    };

    // The bundle, and its inventory check, come before anything executes.
    let bundle = current_bundle(daemon, context)?;
    if let Some(record) = &bundle {
        let drift = access::inventory_drift(baseline, &record.inventory);
        if !drift.is_empty() {
            access::record_drift(daemon, &context.mission.id, &drift);
            // Fetching a hostile crate is harmless until something executes it,
            // and the confirmation sits in between.
            if !daemon
                .store
                .is_bundle_inventory_confirmed(&record.artifact)?
            {
                let rendered = clyde_policy::access::render_inventory_diff(&drift);
                return Err(DaemonError::Denied(vec![
                    PolicyReason::AccessBaselineDrift {
                        rendered: format!(
                            "the dependency bundle's build-time code execution changed and has not been confirmed:\n{}",
                            rendered.join("\n")
                        ),
                    },
                ]));
            }
        }
    }

    let snapshot = build_snapshot(daemon, context, baseline)?;
    daemon.store.insert_snapshot(snapshot.snapshot.clone())?;
    daemon
        .store
        .set_task_run_snapshot(&request.id, snapshot.snapshot.id.clone())?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::SnapshotCreated,
            serde_json::json!({
                "entries": snapshot.snapshot.manifest.entries.len(),
                "bytes": snapshot.snapshot.manifest.total_bytes(),
                "exclusions": snapshot.snapshot.manifest.exclusions_applied,
            }),
        )
        .mission(context.mission.id.clone())
        .task_run(request.id.clone())
        .snapshot(snapshot.snapshot.id.clone()),
    );

    let argv = cargo_argv(request, policy)?;
    execute_sandboxed(daemon, context, request, policy, &snapshot, argv, bundle).await
}

/// `rust.resolve-deps`: manifests only, with the registry egress profile.
async fn run_fetch(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
    policy: &clyde_core::task::TaskPolicy,
) -> Result<Completed> {
    // A fetch task has no reason to see application code, and narrowing the
    // input narrows what a compromised fetch can exfiltrate through an
    // allowlisted registry connection.
    let manifests = manifest_only_paths(&context.workspace.root)?;
    let snapshot_request = clyde_snapshot::SnapshotRequest {
        workspace: context.workspace.id.clone(),
        mission: context.mission.id.clone(),
        root: context.workspace.root.clone(),
        requested_path: RepoPath::root(),
        grants: Vec::new(),
        pins: manifests,
        closure_paths: Vec::new(),
        out_of_lease_paths: Vec::new(),
        configured_exclusions: context.config.snapshot.exclusions.clone(),
        max_bytes: 64 << 20,
        max_entries: 10_000,
    };
    let snapshot = clyde_snapshot::build(&daemon.content, &snapshot_request)?;
    daemon.store.insert_snapshot(snapshot.snapshot.clone())?;
    daemon
        .store
        .set_task_run_snapshot(&request.id, snapshot.snapshot.id.clone())?;

    let argv = vec![
        cargo_program(daemon, policy)?,
        "fetch".to_owned(),
        // The lockfile is authoritative and resolution cannot drift during a
        // fetch.
        "--locked".to_owned(),
    ];
    execute_sandboxed(daemon, context, request, policy, &snapshot, argv, None).await
}

/// Runs a sandboxed task and classifies the result.
async fn execute_sandboxed(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
    policy: &clyde_core::task::TaskPolicy,
    snapshot: &BuiltSnapshot,
    argv: Vec<String>,
    bundle: Option<clyde_store::BundleRecord>,
) -> Result<Completed> {
    let runtime_root = daemon
        .runtime_roots
        .get(policy.runtime_root)
        .ok_or_else(|| {
            DaemonError::invalid(format!(
                "no {} runtime root is configured; set sandbox.runtime_root_{} in host configuration",
                policy.runtime_root.name(),
                policy.runtime_root.name()
            ))
        })?;

    let log_dir = daemon.paths.task_logs(request.id.as_str());
    std::fs::create_dir_all(&log_dir)
        .map_err(|error| DaemonError::io("creating the task log directory", error))?;
    let stdout_path = log_dir.join("stdout");
    let stderr_path = log_dir.join("stderr");

    // A proxy socket exists only where the profile permits egress. Under `none`
    // it is simply not bound, which is what makes the absence structural.
    let (egress_socket, proxy, recorder) = if sandboxes::needs_egress_socket(&policy.egress) {
        let socket = daemon
            .paths
            .run()
            .join(format!("egress-{}.sock", request.id));
        let hosts = clyde_policy::egress::resolve_allowlist(
            &policy.egress,
            &context.config.registries.allowed,
            &context.config.egress.model_api_hosts,
        );
        let recorder = Arc::new(clyde_egress::MemoryRecorder::default());
        let handle = clyde_egress::bind(
            socket.clone(),
            Arc::new(EgressContext {
                profile: policy.egress.clone(),
                allowlist: hosts,
                budget: EgressBudget {
                    max_bytes: context.lease.budget.max_egress_bytes,
                    max_requests: context.lease.budget.max_egress_requests,
                    max_connections: context.lease.budget.max_egress_requests,
                },
                task_run: Some(request.id.clone()),
                auth_header: None,
                ca: None,
            }),
            Arc::clone(&recorder) as Arc<dyn EgressRecorder>,
        )
        .await?;
        (Some(socket), Some(handle), Some(recorder))
    } else {
        (None, None, None)
    };

    let cache_root = context.mission.cache_dir.clone();
    let spec = sandboxes::build_sandbox(&sandboxes::BuildSandbox {
        id: request.id.to_string(),
        policy,
        runtime_root,
        snapshot_tree: snapshot.tree.clone(),
        cache_root,
        dependency_bundle: bundle.as_ref().map(|record| record.content_ref.clone()),
        egress_socket,
        forwarder: context.config.sandbox.forwarder.clone(),
        argv,
        stdout_path: stdout_path.clone(),
        stderr_path: stderr_path.clone(),
    });

    let backend = daemon.backends.select(&spec)?;
    daemon
        .store
        .transition_task_run(&request.id, TaskRunState::Preparing)?;
    let handle = backend.start(spec).await?;
    daemon
        .store
        .transition_task_run(&request.id, TaskRunState::Running)?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::TaskStarted { task: request.task },
            serde_json::json!({"backend": backend.kind().to_string()}),
        )
        .mission(context.mission.id.clone())
        .task_run(request.id.clone()),
    );

    let status = backend.wait(&handle).await?;
    if let Some(proxy) = proxy {
        proxy.shutdown().await;
    }

    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();

    // Every attempt, allowed and refused, is recorded on the task run.
    let mut egress_denied = false;
    if let Some(recorder) = recorder {
        for attempt in recorder.attempts() {
            egress_denied |= !attempt.was_allowed();
            let kind = if attempt.was_allowed() {
                AuditEventKind::EgressAttemptAllowed
            } else {
                AuditEventKind::EgressAttemptDenied
            };
            audit::record(
                daemon.store.as_ref(),
                audit::draft(
                    kind,
                    serde_json::json!({
                        "host": attempt.host.as_str(),
                        "profile": attempt.profile,
                        "bytes_in": attempt.bytes_in,
                        "bytes_out": attempt.bytes_out,
                        "reason": attempt.denial_reason.as_ref().map(|reason| reason.render()),
                    }),
                )
                .mission(context.mission.id.clone())
                .task_run(request.id.clone()),
            );
            daemon.store.insert_egress_attempt(attempt)?;
        }
    }

    let classification = clyde_snapshot::classify(clyde_snapshot::ClassificationInput {
        exit: ExitSummary {
            code: status.code,
            timed_out: status.timed_out,
            signal: status.signal,
        },
        stdout: &stdout,
        stderr: &stderr,
        egress_denied,
        excluded: &snapshot.excluded,
    });

    if !classification.drift.is_empty() {
        access::record_drift(daemon, &context.mission.id, &classification.drift);
    }

    let mut produced = Vec::new();
    for (name, content) in [("stdout", &stdout), ("stderr", &stderr)] {
        if content.is_empty() {
            continue;
        }
        let artifact = artifacts::store_bytes(
            daemon.store.as_ref(),
            &daemon.paths.blobs(),
            &context.mission.id,
            ArtifactKind::Log,
            policy.trust_class,
            Some(request.id.clone()),
            content.as_bytes(),
        )?;
        let _ = name;
        produced.push(artifact.id);
    }

    Ok(Completed {
        outcome: TaskOutcome::new(status.code, classification.class, classification.summary),
        artifacts: produced,
        snapshot: Some(snapshot.snapshot.id.clone()),
        bundle: bundle.map(|record| record.artifact),
    })
}

/// Records the finished run.
async fn finish(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    request: &TaskRequest,
    outcome: Result<Completed>,
) -> Result<TaskRun> {
    let completed = match outcome {
        Ok(completed) => completed,
        Err(error) => {
            // A Clyde-side failure is recorded as one. It is never attributed to
            // the project's code.
            let class = match &error {
                DaemonError::Denied(_) => TaskFailureClass::PolicyDenied,
                DaemonError::Sandbox(sandbox) if sandbox.is_host_fault() => {
                    TaskFailureClass::SandboxFailure
                }
                DaemonError::Sandbox(_) => TaskFailureClass::ResourceExhausted,
                _ => TaskFailureClass::Internal,
            };
            let run = daemon.store.complete_task_run(
                &request.id,
                TaskRunState::Failed,
                TaskOutcome::new(None, class, error.to_string()),
                Utc::now(),
                Vec::new(),
            )?;
            audit::record(
                daemon.store.as_ref(),
                audit::draft(
                    AuditEventKind::TaskFinished { task: request.task },
                    serde_json::json!({"classification": class.name()}),
                )
                .mission(context.mission.id.clone())
                .task_run(request.id.clone()),
            );
            let _ = run;
            return Err(error);
        }
    };

    if let Some(bundle) = completed.bundle {
        daemon.store.set_task_run_bundle(&request.id, bundle)?;
    }
    let state = if completed.outcome.classification.is_success() {
        TaskRunState::Succeeded
    } else if completed.outcome.classification == TaskFailureClass::ResourceExhausted {
        TaskRunState::TimedOut
    } else {
        TaskRunState::Failed
    };
    let run = daemon.store.complete_task_run(
        &request.id,
        state,
        completed.outcome.clone(),
        Utc::now(),
        completed.artifacts,
    )?;
    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::TaskFinished { task: request.task },
            serde_json::json!({
                "classification": completed.outcome.classification.name(),
                "exit_code": completed.outcome.exit_code,
            }),
        )
        .mission(context.mission.id.clone())
        .task_run(request.id.clone())
        .snapshot(completed.snapshot.unwrap_or_else(|| {
            // A control-plane task has no snapshot; the field stays absent by
            // using the run's own recorded value instead.
            run.snapshot.clone().unwrap_or_else(|| {
                clyde_core::ids::SnapshotId::parse(format!("s-blake3:{}", "0".repeat(64)))
                    .unwrap_or_else(|_| unreachable_snapshot())
            })
        })),
    );
    Ok(run)
}

/// A placeholder identifier for the unreachable branch above.
fn unreachable_snapshot() -> clyde_core::ids::SnapshotId {
    match clyde_core::ids::SnapshotId::parse(format!("s-blake3:{}", "0".repeat(64))) {
        Ok(id) => id,
        Err(_) => unreachable_snapshot(),
    }
}

/// Builds the snapshot a baseline implies.
fn build_snapshot(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
    baseline: &clyde_core::baseline::AccessBaseline,
) -> Result<BuiltSnapshot> {
    let request = access::snapshot_request(
        daemon,
        &context.mission.id,
        baseline,
        context.config.snapshot.exclusions.clone(),
    )?;
    Ok(clyde_snapshot::build(&daemon.content, &request)?)
}

/// The dependency bundle a build should use.
fn current_bundle(
    daemon: &Arc<Daemon>,
    context: &TaskContext,
) -> Result<Option<clyde_store::BundleRecord>> {
    let lockfile = context.workspace.root.join("Cargo.lock");
    if !lockfile.exists() {
        return Ok(None);
    }
    let parsed = clyde_snapshot::cargo::lockfile::read(&lockfile)?;
    Ok(daemon.store.find_bundle_for_lockfile(&parsed.digest)?)
}

/// Manifest and lockfile paths, for a fetch snapshot.
fn manifest_only_paths(root: &std::path::Path) -> Result<Vec<RepoPath>> {
    let mut paths = Vec::new();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config.toml",
        "rust-toolchain.toml",
    ] {
        if root.join(name).exists()
            && let Ok(path) = RepoPath::parse(name)
        {
            paths.push(path);
        }
    }
    // Member manifests, so cargo can construct the workspace graph.
    let closure = clyde_snapshot::compute_closure(root, &RepoPath::root())?;
    paths.extend(closure.member_manifests);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The cargo argv for a build or test task.
fn cargo_argv(request: &TaskRequest, policy: &clyde_core::task::TaskPolicy) -> Result<Vec<String>> {
    let program = format!("{}/bin/cargo", "/nix/store/placeholder");
    let _ = program;
    let mut argv = vec!["cargo".to_owned()];
    match &request.options {
        TaskOptions::RustCheck {
            package,
            all_targets,
        } => {
            argv.push("check".to_owned());
            // `--frozen` implies `--locked --offline`: the lockfile is
            // authoritative and a missing dependency fails rather than fetching.
            argv.push("--frozen".to_owned());
            argv.push("--message-format=json".to_owned());
            if let Some(package) = package {
                argv.push("-p".to_owned());
                argv.push(package.clone());
            }
            if *all_targets {
                argv.push("--all-targets".to_owned());
            }
        }
        TaskOptions::RustTestUnit { package, filter } => {
            argv.push("test".to_owned());
            argv.push("--frozen".to_owned());
            argv.push("--lib".to_owned());
            argv.push("--bins".to_owned());
            if let Some(package) = package {
                argv.push("-p".to_owned());
                argv.push(package.clone());
            }
            if let Some(filter) = filter {
                argv.push("--".to_owned());
                argv.push(filter.clone());
            }
        }
        other => {
            return Err(DaemonError::invalid(format!(
                "{} is not a build task",
                other.task().name()
            )));
        }
    }
    let _ = policy;
    Ok(argv)
}

/// The cargo program inside the runtime root.
fn cargo_program(daemon: &Arc<Daemon>, policy: &clyde_core::task::TaskPolicy) -> Result<String> {
    let root = daemon
        .runtime_roots
        .get(policy.runtime_root)
        .ok_or_else(|| DaemonError::invalid("no runtime root is configured for this task"))?;
    Ok(root
        .program("cargo")
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "cargo".to_owned()))
}

fn policy_digest_string(daemon: &Arc<Daemon>, id: &TaskRunId) -> String {
    daemon
        .store
        .get_task_run(id)
        .map(|run| run.policy_digest.to_string())
        .unwrap_or_default()
}

/// Reads a task's logs.
pub fn read_logs(
    daemon: &Arc<Daemon>,
    id: &TaskRunId,
    stream: &str,
    offset: u64,
) -> Result<(String, bool)> {
    let path: PathBuf = daemon.paths.task_logs(id.as_str()).join(match stream {
        "stdout" => "stdout",
        _ => "stderr",
    });
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let start = usize::try_from(offset).unwrap_or(0).min(content.len());
    let mut cut = start;
    while cut < content.len() && !content.is_char_boundary(cut) {
        cut += 1;
    }
    let slice = content.get(cut..).unwrap_or("").to_owned();
    let truncated = slice.len() >= artifacts::MAX_ARTIFACT_BYTES;
    Ok((slice, truncated))
}

/// Which egress profiles a mission's tasks would use, for the envelope.
pub fn egress_profiles_for(tasks: &BTreeSet<TaskType>) -> Vec<EgressProfile> {
    let mut profiles: Vec<EgressProfile> = tasks
        .iter()
        .map(|task| clyde_policy::builtin_policy(*task).egress)
        .collect();
    profiles.dedup();
    profiles
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

    fn request(options: TaskOptions, task: TaskType) -> TaskRequest {
        TaskRequest {
            id: ids::new::task_run_id().unwrap(),
            lease: ids::new::lease_id().unwrap(),
            actor: clyde_core::ids::ActorId::parse("agent:claude").unwrap(),
            task,
            path: RepoPath::parse("crates/core").unwrap(),
            options,
            requested_at: Utc::now(),
        }
    }

    #[test]
    fn a_check_runs_frozen_so_a_missing_dependency_fails_rather_than_fetching() {
        let policy = clyde_policy::builtin_policy(TaskType::RustCheck);
        let argv = cargo_argv(
            &request(
                TaskOptions::RustCheck {
                    package: Some("clyde-core".to_owned()),
                    all_targets: true,
                },
                TaskType::RustCheck,
            ),
            &policy,
        )
        .unwrap();
        assert_eq!(argv[0], "cargo");
        assert!(argv.contains(&"check".to_owned()));
        assert!(
            argv.contains(&"--frozen".to_owned()),
            "--frozen implies --locked --offline"
        );
        assert!(
            argv.contains(&"--message-format=json".to_owned()),
            "classification matches on diagnostic codes, not rendered text"
        );
        assert!(argv.contains(&"clyde-core".to_owned()));
        assert!(argv.contains(&"--all-targets".to_owned()));
    }

    #[test]
    fn a_unit_test_run_is_scoped_to_lib_and_bin_targets() {
        let policy = clyde_policy::builtin_policy(TaskType::RustTestUnit);
        let argv = cargo_argv(
            &request(
                TaskOptions::RustTestUnit {
                    package: None,
                    filter: Some("parses".to_owned()),
                },
                TaskType::RustTestUnit,
            ),
            &policy,
        )
        .unwrap();
        assert!(argv.contains(&"test".to_owned()));
        assert!(argv.contains(&"--lib".to_owned()));
        assert!(argv.contains(&"--frozen".to_owned()));
        let separator = argv.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(argv[separator + 1], "parses");
    }

    #[test]
    fn a_non_build_task_has_no_cargo_argv() {
        let policy = clyde_policy::builtin_policy(TaskType::GitCommitPrepare);
        assert!(
            cargo_argv(
                &request(
                    TaskOptions::GitCommitPrepare {
                        message: "x".to_owned()
                    },
                    TaskType::GitCommitPrepare
                ),
                &policy
            )
            .is_err()
        );
    }

    #[test]
    fn egress_profiles_report_what_a_mission_would_reach() {
        let offline: BTreeSet<TaskType> = [TaskType::RustCheck, TaskType::RustTestUnit]
            .into_iter()
            .collect();
        assert!(
            egress_profiles_for(&offline)
                .iter()
                .all(EgressProfile::is_none)
        );
        let fetching: BTreeSet<TaskType> = [TaskType::RustResolveDeps].into_iter().collect();
        assert_eq!(
            egress_profiles_for(&fetching),
            vec![EgressProfile::RustRegistry]
        );
    }
}
