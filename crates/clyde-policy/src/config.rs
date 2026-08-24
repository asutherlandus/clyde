//! Configuration layering (D14, D20).
//!
//! Precedence is built-in defaults → host config → user config → repository
//! config. The first three may set any key. **Repository config may only
//! narrow**, and a repository value that would widen authority is a *rejection
//! with a diagnostic*, not a silent clamp: a silently clamped config leaves the
//! user believing something is configured that is not.
//!
//! Some keys are rejected outright rather than narrowed, because narrowing is
//! not a meaningful operation on them — most importantly `agent.command`, since
//! a repository that can choose what Clyde execs has arbitrary code execution in
//! the workspace environment before any policy applies (D20).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use clyde_core::HumanDuration;
use clyde_core::budget::Budget;
use clyde_core::classification::{ApprovalRequirement, EgressProfile, HostName, ResourceLimits};
use clyde_core::digest::Digest;
use clyde_core::task::{RuntimeRootKind, TaskType};

use crate::egress::{EgressComparison, default_registry_hosts, is_no_wider_than};

/// Which layer a value came from. Recorded so that "why is this configured this
/// way" is answerable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    BuiltIn,
    Host,
    User,
    /// `.clyde/policy.toml`. Untrusted content: may only narrow.
    Repository,
}

impl ConfigSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Host => "host",
            Self::User => "user",
            Self::Repository => "repository",
        }
    }

    /// Whether this layer is permitted to widen authority.
    fn may_widen(self) -> bool {
        !matches!(self, Self::Repository)
    }
}

/// What the agent process is, and what environment it may see.
///
/// Host or user configuration only. This is the highest-consequence instance of
/// D14 and is called out separately as D20.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub command: Option<PathBuf>,
    pub args: Vec<String>,
    /// Allowlist of environment variable names passed through. The sandbox
    /// environment is otherwise cleared.
    pub env: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    pub allowed: BTreeSet<HostName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressConfig {
    pub model_api_hosts: BTreeSet<HostName>,
    /// Whether the model-API credential is injected host-side. There is no
    /// setting that puts it in the sandbox instead (D11).
    pub model_api_auth_header: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotConfig {
    /// Additional exclusions. Repository config may add and never remove, since
    /// narrowing is always permitted.
    pub exclusions: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionDefaults {
    pub budget: Budget,
    pub max_expiry: HumanDuration,
    pub edit_paths: BTreeSet<clyde_core::RepoPath>,
    pub read_paths: BTreeSet<clyde_core::RepoPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushConfig {
    pub remotes: BTreeSet<String>,
    pub branch_patterns: BTreeSet<String>,
    /// Patterns refused outright, whatever the branch allowlist says.
    pub protected_branch_patterns: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitsConfig {
    pub workspace: ResourceLimits,
    pub build: ResourceLimits,
}

/// Host paths to the sandbox implementations and runtime roots.
///
/// Host or user configuration only: a repository cannot choose which runtime
/// root a task executes against (D20).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxConfig {
    pub bwrap: Option<PathBuf>,
    pub firecracker: Option<PathBuf>,
    pub prlimit: Option<PathBuf>,
    pub systemd_run: Option<PathBuf>,
    pub runtime_roots: BTreeMap<RuntimeRootKind, PathBuf>,
    pub forwarder: Option<PathBuf>,
}

/// Broker configuration. Never settable by a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrokerConfig {
    pub socket: Option<PathBuf>,
    /// Whether the broker may use an `ssh-agent` connection, in addition to a
    /// key file.
    pub allow_ssh_agent: bool,
}

/// A per-task narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOverride {
    pub enabled: bool,
    pub egress: Option<EgressProfile>,
    pub approval: Option<ApprovalRequirement>,
    pub limits: Option<ResourceLimits>,
}

/// The fully resolved configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub agent: AgentConfig,
    pub registries: RegistryConfig,
    pub egress: EgressConfig,
    pub snapshot: SnapshotConfig,
    pub mission_defaults: MissionDefaults,
    pub push: PushConfig,
    pub limits: LimitsConfig,
    pub sandbox: SandboxConfig,
    pub broker: BrokerConfig,
    pub tasks: BTreeMap<TaskType, TaskOverride>,
}

impl Config {
    /// The built-in defaults, which are the narrowest useful configuration.
    pub fn defaults() -> Self {
        Self {
            agent: AgentConfig {
                command: None,
                args: Vec::new(),
                env: ["TERM", "LANG", "LC_ALL"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            registries: RegistryConfig {
                allowed: default_registry_hosts(),
            },
            egress: EgressConfig {
                model_api_hosts: BTreeSet::new(),
                model_api_auth_header: None,
            },
            snapshot: SnapshotConfig {
                exclusions: BTreeSet::new(),
            },
            mission_defaults: MissionDefaults {
                budget: Budget {
                    max_duration: HumanDuration::parse("2h").unwrap_or(HumanDuration::MINIMUM),
                    max_task_runs: 50,
                    max_parallel_subagents: 2,
                    max_subagents: 4,
                    max_cpu_seconds: 7200,
                    max_cache_bytes: 8 << 30,
                    max_artifact_bytes: 2 << 30,
                    max_egress_bytes: 512 << 20,
                    max_egress_requests: 2000,
                },
                max_expiry: HumanDuration::parse("8h").unwrap_or(HumanDuration::MINIMUM),
                edit_paths: BTreeSet::new(),
                read_paths: BTreeSet::new(),
            },
            push: PushConfig {
                remotes: BTreeSet::new(),
                branch_patterns: BTreeSet::new(),
                protected_branch_patterns: ["main", "master", "release/*", "production"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            limits: LimitsConfig {
                workspace: ResourceLimits {
                    max_wall_clock: HumanDuration::parse("4h").unwrap_or(HumanDuration::MINIMUM),
                    max_memory_bytes: 4 << 30,
                    max_cpu_percent: 200,
                    max_tasks: 256,
                    max_open_files: 4096,
                },
                build: ResourceLimits {
                    max_wall_clock: HumanDuration::parse("45m").unwrap_or(HumanDuration::MINIMUM),
                    max_memory_bytes: 8 << 30,
                    max_cpu_percent: 400,
                    max_tasks: 512,
                    max_open_files: 8192,
                },
            },
            sandbox: SandboxConfig::default(),
            broker: BrokerConfig::default(),
            tasks: BTreeMap::new(),
        }
    }

    /// The override for a task, if any.
    pub fn task_override(&self, task: TaskType) -> Option<&TaskOverride> {
        self.tasks.get(&task)
    }
}

// ---------------------------------------------------------------------------
// File representation
// ---------------------------------------------------------------------------

/// A configuration file as parsed. Every field optional; unknown fields
/// rejected, so a typo in a policy file is an error rather than a silently
/// ignored restriction.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub agent: Option<AgentSection>,
    pub registries: Option<RegistriesSection>,
    pub egress: Option<EgressSection>,
    pub snapshot: Option<SnapshotSection>,
    pub mission: Option<MissionSection>,
    pub push: Option<PushSection>,
    pub limits: Option<LimitsSection>,
    pub sandbox: Option<SandboxSection>,
    pub broker: Option<BrokerSection>,
    pub tasks: Option<BTreeMap<String, TaskSection>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub command: Option<PathBuf>,
    pub args: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistriesSection {
    pub allowed: Option<Vec<HostName>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressSection {
    pub model_api_hosts: Option<Vec<HostName>>,
    pub model_api_auth_header: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotSection {
    pub exclusions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionSection {
    pub defaults: Option<MissionDefaultsSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionDefaultsSection {
    pub max_duration: Option<HumanDuration>,
    pub max_expiry: Option<HumanDuration>,
    pub max_task_runs: Option<u32>,
    pub max_parallel_subagents: Option<u8>,
    pub max_subagents: Option<u8>,
    pub max_cpu_seconds: Option<u64>,
    pub max_cache_bytes: Option<u64>,
    pub max_artifact_bytes: Option<u64>,
    pub max_egress_bytes: Option<u64>,
    pub max_egress_requests: Option<u32>,
    pub edit_paths: Option<Vec<clyde_core::RepoPath>>,
    pub read_paths: Option<Vec<clyde_core::RepoPath>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushSection {
    pub remotes: Option<Vec<String>>,
    pub branch_patterns: Option<Vec<String>>,
    pub protected_branch_patterns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    pub workspace: Option<ResourceLimitsSection>,
    pub build: Option<ResourceLimitsSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimitsSection {
    pub max_wall_clock: Option<HumanDuration>,
    pub max_memory_bytes: Option<u64>,
    pub max_cpu_percent: Option<u32>,
    pub max_tasks: Option<u32>,
    pub max_open_files: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSection {
    pub bwrap: Option<PathBuf>,
    pub firecracker: Option<PathBuf>,
    pub prlimit: Option<PathBuf>,
    pub systemd_run: Option<PathBuf>,
    pub forwarder: Option<PathBuf>,
    pub runtime_root_workspace: Option<PathBuf>,
    pub runtime_root_rust: Option<PathBuf>,
    pub runtime_root_fetch: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerSection {
    pub socket: Option<PathBuf>,
    pub allow_ssh_agent: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSection {
    pub enabled: Option<bool>,
    pub egress: Option<EgressProfile>,
    pub approval: Option<ApprovalRequirement>,
    pub runtime_root: Option<RuntimeRootKind>,
    pub limits: Option<ResourceLimitsSection>,
}

// ---------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------

/// Why a configuration value was rejected.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RejectionKind {
    /// The key may never be set by this layer, and narrowing is not a
    /// meaningful operation on it.
    NotSettableByLayer,
    /// The value would widen authority relative to the layer above.
    WidensAuthority { current: String, requested: String },
    /// The value is malformed in a way the type system could not catch.
    Invalid { detail: String },
}

/// One rejected key, with enough detail to fix it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigRejection {
    pub key: String,
    pub source: ConfigSource,
    pub kind: RejectionKind,
}

impl ConfigRejection {
    pub fn render(&self) -> String {
        match &self.kind {
            RejectionKind::NotSettableByLayer => format!(
                "{} configuration may not set {}: repository content cannot widen its own authority",
                self.source.name(),
                self.key
            ),
            RejectionKind::WidensAuthority { current, requested } => format!(
                "{} configuration sets {} to {requested}, which is wider than {current}; repository configuration may only narrow",
                self.source.name(),
                self.key
            ),
            RejectionKind::Invalid { detail } => {
                format!("{} is invalid: {detail}", self.key)
            }
        }
    }
}

/// Errors from loading a configuration layer.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("configuration was rejected:\n{}", render_rejections(.0))]
    Rejected(Vec<ConfigRejection>),
}

fn render_rejections(rejections: &[ConfigRejection]) -> String {
    rejections
        .iter()
        .map(|rejection| format!("  - {}", rejection.render()))
        .collect::<Vec<_>>()
        .join("\n")
}

impl ConfigError {
    /// The rejected keys, for the `config_loads` audit record.
    pub fn rejections(&self) -> &[ConfigRejection] {
        match self {
            Self::Rejected(rejections) => rejections,
            Self::Parse(_) => &[],
        }
    }
}

/// The record written to `config_loads` for each layer applied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigLoadRecord {
    pub source: ConfigSource,
    pub digest: Digest,
    pub rejections: Vec<ConfigRejection>,
}

// ---------------------------------------------------------------------------
// Layering
// ---------------------------------------------------------------------------

/// Parses and applies one configuration layer.
///
/// Returns the merged configuration and the load record. A repository layer that
/// would widen authority produces [`ConfigError::Rejected`] listing every
/// offending key, so one pass reports all the problems.
pub fn apply_layer(
    base: Config,
    text: &str,
    source: ConfigSource,
) -> Result<(Config, ConfigLoadRecord), ConfigError> {
    let digest = Digest::of_bytes(text.as_bytes());
    let file: ConfigFile = toml::from_str(text)?;
    let mut rejections = Vec::new();
    let merged = merge(base, file, source, &mut rejections);
    if rejections.is_empty() {
        Ok((
            merged,
            ConfigLoadRecord {
                source,
                digest,
                rejections,
            },
        ))
    } else {
        Err(ConfigError::Rejected(rejections))
    }
}

/// Merges a parsed file into `base`, collecting rejections rather than
/// short-circuiting, so a user fixing a repo config sees every problem at once.
fn merge(
    mut base: Config,
    file: ConfigFile,
    source: ConfigSource,
    rejections: &mut Vec<ConfigRejection>,
) -> Config {
    let reject_not_settable = |key: &str, rejections: &mut Vec<ConfigRejection>| {
        rejections.push(ConfigRejection {
            key: key.to_owned(),
            source,
            kind: RejectionKind::NotSettableByLayer,
        });
    };

    // agent.*: never settable by a repository (D20). This is checked first
    // because it is the highest-consequence key in the file.
    if let Some(agent) = file.agent {
        if source.may_widen() {
            if let Some(command) = agent.command {
                base.agent.command = Some(command);
            }
            if let Some(args) = agent.args {
                base.agent.args = args;
            }
            if let Some(env) = agent.env {
                base.agent.env = env.into_iter().collect();
            }
        } else {
            if agent.command.is_some() {
                reject_not_settable("agent.command", rejections);
            }
            if agent.args.is_some() {
                reject_not_settable("agent.args", rejections);
            }
            if agent.env.is_some() {
                reject_not_settable("agent.env", rejections);
            }
        }
    }

    // sandbox.* and broker.*: host or user only. A repository choosing the
    // runtime root or the broker socket is the same class of problem as
    // choosing the agent command.
    if let Some(sandbox) = file.sandbox {
        if source.may_widen() {
            base.sandbox.bwrap = sandbox.bwrap.or(base.sandbox.bwrap);
            base.sandbox.firecracker = sandbox.firecracker.or(base.sandbox.firecracker);
            base.sandbox.prlimit = sandbox.prlimit.or(base.sandbox.prlimit);
            base.sandbox.systemd_run = sandbox.systemd_run.or(base.sandbox.systemd_run);
            base.sandbox.forwarder = sandbox.forwarder.or(base.sandbox.forwarder);
            for (kind, path) in [
                (RuntimeRootKind::Workspace, sandbox.runtime_root_workspace),
                (RuntimeRootKind::Rust, sandbox.runtime_root_rust),
                (RuntimeRootKind::Fetch, sandbox.runtime_root_fetch),
            ] {
                if let Some(path) = path {
                    base.sandbox.runtime_roots.insert(kind, path);
                }
            }
        } else {
            reject_not_settable("sandbox.*", rejections);
        }
    }
    if let Some(broker) = file.broker {
        if source.may_widen() {
            base.broker.socket = broker.socket.or(base.broker.socket);
            if let Some(allow) = broker.allow_ssh_agent {
                base.broker.allow_ssh_agent = allow;
            }
        } else {
            reject_not_settable("broker.*", rejections);
        }
    }

    // registries.allowed: a repository may narrow the set of allowed
    // registries but cannot add a host.
    if let Some(registries) = file.registries
        && let Some(allowed) = registries.allowed
    {
        let requested: BTreeSet<HostName> = allowed.into_iter().collect();
        if source.may_widen() || requested.is_subset(&base.registries.allowed) {
            base.registries.allowed = requested;
        } else {
            let added: Vec<String> = requested
                .difference(&base.registries.allowed)
                .map(HostName::to_string)
                .collect();
            rejections.push(ConfigRejection {
                key: "registries.allowed".to_owned(),
                source,
                kind: RejectionKind::WidensAuthority {
                    current: render_hosts(&base.registries.allowed),
                    requested: format!("adds {}", added.join(", ")),
                },
            });
        }
    }

    // egress.model_api_hosts: host or user only. A repository adding a model
    // host would add an exfiltration destination.
    if let Some(egress) = file.egress {
        if source.may_widen() {
            if let Some(hosts) = egress.model_api_hosts {
                base.egress.model_api_hosts = hosts.into_iter().collect();
            }
            if let Some(header) = egress.model_api_auth_header {
                base.egress.model_api_auth_header = Some(header);
            }
        } else {
            if egress.model_api_hosts.is_some() {
                reject_not_settable("egress.model_api_hosts", rejections);
            }
            if egress.model_api_auth_header.is_some() {
                reject_not_settable("egress.model_api_auth_header", rejections);
            }
        }
    }

    // snapshot.exclusions is purely additive, in every layer: adding an
    // exclusion narrows what a task can see, and narrowing is always permitted.
    if let Some(snapshot) = file.snapshot
        && let Some(exclusions) = snapshot.exclusions
    {
        base.snapshot.exclusions.extend(exclusions);
    }

    if let Some(mission) = file.mission
        && let Some(defaults) = mission.defaults
    {
        merge_mission_defaults(&mut base, defaults, source, rejections);
    }

    if let Some(push) = file.push {
        merge_push(&mut base, push, source, rejections);
    }

    if let Some(limits) = file.limits {
        if let Some(section) = limits.workspace {
            base.limits.workspace = merge_limits(
                base.limits.workspace,
                section,
                source,
                "limits.workspace",
                rejections,
            );
        }
        if let Some(section) = limits.build {
            base.limits.build = merge_limits(
                base.limits.build,
                section,
                source,
                "limits.build",
                rejections,
            );
        }
    }

    if let Some(tasks) = file.tasks {
        for (name, section) in tasks {
            merge_task(&mut base, &name, section, source, rejections);
        }
    }

    base
}

fn render_hosts(hosts: &BTreeSet<HostName>) -> String {
    hosts
        .iter()
        .map(HostName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Applies a numeric narrowing: a repository may only lower the value.
fn narrow_u64(
    current: u64,
    requested: u64,
    key: &str,
    source: ConfigSource,
    rejections: &mut Vec<ConfigRejection>,
) -> u64 {
    if source.may_widen() || requested <= current {
        requested
    } else {
        rejections.push(ConfigRejection {
            key: key.to_owned(),
            source,
            kind: RejectionKind::WidensAuthority {
                current: current.to_string(),
                requested: requested.to_string(),
            },
        });
        current
    }
}

fn merge_mission_defaults(
    base: &mut Config,
    section: MissionDefaultsSection,
    source: ConfigSource,
    rejections: &mut Vec<ConfigRejection>,
) {
    let defaults = &mut base.mission_defaults;
    if let Some(value) = section.max_duration {
        let secs = narrow_u64(
            defaults.budget.max_duration.as_secs(),
            value.as_secs(),
            "mission.defaults.max_duration",
            source,
            rejections,
        );
        defaults.budget.max_duration =
            HumanDuration::from_duration(std::time::Duration::from_secs(secs))
                .unwrap_or(HumanDuration::MINIMUM);
    }
    if let Some(value) = section.max_expiry {
        let secs = narrow_u64(
            defaults.max_expiry.as_secs(),
            value.as_secs(),
            "mission.defaults.max_expiry",
            source,
            rejections,
        );
        defaults.max_expiry = HumanDuration::from_duration(std::time::Duration::from_secs(secs))
            .unwrap_or(HumanDuration::MINIMUM);
    }
    if let Some(value) = section.max_task_runs {
        defaults.budget.max_task_runs = u32::try_from(narrow_u64(
            u64::from(defaults.budget.max_task_runs),
            u64::from(value),
            "mission.defaults.max_task_runs",
            source,
            rejections,
        ))
        .unwrap_or(defaults.budget.max_task_runs);
    }
    if let Some(value) = section.max_parallel_subagents {
        defaults.budget.max_parallel_subagents = u8::try_from(narrow_u64(
            u64::from(defaults.budget.max_parallel_subagents),
            u64::from(value),
            "mission.defaults.max_parallel_subagents",
            source,
            rejections,
        ))
        .unwrap_or(defaults.budget.max_parallel_subagents);
    }
    if let Some(value) = section.max_subagents {
        defaults.budget.max_subagents = u8::try_from(narrow_u64(
            u64::from(defaults.budget.max_subagents),
            u64::from(value),
            "mission.defaults.max_subagents",
            source,
            rejections,
        ))
        .unwrap_or(defaults.budget.max_subagents);
    }
    if let Some(value) = section.max_cpu_seconds {
        defaults.budget.max_cpu_seconds = narrow_u64(
            defaults.budget.max_cpu_seconds,
            value,
            "mission.defaults.max_cpu_seconds",
            source,
            rejections,
        );
    }
    if let Some(value) = section.max_cache_bytes {
        defaults.budget.max_cache_bytes = narrow_u64(
            defaults.budget.max_cache_bytes,
            value,
            "mission.defaults.max_cache_bytes",
            source,
            rejections,
        );
    }
    if let Some(value) = section.max_artifact_bytes {
        defaults.budget.max_artifact_bytes = narrow_u64(
            defaults.budget.max_artifact_bytes,
            value,
            "mission.defaults.max_artifact_bytes",
            source,
            rejections,
        );
    }
    if let Some(value) = section.max_egress_bytes {
        defaults.budget.max_egress_bytes = narrow_u64(
            defaults.budget.max_egress_bytes,
            value,
            "mission.defaults.max_egress_bytes",
            source,
            rejections,
        );
    }
    if let Some(value) = section.max_egress_requests {
        defaults.budget.max_egress_requests = u32::try_from(narrow_u64(
            u64::from(defaults.budget.max_egress_requests),
            u64::from(value),
            "mission.defaults.max_egress_requests",
            source,
            rejections,
        ))
        .unwrap_or(defaults.budget.max_egress_requests);
    }
    // Default scopes are a starting point for a mission proposal rather than a
    // ceiling, and a repository suggesting *which* of its own subtrees an agent
    // should edit is advisory: the human still approves the envelope.
    if let Some(paths) = section.edit_paths {
        defaults.edit_paths = paths.into_iter().collect();
    }
    if let Some(paths) = section.read_paths {
        defaults.read_paths = paths.into_iter().collect();
    }
}

fn merge_push(
    base: &mut Config,
    section: PushSection,
    source: ConfigSource,
    rejections: &mut Vec<ConfigRejection>,
) {
    if let Some(remotes) = section.remotes {
        let requested: BTreeSet<String> = remotes.into_iter().collect();
        if source.may_widen() || requested.is_subset(&base.push.remotes) {
            base.push.remotes = requested;
        } else {
            rejections.push(ConfigRejection {
                key: "push.remotes".to_owned(),
                source,
                kind: RejectionKind::WidensAuthority {
                    current: base
                        .push
                        .remotes
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    requested: requested.iter().cloned().collect::<Vec<_>>().join(", "),
                },
            });
        }
    }
    if let Some(patterns) = section.branch_patterns {
        let requested: BTreeSet<String> = patterns.into_iter().collect();
        if source.may_widen() || requested.is_subset(&base.push.branch_patterns) {
            base.push.branch_patterns = requested;
        } else {
            rejections.push(ConfigRejection {
                key: "push.branch_patterns".to_owned(),
                source,
                kind: RejectionKind::WidensAuthority {
                    current: base
                        .push
                        .branch_patterns
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    requested: requested.iter().cloned().collect::<Vec<_>>().join(", "),
                },
            });
        }
    }
    // Protected patterns are additive in every layer: adding one refuses more.
    if let Some(patterns) = section.protected_branch_patterns {
        base.push.protected_branch_patterns.extend(patterns);
    }
}

fn merge_limits(
    current: ResourceLimits,
    section: ResourceLimitsSection,
    source: ConfigSource,
    key_prefix: &str,
    rejections: &mut Vec<ConfigRejection>,
) -> ResourceLimits {
    let mut limits = current;
    if let Some(value) = section.max_wall_clock {
        let secs = narrow_u64(
            current.max_wall_clock.as_secs(),
            value.as_secs(),
            &format!("{key_prefix}.max_wall_clock"),
            source,
            rejections,
        );
        limits.max_wall_clock = HumanDuration::from_duration(std::time::Duration::from_secs(secs))
            .unwrap_or(HumanDuration::MINIMUM);
    }
    if let Some(value) = section.max_memory_bytes {
        limits.max_memory_bytes = narrow_u64(
            current.max_memory_bytes,
            value,
            &format!("{key_prefix}.max_memory_bytes"),
            source,
            rejections,
        );
    }
    if let Some(value) = section.max_cpu_percent {
        limits.max_cpu_percent = u32::try_from(narrow_u64(
            u64::from(current.max_cpu_percent),
            u64::from(value),
            &format!("{key_prefix}.max_cpu_percent"),
            source,
            rejections,
        ))
        .unwrap_or(current.max_cpu_percent);
    }
    if let Some(value) = section.max_tasks {
        limits.max_tasks = u32::try_from(narrow_u64(
            u64::from(current.max_tasks),
            u64::from(value),
            &format!("{key_prefix}.max_tasks"),
            source,
            rejections,
        ))
        .unwrap_or(current.max_tasks);
    }
    if let Some(value) = section.max_open_files {
        limits.max_open_files = narrow_u64(
            current.max_open_files,
            value,
            &format!("{key_prefix}.max_open_files"),
            source,
            rejections,
        );
    }
    limits
}

fn merge_task(
    base: &mut Config,
    name: &str,
    section: TaskSection,
    source: ConfigSource,
    rejections: &mut Vec<ConfigRejection>,
) {
    let task = match TaskType::parse(name) {
        Ok(task) => task,
        Err(error) => {
            rejections.push(ConfigRejection {
                key: format!("tasks.{name}"),
                source,
                kind: RejectionKind::Invalid {
                    detail: error.to_string(),
                },
            });
            return;
        }
    };

    // Runtime root selection is never settable by a repository (D20).
    if section.runtime_root.is_some() && !source.may_widen() {
        rejections.push(ConfigRejection {
            key: format!("tasks.{name}.runtime_root"),
            source,
            kind: RejectionKind::NotSettableByLayer,
        });
    }

    let builtin = crate::catalog::builtin_policy(task);
    let existing = base.tasks.get(&task).cloned();
    let mut entry = existing.unwrap_or(TaskOverride {
        enabled: true,
        egress: None,
        approval: None,
        limits: None,
    });

    // enabled: a repository may disable a task, never enable one the layer
    // above disabled.
    if let Some(enabled) = section.enabled {
        if source.may_widen() || !enabled {
            entry.enabled = enabled;
        } else {
            rejections.push(ConfigRejection {
                key: format!("tasks.{name}.enabled"),
                source,
                kind: RejectionKind::WidensAuthority {
                    current: "false".to_owned(),
                    requested: "true".to_owned(),
                },
            });
        }
    }

    // egress: must be no wider than what is currently in force.
    if let Some(requested) = section.egress {
        let current = entry.egress.clone().unwrap_or(builtin.egress.clone());
        if source.may_widen() {
            entry.egress = Some(requested);
        } else {
            match is_no_wider_than(&requested, &current) {
                EgressComparison::NoWider => entry.egress = Some(requested),
                EgressComparison::Wider | EgressComparison::Incomparable => {
                    rejections.push(ConfigRejection {
                        key: format!("tasks.{name}.egress"),
                        source,
                        kind: RejectionKind::WidensAuthority {
                            current: current.to_string(),
                            requested: requested.to_string(),
                        },
                    });
                }
            }
        }
    }

    // approval: a repository may only strengthen the requirement.
    if let Some(requested) = section.approval {
        let current = entry.approval.unwrap_or(builtin.approval);
        if source.may_widen() || requested >= current {
            entry.approval = Some(requested);
        } else {
            rejections.push(ConfigRejection {
                key: format!("tasks.{name}.approval"),
                source,
                kind: RejectionKind::WidensAuthority {
                    current: current.to_string(),
                    requested: requested.to_string(),
                },
            });
        }
    }

    if let Some(section) = section.limits {
        let current = entry.limits.unwrap_or(builtin.limits);
        entry.limits = Some(merge_limits(
            current,
            section,
            source,
            &format!("tasks.{name}.limits"),
            rejections,
        ));
    }

    base.tasks.insert(task, entry);
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

    fn host_layer(base: Config, text: &str) -> Config {
        apply_layer(base, text, ConfigSource::Host)
            .expect("host layer must apply")
            .0
    }

    fn repo_result(base: Config, text: &str) -> Result<(Config, ConfigLoadRecord), ConfigError> {
        apply_layer(base, text, ConfigSource::Repository)
    }

    #[test]
    fn defaults_are_internally_valid() {
        let config = Config::defaults();
        assert!(config.limits.workspace.validate().is_ok());
        assert!(config.limits.build.validate().is_ok());
        assert!(config.mission_defaults.budget.validate().is_ok());
        assert_eq!(config.registries.allowed.len(), 3);
        assert!(
            config.push.remotes.is_empty(),
            "no remote is push-allowlisted by default"
        );
        assert!(config.push.protected_branch_patterns.contains("main"));
    }

    #[test]
    fn repository_config_cannot_set_the_agent_command() {
        // The load-bearing D20 test: a repository that can choose what Clyde
        // execs has arbitrary code execution before any policy applies.
        let error = repo_result(
            Config::defaults(),
            r#"
            [agent]
            command = "/tmp/evil"
            "#,
        )
        .expect_err("agent.command must be rejected");
        let rejections = error.rejections();
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].key, "agent.command");
        assert_eq!(rejections[0].kind, RejectionKind::NotSettableByLayer);
        assert!(rejections[0].render().contains("may not set agent.command"));
    }

    #[test]
    fn host_config_can_set_the_agent_command() {
        let config = host_layer(
            Config::defaults(),
            r#"
            [agent]
            command = "/usr/bin/claude"
            args = ["--mcp-socket", "/run/clyde/clyded.sock"]
            env = ["TERM"]
            "#,
        );
        assert_eq!(config.agent.command, Some(PathBuf::from("/usr/bin/claude")));
        assert_eq!(config.agent.args.len(), 2);
        assert_eq!(config.agent.env.len(), 1);
    }

    #[test]
    fn repository_config_cannot_add_a_registry_host_but_can_remove_one() {
        let error = repo_result(
            Config::defaults(),
            r#"
            [registries]
            allowed = ["crates.io", "mirror.evil.test"]
            "#,
        )
        .expect_err("adding a registry host must be rejected");
        assert_eq!(error.rejections()[0].key, "registries.allowed");
        assert!(error.rejections()[0].render().contains("mirror.evil.test"));

        let (narrowed, record) = repo_result(
            Config::defaults(),
            r#"
            [registries]
            allowed = ["crates.io"]
            "#,
        )
        .expect("narrowing the registry set must be permitted");
        assert_eq!(narrowed.registries.allowed.len(), 1);
        assert!(record.rejections.is_empty());
        assert_eq!(record.source, ConfigSource::Repository);
    }

    #[test]
    fn repository_config_cannot_set_model_api_hosts_or_broker_or_sandbox() {
        for text in [
            "[egress]\nmodel_api_hosts = [\"evil.test\"]\n",
            "[broker]\nsocket = \"/tmp/brokerd.sock\"\n",
            "[sandbox]\nruntime_root_rust = \"/tmp/root\"\n",
        ] {
            let error =
                repo_result(Config::defaults(), text).expect_err("host-only key must be rejected");
            assert!(matches!(
                error.rejections()[0].kind,
                RejectionKind::NotSettableByLayer
            ));
        }
    }

    #[test]
    fn snapshot_exclusions_are_additive_from_a_repository() {
        let (config, _) = repo_result(
            Config::defaults(),
            r#"
            [snapshot]
            exclusions = ["fixtures/large", "*.bin"]
            "#,
        )
        .expect("adding exclusions narrows and is permitted");
        assert!(config.snapshot.exclusions.contains("fixtures/large"));
        assert!(config.snapshot.exclusions.contains("*.bin"));
    }

    #[test]
    fn repository_config_may_lower_but_not_raise_limits() {
        let base = Config::defaults();
        let (narrowed, _) = repo_result(
            base.clone(),
            r#"
            [limits.build]
            max_memory_bytes = 1073741824
            max_wall_clock = "10m"
            "#,
        )
        .expect("lowering limits is narrowing");
        assert_eq!(narrowed.limits.build.max_memory_bytes, 1 << 30);
        assert_eq!(narrowed.limits.build.max_wall_clock.as_secs(), 600);

        let error = repo_result(
            base,
            r#"
            [limits.build]
            max_memory_bytes = 137438953472
            "#,
        )
        .expect_err("raising a limit must be rejected");
        assert_eq!(error.rejections()[0].key, "limits.build.max_memory_bytes");
    }

    #[test]
    fn repository_config_may_narrow_a_task_egress_profile_but_not_widen_it() {
        let mut base = Config::defaults();
        base.tasks.insert(
            TaskType::RustResolveDeps,
            TaskOverride {
                enabled: true,
                egress: Some(EgressProfile::RustRegistry),
                approval: None,
                limits: None,
            },
        );
        let (narrowed, _) = repo_result(
            base.clone(),
            r#"
            [tasks."rust.resolve-deps"]
            egress = { profile = "none" }
            "#,
        )
        .expect("narrowing to none is permitted");
        assert_eq!(
            narrowed.tasks[&TaskType::RustResolveDeps].egress,
            Some(EgressProfile::None)
        );

        let error = repo_result(
            base,
            r#"
            [tasks."rust.check"]
            egress = { profile = "rust_registry" }
            "#,
        )
        .expect_err("giving rust.check network access must be rejected");
        assert_eq!(error.rejections()[0].key, "tasks.rust.check.egress");
    }

    #[test]
    fn repository_config_may_strengthen_but_not_weaken_an_approval_requirement() {
        let (strengthened, _) = repo_result(
            Config::defaults(),
            r#"
            [tasks."rust.check"]
            approval = "human_required"
            "#,
        )
        .expect("requiring approval is narrowing");
        assert_eq!(
            strengthened.tasks[&TaskType::RustCheck].approval,
            Some(ApprovalRequirement::HumanRequired)
        );

        let error = repo_result(
            Config::defaults(),
            r#"
            [tasks."git.push"]
            approval = "none"
            "#,
        )
        .expect_err("removing approval on push must be rejected");
        assert_eq!(error.rejections()[0].key, "tasks.git.push.approval");
    }

    #[test]
    fn repository_config_may_disable_a_task_but_not_enable_one() {
        let (disabled, _) = repo_result(
            Config::defaults(),
            r#"
            [tasks."rust.test.unit"]
            enabled = false
            "#,
        )
        .expect("disabling narrows");
        assert!(!disabled.tasks[&TaskType::RustTestUnit].enabled);

        let mut base = Config::defaults();
        base.tasks.insert(
            TaskType::GitPush,
            TaskOverride {
                enabled: false,
                egress: None,
                approval: None,
                limits: None,
            },
        );
        let error = repo_result(
            base,
            r#"
            [tasks."git.push"]
            enabled = true
            "#,
        )
        .expect_err("re-enabling must be rejected");
        assert_eq!(error.rejections()[0].key, "tasks.git.push.enabled");
    }

    #[test]
    fn unknown_keys_and_unknown_tasks_are_rejected() {
        assert!(matches!(
            apply_layer(
                Config::defaults(),
                "[nonsense]\nx = 1\n",
                ConfigSource::Host
            ),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            apply_layer(
                Config::defaults(),
                "[agent]\ncommandd = \"x\"\n",
                ConfigSource::Host
            ),
            Err(ConfigError::Parse(_))
        ));
        let error = repo_result(
            Config::defaults(),
            r#"
            [tasks."shell.exec"]
            enabled = true
            "#,
        )
        .expect_err("an unknown task name must be rejected");
        assert!(matches!(
            error.rejections()[0].kind,
            RejectionKind::Invalid { .. }
        ));
    }

    #[test]
    fn all_rejections_are_reported_in_one_pass() {
        let error = repo_result(
            Config::defaults(),
            r#"
            [agent]
            command = "/tmp/evil"
            args = ["--x"]

            [registries]
            allowed = ["evil.test"]

            [limits.build]
            max_cpu_percent = 6400
            "#,
        )
        .expect_err("multiple problems");
        let keys: Vec<&str> = error
            .rejections()
            .iter()
            .map(|rejection| rejection.key.as_str())
            .collect();
        assert!(keys.contains(&"agent.command"));
        assert!(keys.contains(&"agent.args"));
        assert!(keys.contains(&"registries.allowed"));
        assert!(keys.contains(&"limits.build.max_cpu_percent"));
    }

    #[test]
    fn layer_precedence_is_defaults_then_host_then_user_then_repo() {
        let base = Config::defaults();
        let host = host_layer(
            base,
            r#"
            [mission.defaults]
            max_task_runs = 40

            [push]
            remotes = ["origin", "fork"]
            "#,
        );
        assert_eq!(host.mission_defaults.budget.max_task_runs, 40);
        let (user, _) = apply_layer(
            host,
            r#"
            [mission.defaults]
            max_task_runs = 30
            "#,
            ConfigSource::User,
        )
        .expect("user layer");
        assert_eq!(user.mission_defaults.budget.max_task_runs, 30);
        let (repo, _) = repo_result(
            user,
            r#"
            [mission.defaults]
            max_task_runs = 5

            [push]
            remotes = ["fork"]
            "#,
        )
        .expect("repo narrowing");
        assert_eq!(repo.mission_defaults.budget.max_task_runs, 5);
        assert_eq!(repo.push.remotes.len(), 1);
    }

    #[test]
    fn a_user_layer_may_widen_relative_to_the_host() {
        // Only the repository layer is narrow-only; a user configuring their own
        // machine is not an untrusted input.
        let host = host_layer(
            Config::defaults(),
            "[mission.defaults]\nmax_task_runs = 10\n",
        );
        let (user, _) = apply_layer(
            host,
            "[mission.defaults]\nmax_task_runs = 20\n",
            ConfigSource::User,
        )
        .expect("user layer may widen");
        assert_eq!(user.mission_defaults.budget.max_task_runs, 20);
    }

    #[test]
    fn load_records_carry_the_file_digest() {
        let text = "[snapshot]\nexclusions = [\"x\"]\n";
        let (_, record) =
            apply_layer(Config::defaults(), text, ConfigSource::Repository).expect("valid layer");
        assert_eq!(record.digest, Digest::of_bytes(text.as_bytes()));
    }

    #[test]
    fn an_empty_layer_changes_nothing() {
        let base = Config::defaults();
        let (after, record) = apply_layer(base.clone(), "", ConfigSource::Repository)
            .expect("an empty file is valid");
        assert_eq!(base, after);
        assert!(record.rejections.is_empty());
    }
}
