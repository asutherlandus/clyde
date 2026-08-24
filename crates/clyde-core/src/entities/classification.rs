//! Cross-cutting classification vocabulary: trust classes, environments,
//! isolation levels, egress profiles, and the policy-shaped enums that task
//! policies and leases are built from.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::duration::HumanDuration;
use crate::error::ValidationError;

/// Trust class of an execution context.
///
/// T0/T1 hold semi-trusted actors; T2 and above execute project or dependency
/// code and are the reason the isolation and egress machinery exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// Reading and searching, no project code execution.
    T0,
    /// Editing and text manipulation in the workspace environment.
    T1,
    /// Project and dependency code execution, offline.
    T2,
    /// Project code execution with network reachability.
    T3,
    /// Brokered authority: credentials are in scope, outside any sandbox.
    T4,
}

impl fmt::Display for TrustClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
            Self::T4 => "T4",
        };
        f.write_str(text)
    }
}

impl TrustClass {
    /// Whether cgroup v2 limits are mandatory for this class (D22).
    ///
    /// T2 and above execute untrusted code, and rlimits are a weaker bound
    /// rather than an equivalent one, so there is no degraded mode.
    pub fn requires_cgroup_limits(self) -> bool {
        self >= Self::T2
    }
}

/// Which environment family a task runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// The agent's own workspace environment (D1, D17).
    Workspace,
    /// A build/test/fetch sandbox over an immutable snapshot.
    Build,
    /// The credential broker, outside any sandbox.
    Broker,
    /// Trusted, in clyded itself.
    ControlPlane,
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Workspace => "workspace",
            Self::Build => "build",
            Self::Broker => "broker",
            Self::ControlPlane => "control-plane",
        };
        f.write_str(text)
    }
}

/// Minimum isolation a policy demands.
///
/// Ordering is by strength, so a backend satisfies a policy when its own level
/// is greater than or equal to the policy's minimum. A backend never silently
/// runs something weaker than the policy asked for (Phase 2b deliverable 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    /// Runs inside clyded. Only for trusted control-plane operations.
    InProcess,
    /// The broker process. Not a sandbox; a separate authority domain.
    Broker,
    /// User/pid/net/ipc/uts/cgroup namespaces (bubblewrap, Phase 2a).
    NamespaceSandbox,
    /// Hardware-virtualised guest (Firecracker, Phase 2b).
    MicroVm,
}

impl fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::InProcess => "in-process",
            Self::Broker => "broker",
            Self::NamespaceSandbox => "namespace-sandbox",
            Self::MicroVm => "microvm",
        };
        f.write_str(text)
    }
}

/// Which backend actually ran a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Trusted execution inside clyded.
    ControlPlane,
    /// The broker process.
    Broker,
    /// bubblewrap (D5).
    Bubblewrap,
    /// Firecracker (D9).
    Firecracker,
    /// Test-only backend. Never selectable in a shipped binary.
    #[serde(rename = "test_only")]
    TestOnly,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::ControlPlane => "control-plane",
            Self::Broker => "broker",
            Self::Bubblewrap => "bubblewrap",
            Self::Firecracker => "firecracker",
            Self::TestOnly => "test-only",
        };
        f.write_str(text)
    }
}

/// A validated DNS hostname used in an egress allowlist.
///
/// Wildcards are not accepted: an allowlist entry names one host, so approving
/// a destination cannot approve a family of them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct HostName(String);

impl HostName {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValidationError> {
        let raw = value.as_ref().trim();
        let invalid = |reason: &'static str| ValidationError::InvalidRepoPathComponent {
            path: raw.to_owned(),
            reason,
        };
        if raw.is_empty() {
            return Err(ValidationError::EmptyField { field: "host" });
        }
        if raw.len() > 253 {
            return Err(ValidationError::FieldTooLong {
                field: "host",
                max: 253,
            });
        }
        if raw.contains("://") || raw.contains('/') {
            return Err(invalid(
                "looks like a URL; an allowlist entry is a bare host",
            ));
        }
        if raw.contains(':') {
            return Err(invalid("must not carry a port"));
        }
        if raw.contains('*') {
            return Err(invalid("wildcards are not accepted in an allowlist"));
        }
        let lowered = raw.to_ascii_lowercase();
        let labels: Vec<&str> = lowered.split('.').collect();
        if labels.iter().any(|label| label.is_empty()) {
            return Err(invalid("has an empty label"));
        }
        let label_ok = |label: &&str| {
            label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        if !labels.iter().all(label_ok) {
            return Err(invalid("is not a valid DNS name"));
        }
        Ok(Self(lowered))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for HostName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The closed set of egress profiles (network egress model).
///
/// `None` is not "networking disabled by a flag": it is the absence of a proxy
/// socket in the sandbox, which is why there is no runtime switch that turns
/// egress back on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum EgressProfile {
    /// Loopback only, no proxy socket bound.
    None,
    /// Configured model API host(s). TLS terminated, auth injected host-side.
    ModelApi,
    /// Crate registry hosts, pass-through CONNECT.
    RustRegistry,
    /// Not a sandbox profile: the broker's own egress.
    Broker,
    /// An explicit host list from a human approval. Never a default.
    Custom { hosts: BTreeSet<HostName> },
}

impl EgressProfile {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Short stable name for logs, audit records, and CLI output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ModelApi => "model-api",
            Self::RustRegistry => "rust-registry",
            Self::Broker => "broker",
            Self::Custom { .. } => "custom",
        }
    }

    /// Whether the proxy terminates TLS for this profile (D7 amendment).
    ///
    /// Exactly one profile does. The carve-out is narrow on purpose: a sandbox
    /// that does not trust the Clyde CA cannot be transparently intercepted,
    /// and only the workspace environment receives that certificate.
    pub fn terminates_tls(&self) -> bool {
        matches!(self, Self::ModelApi)
    }
}

impl fmt::Display for EgressProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Custom { hosts } => {
                let rendered: Vec<&str> = hosts.iter().map(HostName::as_str).collect();
                write!(f, "custom[{}]", rendered.join(","))
            }
            other => f.write_str(other.name()),
        }
    }
}

/// What credentials a context may use. Never "which credential": the broker
/// interface is capability-oriented, so no type here names a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPolicy {
    /// No credential is reachable. The only value any sandbox ever holds.
    None,
    /// Brokered use of the developer's existing git credential (D8).
    BrokeredGitPush,
}

impl fmt::Display for CredentialPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::None => "none",
            Self::BrokeredGitPush => "brokered-git-push",
        };
        f.write_str(text)
    }
}

impl CredentialPolicy {
    /// Partial order by authority. `None` is below everything.
    pub fn is_no_wider_than(self, other: Self) -> bool {
        match (self, other) {
            (Self::None, _) => true,
            (Self::BrokeredGitPush, Self::BrokeredGitPush) => true,
            (Self::BrokeredGitPush, Self::None) => false,
        }
    }
}

/// Whether an action needs approval, and from whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// Inside the lease; no approval.
    None,
    /// Allowed only if policy configuration pre-approved this class.
    PolicyGated,
    /// A human decides, on the admin socket, every time (D2).
    HumanRequired,
}

impl fmt::Display for ApprovalRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::None => "none",
            Self::PolicyGated => "policy-gated",
            Self::HumanRequired => "human-required",
        };
        f.write_str(text)
    }
}

/// Task input surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InputSpec {
    /// The live mutable tree, lease-scoped by mount topology.
    LiveWorkspace,
    /// An immutable snapshot. `closure_aware` means the build closure is
    /// admitted read-only beyond the requested path.
    Snapshot { closure_aware: bool },
    /// Manifests and lockfile only: no application source (Phase 3).
    ManifestsOnly,
    /// A commit identifier and refspec, not a filesystem.
    CommitRef,
}

/// What a task is permitted to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSpec {
    Log,
    BuildOutput,
    DependencyBundle,
    FetchManifest,
    CommitProposal,
    Diff,
}

/// Cache policy. Dependency caches are read-only; build caches are per-mission
/// and writable (D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CachePolicy {
    None,
    /// `CARGO_TARGET_DIR` and a per-mission `CARGO_HOME`, destroyed at closeout.
    MissionScoped {
        cargo_home: bool,
        target_dir: bool,
    },
    /// Writes into the content-addressed dependency bundle store.
    WritesDependencyBundle,
}

/// How much detail the audit record carries for a task family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditLevel {
    Summary,
    Detailed,
    /// Every decision and every egress attempt, for brokered authority.
    Full,
}

/// Resource limits applied to a sandbox.
///
/// `max_wall_clock` is enforced by the daemon; memory, CPU, and task counts come
/// from cgroup v2 where the trust class requires it (D22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub max_wall_clock: HumanDuration,
    pub max_memory_bytes: u64,
    pub max_cpu_percent: u32,
    pub max_tasks: u32,
    pub max_open_files: u64,
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.max_memory_bytes == 0 {
            return Err(ValidationError::ZeroValue {
                field: "max_memory_bytes",
            });
        }
        if self.max_cpu_percent == 0 {
            return Err(ValidationError::ZeroValue {
                field: "max_cpu_percent",
            });
        }
        if self.max_tasks == 0 {
            return Err(ValidationError::ZeroValue { field: "max_tasks" });
        }
        if self.max_open_files == 0 {
            return Err(ValidationError::ZeroValue {
                field: "max_open_files",
            });
        }
        Ok(())
    }

    /// Element-wise minimum. Used when configuration narrows a built-in policy:
    /// narrowing is always permitted, widening never is (D14).
    pub fn narrowed_to(self, other: Self) -> Self {
        Self {
            max_wall_clock: self.max_wall_clock.min(other.max_wall_clock),
            max_memory_bytes: self.max_memory_bytes.min(other.max_memory_bytes),
            max_cpu_percent: self.max_cpu_percent.min(other.max_cpu_percent),
            max_tasks: self.max_tasks.min(other.max_tasks),
            max_open_files: self.max_open_files.min(other.max_open_files),
        }
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

    #[test]
    fn trust_classes_order_by_strength() {
        assert!(TrustClass::T0 < TrustClass::T2);
        assert!(TrustClass::T2.requires_cgroup_limits());
        assert!(TrustClass::T3.requires_cgroup_limits());
        assert!(!TrustClass::T1.requires_cgroup_limits());
    }

    #[test]
    fn isolation_levels_order_by_strength() {
        assert!(IsolationLevel::MicroVm > IsolationLevel::NamespaceSandbox);
        assert!(IsolationLevel::NamespaceSandbox > IsolationLevel::InProcess);
    }

    #[test]
    fn only_model_api_terminates_tls() {
        assert!(EgressProfile::ModelApi.terminates_tls());
        for profile in [
            EgressProfile::None,
            EgressProfile::RustRegistry,
            EgressProfile::Broker,
            EgressProfile::Custom {
                hosts: BTreeSet::new(),
            },
        ] {
            assert!(
                !profile.terminates_tls(),
                "{profile} must not be terminated"
            );
        }
    }

    #[test]
    fn hostname_validation_rejects_urls_ports_and_wildcards() {
        assert_eq!(
            HostName::parse("Static.Crates.IO").unwrap().as_str(),
            "static.crates.io"
        );
        for bad in [
            "https://crates.io",
            "crates.io:443",
            "*.crates.io",
            "crates..io",
            "-bad.example",
            "bad-.example",
            "",
            "crates.io/index",
        ] {
            assert!(HostName::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn credential_policy_ordering_is_fail_closed() {
        assert!(CredentialPolicy::None.is_no_wider_than(CredentialPolicy::BrokeredGitPush));
        assert!(!CredentialPolicy::BrokeredGitPush.is_no_wider_than(CredentialPolicy::None));
    }

    #[test]
    fn resource_limits_narrow_element_wise() {
        let a = ResourceLimits {
            max_wall_clock: HumanDuration::parse("10m").unwrap(),
            max_memory_bytes: 8 << 30,
            max_cpu_percent: 400,
            max_tasks: 512,
            max_open_files: 4096,
        };
        let b = ResourceLimits {
            max_wall_clock: HumanDuration::parse("5m").unwrap(),
            max_memory_bytes: 16 << 30,
            max_cpu_percent: 100,
            max_tasks: 1024,
            max_open_files: 1024,
        };
        let narrowed = a.narrowed_to(b);
        assert_eq!(narrowed.max_wall_clock.as_secs(), 300);
        assert_eq!(narrowed.max_memory_bytes, 8 << 30);
        assert_eq!(narrowed.max_cpu_percent, 100);
        assert_eq!(narrowed.max_open_files, 1024);
        assert!(narrowed.validate().is_ok());
    }

    #[test]
    fn zero_limits_are_rejected() {
        let limits = ResourceLimits {
            max_wall_clock: HumanDuration::parse("1m").unwrap(),
            max_memory_bytes: 0,
            max_cpu_percent: 100,
            max_tasks: 10,
            max_open_files: 10,
        };
        assert!(limits.validate().is_err());
    }

    #[test]
    fn egress_profile_serde_round_trips_with_hosts() {
        let profile = EgressProfile::Custom {
            hosts: [HostName::parse("example.test").unwrap()]
                .into_iter()
                .collect(),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let back: EgressProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(profile, back);
        assert_eq!(profile.to_string(), "custom[example.test]");
    }
}
