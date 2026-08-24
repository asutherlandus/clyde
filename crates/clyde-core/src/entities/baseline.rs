//! Access baselines (D18).
//!
//! A baseline pins what a build or test task may read: subtree grants for
//! first-party code, file pins for anything outside them, and an inventory of
//! every dependency package that executes code at build time.
//!
//! The two tiers exist because the threat is dependency code *changing*, while
//! editing the project is the work. Treating both at the same granularity would
//! make an agent adding a source file indistinguishable from a build script
//! reaching somewhere new, and a control that fires on ordinary editing gets
//! clicked through.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::digest::{CanonicalError, Digest};
use crate::entities::task::TaskType;
use crate::error::ValidationError;
use crate::ids::{ActorId, WorkspaceId};
use crate::repo_path::RepoPath;

/// Why a subtree is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantSource {
    /// Approved as writable in the mission envelope.
    MissionEditScope,
    /// Approved as readable in the mission envelope.
    MissionReadScope,
    /// Required by cargo, within the approved scope.
    BuildClosure,
}

/// One entry in a baseline's repo read set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselinePathEntry {
    /// First-party project code approved as a whole subtree. **Not**
    /// drift-sensitive: files created, renamed, moved, or deleted inside it are
    /// ordinary work and produce no prompt.
    SubtreeGrant { path: RepoPath, source: GrantSource },
    /// A single path outside every granted subtree, confirmed individually.
    /// Drift-sensitive.
    FilePin {
        path: RepoPath,
        blake3: Option<Digest>,
        reason: String,
    },
    /// Rollup form of many pins under one out-of-scope directory.
    SubtreePin { path: RepoPath, reason: String },
}

impl BaselinePathEntry {
    pub fn path(&self) -> &RepoPath {
        match self {
            Self::SubtreeGrant { path, .. }
            | Self::FilePin { path, .. }
            | Self::SubtreePin { path, .. } => path,
        }
    }

    /// Whether this entry admits `candidate`.
    ///
    /// A grant and a subtree pin admit their whole subtree; a file pin admits
    /// exactly one path.
    pub fn admits(&self, candidate: &RepoPath) -> bool {
        match self {
            Self::SubtreeGrant { path, .. } | Self::SubtreePin { path, .. } => {
                candidate.is_within(path)
            }
            Self::FilePin { path, .. } => candidate == path,
        }
    }

    /// Whether a change under this entry is drift.
    ///
    /// Only subtree grants are insensitive; that asymmetry is the whole point of
    /// the two-tier model.
    pub fn is_drift_sensitive(&self) -> bool {
        !matches!(self, Self::SubtreeGrant { .. })
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::SubtreeGrant { .. } => Ok(()),
            // A pin requires a reason so a later reviewer can tell why the build
            // reaches outside the project's approved scope.
            Self::FilePin { reason, .. } | Self::SubtreePin { reason, .. } => {
                if reason.trim().is_empty() {
                    Err(ValidationError::EmptyField { field: "reason" })
                } else if reason.len() > 1024 {
                    Err(ValidationError::FieldTooLong {
                        field: "reason",
                        max: 1024,
                    })
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// What kind of build-time code execution a package performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeExecKind {
    BuildScript,
    ProcMacro,
    Both,
}

impl CodeExecKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::BuildScript => "build.rs",
            Self::ProcMacro => "proc-macro",
            Self::Both => "build.rs + proc-macro",
        }
    }
}

impl std::fmt::Display for CodeExecKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One package that executes code at build time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CodeExecEntry {
    pub crate_name: String,
    pub version: String,
    pub kind: CodeExecKind,
    /// Hash of the package source in the bundle. A change here at the same
    /// version is a registry-tampering signal.
    pub source_blake3: Digest,
}

/// The pinned inventory of build-time code execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecInventory {
    pub lockfile_digest: Option<Digest>,
    pub entries: Vec<CodeExecEntry>,
}

impl CodeExecInventory {
    /// Sorts entries so two computations of the same inventory compare equal.
    pub fn canonicalised(mut self) -> Self {
        self.entries.sort();
        self.entries.dedup();
        self
    }

    pub fn find(&self, crate_name: &str) -> Option<&CodeExecEntry> {
        self.entries
            .iter()
            .find(|entry| entry.crate_name == crate_name)
    }
}

/// Where a baseline came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineOrigin {
    /// Proposed from the static build closure. The normal path.
    StaticClosure,
    /// Observed during a learn-mode run. A privilege, admin-channel only.
    Learned,
    /// A confirmed amendment to an existing baseline.
    Amended,
}

/// A confirmed access baseline.
///
/// Stored in Clyde state only, never in the repository: an attacker able to edit
/// the baseline could conceal their own drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessBaseline {
    pub workspace: WorkspaceId,
    pub task: TaskType,
    /// The build target the baseline applies to.
    pub target: RepoPath,
    pub paths: Vec<BaselinePathEntry>,
    pub inventory: CodeExecInventory,
    pub origin: BaselineOrigin,
    /// Must be a human, confirmed on the admin channel.
    pub confirmed_by: ActorId,
    pub confirmed_at: DateTime<Utc>,
}

/// A baseline that has been computed but not yet confirmed by a human.
///
/// A proposal has no effect: it is a distinct type from [`AccessBaseline`] so
/// that "unconfirmed baseline in force" is not representable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineProposal {
    pub workspace: WorkspaceId,
    pub task: TaskType,
    pub target: RepoPath,
    pub paths: Vec<BaselinePathEntry>,
    pub inventory: CodeExecInventory,
    pub origin: BaselineOrigin,
    pub proposed_at: DateTime<Utc>,
    /// Free-text rationale shown to the human who confirms it.
    pub rationale: Vec<String>,
}

impl BaselineProposal {
    /// Confirms the proposal. Rejects a non-human confirmer, because a baseline
    /// has no effect until a human confirmed it on the admin channel.
    pub fn confirm(
        self,
        confirmed_by: ActorId,
        confirmed_at: DateTime<Utc>,
    ) -> Result<AccessBaseline, ValidationError> {
        if !confirmed_by.is_human() {
            return Err(ValidationError::NotHumanActor {
                actor: confirmed_by.to_string(),
            });
        }
        let baseline = AccessBaseline {
            workspace: self.workspace,
            task: self.task,
            target: self.target,
            paths: self.paths,
            inventory: self.inventory.canonicalised(),
            origin: self.origin,
            confirmed_by,
            confirmed_at,
        };
        baseline.validate()?;
        Ok(baseline)
    }
}

impl AccessBaseline {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.confirmed_by.is_human() {
            return Err(ValidationError::NotHumanActor {
                actor: self.confirmed_by.to_string(),
            });
        }
        if self.paths.is_empty() {
            return Err(ValidationError::EmptyField { field: "paths" });
        }
        self.paths
            .iter()
            .try_for_each(BaselinePathEntry::validate)?;
        Ok(())
    }

    /// Whether the baseline admits a read of `path`, before absolute exclusions
    /// are applied.
    ///
    /// Exclusions are applied *before* grants by the snapshot builder, so a
    /// caller must consult both; this function answers only the grants ∪ pins
    /// half of the question.
    pub fn admits(&self, path: &RepoPath) -> bool {
        self.paths.iter().any(|entry| entry.admits(path))
    }

    /// The granted subtrees, which is what the snapshot builder materialises.
    pub fn grants(&self) -> impl Iterator<Item = &RepoPath> {
        self.paths.iter().filter_map(|entry| match entry {
            BaselinePathEntry::SubtreeGrant { path, .. } => Some(path),
            _ => None,
        })
    }

    /// The individually confirmed out-of-scope paths.
    pub fn pins(&self) -> impl Iterator<Item = &RepoPath> {
        self.paths.iter().filter_map(|entry| match entry {
            BaselinePathEntry::FilePin { path, .. }
            | BaselinePathEntry::SubtreePin { path, .. } => Some(path),
            BaselinePathEntry::SubtreeGrant { .. } => None,
        })
    }

    /// Digest over the canonical encoding, shown in the mission envelope summary.
    pub fn digest(&self) -> Result<Digest, CanonicalError> {
        let mut canonical = self.clone();
        canonical.paths.sort_by(|a, b| a.path().cmp(b.path()));
        canonical.inventory = canonical.inventory.canonicalised();
        Digest::of_canonical("clyde.access-baseline.v1", &canonical)
    }

    /// The key a baseline is stored under: workspace, task type, and target.
    pub fn key(&self) -> BaselineKey {
        BaselineKey {
            workspace: self.workspace.clone(),
            task: self.task,
            target: self.target.clone(),
        }
    }
}

/// Storage key for a baseline.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BaselineKey {
    pub workspace: WorkspaceId,
    pub task: TaskType,
    pub target: RepoPath,
}

/// A detected deviation from the pinned baseline.
///
/// Path drift and dependency drift are distinct variants because they carry very
/// different signal: the first is usually a build script reaching outside the
/// approved scope, the second is what the control exists for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "drift", rename_all = "snake_case")]
pub enum AccessDrift {
    /// A read outside grants ∪ pins. Never triggered by a new file inside a grant.
    PathOutsideBaseline { path: RepoPath },
    /// A new in-repo path dependency on a crate outside the approved scope.
    ClosureLeftApprovedScope { path: RepoPath, dependent: String },
    NewCodeExecCrate {
        crate_name: String,
        version: String,
        kind: CodeExecKind,
    },
    CodeExecVersionChanged {
        crate_name: String,
        from: String,
        to: String,
    },
    /// Same version, different source content: a registry-tampering signal.
    CodeExecContentChanged { crate_name: String, version: String },
}

impl AccessDrift {
    /// Whether this is dependency drift rather than path drift, which decides
    /// how the escalation is presented.
    pub fn is_dependency_drift(&self) -> bool {
        matches!(
            self,
            Self::NewCodeExecCrate { .. }
                | Self::CodeExecVersionChanged { .. }
                | Self::CodeExecContentChanged { .. }
        )
    }

    /// Whether this drift must be reported with distinct visual emphasis: a
    /// same-version content change is tampering, not an upgrade.
    pub fn is_tampering_signal(&self) -> bool {
        matches!(self, Self::CodeExecContentChanged { .. })
    }

    /// One-line rendering for an approval prompt.
    pub fn render(&self) -> String {
        match self {
            Self::PathOutsideBaseline { path } => {
                format!("path outside the confirmed baseline: {path}")
            }
            Self::ClosureLeftApprovedScope { path, dependent } => format!(
                "{dependent} now depends on {path}, which is outside the mission's approved scope"
            ),
            Self::NewCodeExecCrate {
                crate_name,
                version,
                kind,
            } => format!("+ {crate_name} {version}   (new {kind} crate)"),
            Self::CodeExecVersionChanged {
                crate_name,
                from,
                to,
            } => format!("~ {crate_name} {from} -> {to}   (build-time code changed)"),
            Self::CodeExecContentChanged {
                crate_name,
                version,
            } => format!("! {crate_name} {version}   (same version, different source hash)"),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::ids;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn baseline(paths: Vec<BaselinePathEntry>) -> AccessBaseline {
        AccessBaseline {
            workspace: ids::new::workspace_id().unwrap(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths,
            inventory: CodeExecInventory::default(),
            origin: BaselineOrigin::StaticClosure,
            confirmed_by: ActorId::parse("human:andrew").unwrap(),
            confirmed_at: Utc::now(),
        }
    }

    #[test]
    fn a_grant_admits_its_whole_subtree_including_new_files() {
        let baseline = baseline(vec![BaselinePathEntry::SubtreeGrant {
            path: path("crates/core"),
            source: GrantSource::MissionEditScope,
        }]);
        assert!(baseline.admits(&path("crates/core/src/lib.rs")));
        // The property that keeps the inner loop prompt-free: a file that did
        // not exist when the baseline was confirmed is still admitted.
        assert!(baseline.admits(&path("crates/core/src/brand_new_module.rs")));
        assert!(baseline.admits(&path("crates/core/tests/new_test.rs")));
        assert!(!baseline.admits(&path("crates/policy/src/lib.rs")));
    }

    #[test]
    fn a_file_pin_admits_exactly_one_path() {
        let baseline = baseline(vec![BaselinePathEntry::FilePin {
            path: path("docs/schema.sql"),
            blake3: None,
            reason: "include_str! target".to_owned(),
        }]);
        assert!(baseline.admits(&path("docs/schema.sql")));
        assert!(!baseline.admits(&path("docs/other.sql")));
        assert!(!baseline.admits(&path("docs")));
    }

    #[test]
    fn only_grants_are_drift_insensitive() {
        let grant = BaselinePathEntry::SubtreeGrant {
            path: path("src"),
            source: GrantSource::BuildClosure,
        };
        let pin = BaselinePathEntry::FilePin {
            path: path("docs/x"),
            blake3: None,
            reason: "r".to_owned(),
        };
        let subtree_pin = BaselinePathEntry::SubtreePin {
            path: path("fixtures"),
            reason: "r".to_owned(),
        };
        assert!(!grant.is_drift_sensitive());
        assert!(pin.is_drift_sensitive());
        assert!(subtree_pin.is_drift_sensitive());
    }

    #[test]
    fn pins_require_a_reason() {
        let pin = BaselinePathEntry::FilePin {
            path: path("docs/x"),
            blake3: None,
            reason: "  ".to_owned(),
        };
        assert!(pin.validate().is_err());
        let subtree_pin = BaselinePathEntry::SubtreePin {
            path: path("fixtures"),
            reason: String::new(),
        };
        assert!(subtree_pin.validate().is_err());
    }

    #[test]
    fn a_proposal_only_becomes_a_baseline_when_a_human_confirms() {
        let proposal = BaselineProposal {
            workspace: ids::new::workspace_id().unwrap(),
            task: TaskType::RustCheck,
            target: path("crates/core"),
            paths: vec![BaselinePathEntry::SubtreeGrant {
                path: path("crates/core"),
                source: GrantSource::MissionEditScope,
            }],
            inventory: CodeExecInventory::default(),
            origin: BaselineOrigin::Learned,
            proposed_at: Utc::now(),
            rationale: vec!["observed in learn mode".to_owned()],
        };
        let by_agent = proposal
            .clone()
            .confirm(ActorId::parse("agent:claude").unwrap(), Utc::now());
        assert!(
            matches!(by_agent, Err(ValidationError::NotHumanActor { .. })),
            "an actor must not be able to confirm its own baseline"
        );
        assert!(
            proposal
                .confirm(ActorId::parse("human:andrew").unwrap(), Utc::now())
                .is_ok()
        );
    }

    #[test]
    fn an_empty_baseline_is_invalid() {
        assert!(baseline(vec![]).validate().is_err());
    }

    #[test]
    fn digest_is_order_independent_over_paths_and_inventory() {
        let entry_a = BaselinePathEntry::SubtreeGrant {
            path: path("a"),
            source: GrantSource::MissionEditScope,
        };
        let entry_b = BaselinePathEntry::FilePin {
            path: path("b/c"),
            blake3: None,
            reason: "r".to_owned(),
        };
        let one = baseline(vec![entry_a.clone(), entry_b.clone()]);
        let other = AccessBaseline {
            paths: vec![entry_b, entry_a],
            ..one.clone()
        };
        assert_eq!(one.digest().unwrap(), other.digest().unwrap());
    }

    #[test]
    fn drift_classification_separates_tampering_from_upgrade() {
        let tampered = AccessDrift::CodeExecContentChanged {
            crate_name: "zstd-sys".to_owned(),
            version: "2.0.10".to_owned(),
        };
        let upgraded = AccessDrift::CodeExecVersionChanged {
            crate_name: "ring".to_owned(),
            from: "0.17.8".to_owned(),
            to: "0.17.9".to_owned(),
        };
        let path_drift = AccessDrift::PathOutsideBaseline {
            path: path("crates/proto/schema.sql"),
        };
        assert!(tampered.is_tampering_signal());
        assert!(!upgraded.is_tampering_signal());
        assert!(tampered.is_dependency_drift());
        assert!(upgraded.is_dependency_drift());
        assert!(!path_drift.is_dependency_drift());
        assert!(tampered.render().starts_with('!'));
        assert!(upgraded.render().starts_with('~'));
    }

    #[test]
    fn inventory_canonicalisation_is_stable() {
        let make = |name: &str| CodeExecEntry {
            crate_name: name.to_owned(),
            version: "1.0.0".to_owned(),
            kind: CodeExecKind::BuildScript,
            source_blake3: Digest::of_bytes(name.as_bytes()),
        };
        let one = CodeExecInventory {
            lockfile_digest: None,
            entries: vec![make("b"), make("a"), make("a")],
        }
        .canonicalised();
        let other = CodeExecInventory {
            lockfile_digest: None,
            entries: vec![make("a"), make("b")],
        }
        .canonicalised();
        assert_eq!(one, other);
        assert_eq!(one.entries.len(), 2);
        assert!(one.find("a").is_some());
        assert!(one.find("c").is_none());
    }
}
