//! Task types, policies, requests, runs, and outcome classification.
//!
//! The MVP catalog is closed and task types are enum variants rather than
//! strings, so an unknown task type is unrepresentable rather than a runtime
//! lookup failure (schema reference: task type and policy).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::classification::{
    ApprovalRequirement, AuditLevel, BackendKind, CachePolicy, CredentialPolicy, EgressProfile,
    Environment, InputSpec, IsolationLevel, OutputSpec, ResourceLimits, TrustClass,
};
use crate::error::{TransitionError, ValidationError};
use crate::ids::{ActorId, ArtifactId, LeaseId, SnapshotId, TaskRunId};
use crate::repo_path::RepoPath;

/// The closed MVP task catalog (Phase 0 deliverable 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskType {
    #[serde(rename = "workspace.read")]
    WorkspaceRead,
    #[serde(rename = "workspace.edit")]
    WorkspaceEdit,
    #[serde(rename = "repo.search")]
    RepoSearch,
    #[serde(rename = "rust.check")]
    RustCheck,
    #[serde(rename = "rust.test.unit")]
    RustTestUnit,
    #[serde(rename = "rust.resolve-deps")]
    RustResolveDeps,
    #[serde(rename = "git.commit.prepare")]
    GitCommitPrepare,
    #[serde(rename = "git.push")]
    GitPush,
}

impl TaskType {
    /// Every task type, for catalog iteration and exhaustiveness tests.
    pub const ALL: [TaskType; 8] = [
        Self::WorkspaceRead,
        Self::WorkspaceEdit,
        Self::RepoSearch,
        Self::RustCheck,
        Self::RustTestUnit,
        Self::RustResolveDeps,
        Self::GitCommitPrepare,
        Self::GitPush,
    ];

    /// The wire name, identical to the serde encoding and to what a human types.
    pub fn name(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace.read",
            Self::WorkspaceEdit => "workspace.edit",
            Self::RepoSearch => "repo.search",
            Self::RustCheck => "rust.check",
            Self::RustTestUnit => "rust.test.unit",
            Self::RustResolveDeps => "rust.resolve-deps",
            Self::GitCommitPrepare => "git.commit.prepare",
            Self::GitPush => "git.push",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        Self::ALL
            .into_iter()
            .find(|task| task.name() == value)
            .ok_or_else(|| ValidationError::MalformedId {
                kind: "task",
                value: value.to_owned(),
                expected: "one of the MVP catalog task types",
            })
    }
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A resolved task policy: what actually governs a run.
///
/// Built-in policies are typed Rust values; `.clyde/policy.toml` may only narrow
/// them (D14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPolicy {
    pub task: TaskType,
    pub environment: Environment,
    pub trust_class: TrustClass,
    pub min_isolation: IsolationLevel,
    pub input: InputSpec,
    pub outputs: Vec<OutputSpec>,
    pub egress: EgressProfile,
    pub credentials: CredentialPolicy,
    pub cache: CachePolicy,
    pub limits: ResourceLimits,
    pub approval: ApprovalRequirement,
    pub audit_level: AuditLevel,
    /// Whether a confirmed access baseline is required before the task may run
    /// (D18). A task with no baseline for its target is refused.
    pub requires_access_baseline: bool,
    /// Which runtime root the task executes against. Repository configuration
    /// can never influence this (D20).
    pub runtime_root: RuntimeRootKind,
}

/// The named runtime roots (D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRootKind {
    /// Text and code manipulation. No project build toolchain, by assertion.
    Workspace,
    /// The Rust toolchain for check/test.
    Rust,
    /// Cargo plus network client tooling, for dependency resolution.
    Fetch,
    /// No runtime root: the operation runs in the control plane or the broker.
    None,
}

impl RuntimeRootKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Rust => "rust",
            Self::Fetch => "fetch",
            Self::None => "none",
        }
    }
}

impl TaskPolicy {
    /// Digest of the policy actually applied, recorded on the task run so
    /// "which policy ran" is answerable from the result rather than inferred
    /// (Phase 2a deliverable 8).
    pub fn digest(&self) -> Result<crate::digest::Digest, crate::digest::CanonicalError> {
        crate::digest::Digest::of_canonical("clyde.task-policy.v1", self)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.limits.validate()?;
        Ok(())
    }
}

/// Task-specific options, validated per type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "task", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskOptions {
    #[serde(rename = "workspace.read")]
    WorkspaceRead { max_bytes: Option<u64> },
    #[serde(rename = "workspace.edit")]
    WorkspaceEdit { summary: String },
    #[serde(rename = "repo.search")]
    RepoSearch { pattern: String },
    #[serde(rename = "rust.check")]
    RustCheck {
        package: Option<String>,
        all_targets: bool,
    },
    #[serde(rename = "rust.test.unit")]
    RustTestUnit {
        package: Option<String>,
        filter: Option<String>,
    },
    #[serde(rename = "rust.resolve-deps")]
    RustResolveDeps { lockfile_digest: String },
    #[serde(rename = "git.commit.prepare")]
    GitCommitPrepare { message: String },
    #[serde(rename = "git.push")]
    GitPush {
        remote: String,
        refspec: String,
        commit: String,
    },
}

const MAX_OPTION_TEXT: usize = 4096;

impl TaskOptions {
    /// The task type these options belong to.
    pub fn task(&self) -> TaskType {
        match self {
            Self::WorkspaceRead { .. } => TaskType::WorkspaceRead,
            Self::WorkspaceEdit { .. } => TaskType::WorkspaceEdit,
            Self::RepoSearch { .. } => TaskType::RepoSearch,
            Self::RustCheck { .. } => TaskType::RustCheck,
            Self::RustTestUnit { .. } => TaskType::RustTestUnit,
            Self::RustResolveDeps { .. } => TaskType::RustResolveDeps,
            Self::GitCommitPrepare { .. } => TaskType::GitCommitPrepare,
            Self::GitPush { .. } => TaskType::GitPush,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        let bounded = |field: &'static str, text: &str| -> Result<(), ValidationError> {
            if text.trim().is_empty() {
                return Err(ValidationError::EmptyField { field });
            }
            if text.len() > MAX_OPTION_TEXT {
                return Err(ValidationError::FieldTooLong {
                    field,
                    max: MAX_OPTION_TEXT,
                });
            }
            Ok(())
        };
        // Cargo package and target names are passed to a build tool as
        // arguments, so they are constrained to a conservative character set
        // rather than trusted (AGENTS.md: treat all external input as untrusted).
        let identifier = |field: &'static str, text: &str| -> Result<(), ValidationError> {
            bounded(field, text)?;
            if !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(ValidationError::InvalidRepoPathComponent {
                    path: text.to_owned(),
                    reason: "is not a valid cargo identifier",
                });
            }
            Ok(())
        };
        match self {
            Self::WorkspaceRead { .. } => Ok(()),
            Self::WorkspaceEdit { summary } => bounded("summary", summary),
            Self::RepoSearch { pattern } => bounded("pattern", pattern),
            Self::RustCheck { package, .. } => match package {
                Some(package) => identifier("package", package),
                None => Ok(()),
            },
            Self::RustTestUnit { package, filter } => {
                if let Some(package) = package {
                    identifier("package", package)?;
                }
                match filter {
                    Some(filter) => bounded("filter", filter),
                    None => Ok(()),
                }
            }
            Self::RustResolveDeps { lockfile_digest } => {
                crate::digest::Digest::parse(lockfile_digest.clone()).map(|_| ())
            }
            Self::GitCommitPrepare { message } => bounded("message", message),
            Self::GitPush {
                remote,
                refspec,
                commit,
            } => {
                identifier("remote", remote)?;
                bounded("refspec", refspec)?;
                validate_git_object_id(commit)
            }
        }
    }
}

/// A git object identifier: 40 or 64 lowercase hex characters.
///
/// Validated here rather than at the broker so a malformed value cannot reach a
/// git invocation at all.
pub fn validate_git_object_id(value: &str) -> Result<(), ValidationError> {
    let well_formed = matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if well_formed {
        Ok(())
    } else {
        Err(ValidationError::MalformedDigest {
            value: value.to_owned(),
        })
    }
}

/// A request to run a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub id: TaskRunId,
    pub lease: LeaseId,
    pub actor: ActorId,
    pub task: TaskType,
    /// The build target or the path the task applies to.
    pub path: RepoPath,
    pub options: TaskOptions,
    pub requested_at: DateTime<Utc>,
}

impl TaskRequest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.options.task() != self.task {
            return Err(ValidationError::MalformedId {
                kind: "task",
                value: self.options.task().name().to_owned(),
                expected: "options matching the requested task type",
            });
        }
        self.options.validate()
    }

    /// Digest over the normalised request, used to bind an approval to exactly
    /// this operation (schema reference: Approval invariants).
    pub fn digest(&self) -> Result<crate::digest::Digest, crate::digest::CanonicalError> {
        // The identifier and timestamp are excluded so that the same logical
        // request digests identically, and a *different* request cannot reuse an
        // approval.
        let normalised = serde_json::json!({
            "lease": self.lease.as_str(),
            "actor": self.actor.as_str(),
            "task": self.task.name(),
            "path": self.path.as_str(),
            "options": self.options,
        });
        crate::digest::Digest::of_canonical("clyde.task-request.v1", &normalised)
    }
}

/// Task run state (schema reference: task run states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunState {
    Requested,
    Denied,
    Admitted,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for TaskRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Requested => "requested",
            Self::Denied => "denied",
            Self::Admitted => "admitted",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        };
        f.write_str(text)
    }
}

impl TaskRunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Denied | Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }

    pub fn transition(self, to: TaskRunState) -> Result<TaskRunState, TransitionError> {
        if self.is_terminal() {
            return Err(TransitionError::terminal("task run", self, to));
        }
        let permitted = match (self, to) {
            (Self::Requested, Self::Denied | Self::Admitted) => true,
            (Self::Admitted, Self::Preparing | Self::Cancelled | Self::Failed) => true,
            (Self::Preparing, Self::Running | Self::Failed | Self::Cancelled) => true,
            (Self::Running, Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut) => {
                true
            }
            _ => false,
        };
        if permitted {
            Ok(to)
        } else {
            Err(TransitionError::not_permitted("task run", self, to))
        }
    }
}

/// Structured failure classification (Phase 2a deliverable 6).
///
/// `SandboxFailure` and `Internal` are deliberately distinguished from project
/// errors so "Clyde is broken" is never reported as "your code is broken".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureClass {
    Success,
    /// Compilation or test failure in project code.
    ProjectCodeError,
    /// Cargo cannot proceed offline with the present cache. Drives Phase 3.
    MissingDependencies,
    /// Blocked by lease or policy before execution.
    PolicyDenied,
    /// The proxy denied a destination, or a `none`-profile task tried to connect.
    EgressBlocked,
    /// A read outside the confirmed access baseline (D18).
    AccessBaselineDrift,
    /// The build tried to read `.git`, which is never available (D21).
    GitMetadataUnavailable,
    ResourceExhausted,
    /// Backend or host problem, not the project's fault.
    SandboxFailure,
    Internal,
}

impl TaskFailureClass {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// Whether the class describes a problem with the user's code.
    ///
    /// Used by the CLI and the actor surface to decide how to phrase a result:
    /// everything else is a Clyde or policy condition.
    pub fn is_users_code(self) -> bool {
        matches!(self, Self::ProjectCodeError)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::ProjectCodeError => "project_code_error",
            Self::MissingDependencies => "missing_dependencies",
            Self::PolicyDenied => "policy_denied",
            Self::EgressBlocked => "egress_blocked",
            Self::AccessBaselineDrift => "access_baseline_drift",
            Self::GitMetadataUnavailable => "git_metadata_unavailable",
            Self::ResourceExhausted => "resource_exhausted",
            Self::SandboxFailure => "sandbox_failure",
            Self::Internal => "internal",
        }
    }
}

impl std::fmt::Display for TaskFailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

const MAX_SUMMARY: usize = 4096;

/// The outcome of a task run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub exit_code: Option<i32>,
    pub classification: TaskFailureClass,
    /// Redaction-safe and bounded. Built by the classifier, never raw output.
    pub summary: String,
}

impl TaskOutcome {
    /// Builds an outcome, truncating an over-long summary rather than rejecting
    /// it: a task result must always be reportable.
    pub fn new(
        exit_code: Option<i32>,
        classification: TaskFailureClass,
        summary: impl Into<String>,
    ) -> Self {
        let mut summary: String = summary.into();
        if summary.len() > MAX_SUMMARY {
            let mut cut = MAX_SUMMARY;
            while cut > 0 && !summary.is_char_boundary(cut) {
                cut -= 1;
            }
            summary.truncate(cut);
            summary.push_str("… [truncated]");
        }
        Self {
            exit_code,
            classification,
            summary,
        }
    }
}

/// A task run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: TaskRunId,
    pub request: TaskRequest,
    /// Digest of the resolved policy actually applied.
    pub policy_digest: crate::digest::Digest,
    pub snapshot: Option<SnapshotId>,
    pub dependency_bundle: Option<ArtifactId>,
    pub backend: BackendKind,
    pub state: TaskRunState,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: Option<TaskOutcome>,
    pub artifacts: Vec<ArtifactId>,
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
    fn task_names_round_trip_through_serde_and_parse() {
        for task in TaskType::ALL {
            let json = serde_json::to_string(&task).unwrap();
            assert_eq!(json, format!("\"{}\"", task.name()));
            assert_eq!(TaskType::parse(task.name()).unwrap(), task);
            let back: TaskType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, task);
        }
    }

    #[test]
    fn unknown_task_names_are_rejected() {
        assert!(TaskType::parse("rust.build").is_err());
        assert!(serde_json::from_str::<TaskType>("\"shell.exec\"").is_err());
    }

    #[test]
    fn options_must_match_the_requested_task() {
        let request = TaskRequest {
            id: crate::ids::new::task_run_id().unwrap(),
            lease: crate::ids::new::lease_id().unwrap(),
            actor: crate::ids::ActorId::parse("agent:claude").unwrap(),
            task: TaskType::RustCheck,
            path: RepoPath::parse("crates/core").unwrap(),
            options: TaskOptions::RustTestUnit {
                package: None,
                filter: None,
            },
            requested_at: Utc::now(),
        };
        assert!(
            request.validate().is_err(),
            "mismatched options must be rejected"
        );
    }

    #[test]
    fn cargo_identifiers_are_constrained() {
        let bad = TaskOptions::RustCheck {
            package: Some("core; rm -rf /".to_owned()),
            all_targets: false,
        };
        assert!(bad.validate().is_err());
        let good = TaskOptions::RustCheck {
            package: Some("clyde-core".to_owned()),
            all_targets: true,
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn git_object_ids_are_validated() {
        assert!(validate_git_object_id(&"a".repeat(40)).is_ok());
        assert!(validate_git_object_id(&"a".repeat(64)).is_ok());
        assert!(validate_git_object_id("HEAD").is_err());
        assert!(validate_git_object_id(&"A".repeat(40)).is_err());
        assert!(validate_git_object_id("../../etc").is_err());
    }

    #[test]
    fn request_digest_ignores_id_and_timestamp_but_not_content() {
        let base = TaskRequest {
            id: crate::ids::new::task_run_id().unwrap(),
            lease: crate::ids::new::lease_id().unwrap(),
            actor: crate::ids::ActorId::parse("agent:claude").unwrap(),
            task: TaskType::GitPush,
            path: RepoPath::root(),
            options: TaskOptions::GitPush {
                remote: "origin".to_owned(),
                refspec: "refs/heads/feature".to_owned(),
                commit: "a".repeat(40),
            },
            requested_at: Utc::now(),
        };
        let other_id = TaskRequest {
            id: crate::ids::new::task_run_id().unwrap(),
            requested_at: Utc::now() + chrono::Duration::seconds(5),
            ..base.clone()
        };
        assert_eq!(base.digest().unwrap(), other_id.digest().unwrap());

        let altered_refspec = TaskRequest {
            options: TaskOptions::GitPush {
                remote: "origin".to_owned(),
                refspec: "refs/heads/main".to_owned(),
                commit: "a".repeat(40),
            },
            ..base.clone()
        };
        assert_ne!(base.digest().unwrap(), altered_refspec.digest().unwrap());
    }

    #[test]
    fn task_run_state_machine() {
        use TaskRunState::*;
        assert_eq!(Requested.transition(Admitted).unwrap(), Admitted);
        assert_eq!(Admitted.transition(Preparing).unwrap(), Preparing);
        assert_eq!(Preparing.transition(Running).unwrap(), Running);
        assert_eq!(Running.transition(Succeeded).unwrap(), Succeeded);
        assert!(Requested.transition(Running).is_err());
        assert!(Succeeded.transition(Failed).is_err());
        assert!(Denied.transition(Admitted).is_err());
    }

    #[test]
    fn failure_classes_separate_clyde_faults_from_user_faults() {
        assert!(TaskFailureClass::ProjectCodeError.is_users_code());
        for class in [
            TaskFailureClass::SandboxFailure,
            TaskFailureClass::Internal,
            TaskFailureClass::MissingDependencies,
            TaskFailureClass::PolicyDenied,
            TaskFailureClass::EgressBlocked,
            TaskFailureClass::GitMetadataUnavailable,
            TaskFailureClass::ResourceExhausted,
            TaskFailureClass::AccessBaselineDrift,
        ] {
            assert!(!class.is_users_code(), "{class} is not the user's code");
        }
    }

    #[test]
    fn outcome_summaries_are_bounded_and_truncation_is_visible() {
        let outcome = TaskOutcome::new(
            Some(1),
            TaskFailureClass::ProjectCodeError,
            "x".repeat(9000),
        );
        assert!(outcome.summary.len() < 9000);
        assert!(outcome.summary.ends_with("[truncated]"));
    }

    #[test]
    fn multibyte_summaries_truncate_on_a_char_boundary() {
        let outcome = TaskOutcome::new(None, TaskFailureClass::Internal, "é".repeat(4000));
        assert!(outcome.summary.ends_with("[truncated]"));
    }
}
