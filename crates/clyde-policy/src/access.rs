//! Access baseline drift and lockfile change classification (D18, Phase 3).
//!
//! Pure comparisons over pinned and observed state. The point of separating them
//! from the snapshot and fetch machinery is that the security-relevant question
//! — *what changed* — is answerable without I/O and is therefore cheap to test
//! exhaustively.

use std::collections::{BTreeMap, BTreeSet};

use clyde_core::baseline::{
    AccessBaseline, AccessDrift, CodeExecEntry, CodeExecInventory, CodeExecKind,
};
use clyde_core::repo_path::RepoPath;

/// Compares a pinned inventory against a freshly computed one.
///
/// Ordering matters in the caller: this comparison runs **before** the sandbox
/// starts, so a newly arrived build script is caught before it runs.
///
/// Removals are not drift. A dependency that no longer executes code at build
/// time is a reduction in exposure, and prompting for it would train the human
/// to click through.
pub fn inventory_drift(
    pinned: &CodeExecInventory,
    current: &CodeExecInventory,
) -> Vec<AccessDrift> {
    let pinned_by_name: BTreeMap<&str, &CodeExecEntry> = pinned
        .entries
        .iter()
        .map(|entry| (entry.crate_name.as_str(), entry))
        .collect();

    current
        .entries
        .iter()
        .filter_map(
            |entry| match pinned_by_name.get(entry.crate_name.as_str()) {
                None => Some(AccessDrift::NewCodeExecCrate {
                    crate_name: entry.crate_name.clone(),
                    version: entry.version.clone(),
                    kind: entry.kind,
                }),
                Some(pinned) if pinned.version != entry.version => {
                    Some(AccessDrift::CodeExecVersionChanged {
                        crate_name: entry.crate_name.clone(),
                        from: pinned.version.clone(),
                        to: entry.version.clone(),
                    })
                }
                // Same version, different content: a registry-tampering signal,
                // reported distinctly from an upgrade.
                Some(pinned) if pinned.source_blake3 != entry.source_blake3 => {
                    Some(AccessDrift::CodeExecContentChanged {
                        crate_name: entry.crate_name.clone(),
                        version: entry.version.clone(),
                    })
                }
                Some(_) => None,
            },
        )
        .collect()
}

/// Compares observed reads against a confirmed baseline.
///
/// A read inside a subtree grant is never drift, whatever its filename and
/// whether or not it existed when the baseline was confirmed. That is the
/// property that keeps first-party development prompt-free.
pub fn path_drift(baseline: &AccessBaseline, observed_reads: &[RepoPath]) -> Vec<AccessDrift> {
    let mut seen: BTreeSet<&RepoPath> = BTreeSet::new();
    observed_reads
        .iter()
        .filter(|path| seen.insert(path))
        .filter(|path| !baseline.admits(path))
        .map(|path| AccessDrift::PathOutsideBaseline { path: path.clone() })
        .collect()
}

/// Classifies a newly required in-repo path dependency.
///
/// A new path dependency on a crate *inside* the approved scope is ordinary
/// work. One *outside* it is a genuine widening of what the mission reaches
/// into, and rare enough to be worth a prompt.
pub fn closure_drift(
    baseline: &AccessBaseline,
    dependency_path: &RepoPath,
    dependent: &str,
) -> Option<AccessDrift> {
    (!baseline.admits(dependency_path)).then(|| AccessDrift::ClosureLeftApprovedScope {
        path: dependency_path.clone(),
        dependent: dependent.to_owned(),
    })
}

/// Where a locked package comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
    /// A registry, by its index URL.
    Registry { index: String },
    /// A git dependency. Never pre-approvable.
    Git { url: String, rev: Option<String> },
    /// An in-repository path dependency.
    Path,
}

impl PackageSource {
    pub fn is_git(&self) -> bool {
        matches!(self, Self::Git { .. })
    }

    pub fn render(&self) -> String {
        match self {
            Self::Registry { index } => index.clone(),
            Self::Git { url, rev } => match rev {
                Some(rev) => format!("git+{url}#{rev}"),
                None => format!("git+{url}"),
            },
            Self::Path => "path".to_owned(),
        }
    }
}

/// One locked package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: PackageSource,
}

/// A lockfile reduced to what the fetch policy needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockfileSummary {
    pub packages: Vec<LockedPackage>,
}

impl LockfileSummary {
    fn by_name(&self) -> BTreeMap<&str, &LockedPackage> {
        self.packages
            .iter()
            .map(|package| (package.name.as_str(), package))
            .collect()
    }
}

/// How a lockfile changed, in the terms the approval prompt uses.
///
/// "Fetch dependencies" and "fetch dependencies, including four new crates from
/// a source you have not used before" deserve different scrutiny, and only Clyde
/// is in a position to tell them apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LockfileChange {
    pub additions: Vec<LockedPackage>,
    pub removals: Vec<LockedPackage>,
    pub version_changes: Vec<VersionChange>,
    /// Same version, different source: the registry-tampering shape.
    pub source_changes: Vec<LockedPackage>,
    pub new_git_dependencies: Vec<LockedPackage>,
    /// Registry index URLs not present in the previously satisfied lockfile.
    pub new_registries: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VersionChange {
    pub name: String,
    pub from: String,
    pub to: String,
}

impl LockfileChange {
    pub fn is_unchanged(&self) -> bool {
        self.additions.is_empty()
            && self.removals.is_empty()
            && self.version_changes.is_empty()
            && self.source_changes.is_empty()
            && self.new_git_dependencies.is_empty()
    }

    /// Whether every change in this diff belongs to a class configuration may
    /// pre-approve.
    ///
    /// Never eligible, whatever configuration says:
    /// - a git dependency
    /// - a package from a registry this lockfile has not used before, which is
    ///   the "four new crates from a source you have not used before" case the
    ///   prompt exists to distinguish
    /// - a source change at the same version, which is a tampering shape
    ///
    /// A first fetch, where nothing was previously satisfied, therefore always
    /// reaches a human: every source in it is new.
    pub fn is_pre_approvable(&self, allow_additions: bool, allow_version_changes: bool) -> bool {
        if !self.new_git_dependencies.is_empty() || !self.new_registries.is_empty() {
            return false;
        }
        if !self.source_changes.is_empty() {
            return false;
        }
        if !self.additions.is_empty() && !allow_additions {
            return false;
        }
        if !self.version_changes.is_empty() && !allow_version_changes {
            return false;
        }
        true
    }

    /// The one-line summary shown above the inventory diff.
    pub fn render_summary(&self) -> String {
        format!(
            "{} additions, {} version changes, {} source changes",
            self.additions.len(),
            self.version_changes.len(),
            self.source_changes.len()
        )
    }
}

/// Records a registry index the previous lockfile did not use.
fn record_new_registry(
    source: &PackageSource,
    previously_used: &BTreeSet<&str>,
    new_registries: &mut BTreeSet<String>,
) {
    if let PackageSource::Registry { index } = source
        && !previously_used.contains(index.as_str())
    {
        new_registries.insert(index.clone());
    }
}

/// Compares two lockfile summaries.
pub fn classify_lockfile_change(
    previous: &LockfileSummary,
    current: &LockfileSummary,
) -> LockfileChange {
    let previous_by_name = previous.by_name();
    let current_by_name = current.by_name();
    let previous_registries: BTreeSet<&str> = previous
        .packages
        .iter()
        .filter_map(|package| match &package.source {
            PackageSource::Registry { index } => Some(index.as_str()),
            _ => None,
        })
        .collect();

    let mut change = LockfileChange::default();

    for package in &current.packages {
        match previous_by_name.get(package.name.as_str()) {
            None => {
                change.additions.push(package.clone());
                if package.source.is_git() {
                    change.new_git_dependencies.push(package.clone());
                }
                record_new_registry(
                    &package.source,
                    &previous_registries,
                    &mut change.new_registries,
                );
            }
            Some(before) if before.version != package.version => {
                change.version_changes.push(VersionChange {
                    name: package.name.clone(),
                    from: before.version.clone(),
                    to: package.version.clone(),
                });
                if package.source.is_git() && !before.source.is_git() {
                    change.new_git_dependencies.push(package.clone());
                }
            }
            Some(before) if before.source != package.source => {
                change.source_changes.push(package.clone());
                if package.source.is_git() {
                    change.new_git_dependencies.push(package.clone());
                }
                record_new_registry(
                    &package.source,
                    &previous_registries,
                    &mut change.new_registries,
                );
            }
            Some(_) => {}
        }
    }

    for package in &previous.packages {
        if !current_by_name.contains_key(package.name.as_str()) {
            change.removals.push(package.clone());
        }
    }

    change
}

/// Whether a fetch may proceed at all, before approval is considered.
///
/// Git dependencies and unknown registries are refused with an actionable
/// explanation rather than half-supported (Phase 3 exit criteria).
pub fn fetch_admissibility(
    change: &LockfileChange,
    allowed_registries: &BTreeSet<clyde_core::classification::HostName>,
) -> Result<(), FetchRefusal> {
    if let Some(package) = change.new_git_dependencies.first() {
        return Err(FetchRefusal::GitDependency {
            name: package.name.clone(),
            origin: package.source.render(),
        });
    }
    for index in &change.new_registries {
        let host = registry_host(index);
        let known = host
            .as_deref()
            .and_then(|host| clyde_core::classification::HostName::parse(host).ok())
            .is_some_and(|host| allowed_registries.contains(&host));
        if !known {
            return Err(FetchRefusal::UnknownRegistry {
                index: index.clone(),
            });
        }
    }
    Ok(())
}

/// Extracts the host from a registry index URL, without a URL parser.
///
/// Index values are `sparse+https://host/path` or `registry+https://host/path`;
/// anything else yields `None` and is therefore treated as unknown, which is the
/// fail-closed direction.
fn registry_host(index: &str) -> Option<String> {
    let without_scheme_prefix = index
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(index);
    let host = without_scheme_prefix
        .split('/')
        .next()
        .filter(|host| !host.is_empty())?;
    // Strip credentials and port, neither of which belongs in an allowlist
    // comparison.
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split_once(':').map(|(h, _)| h).unwrap_or(host);
    Some(host.to_owned())
}

/// Why a fetch was refused outright.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchRefusal {
    #[error(
        "{name} is a git dependency ({origin}); git dependencies are not supported in this version and are refused rather than half-supported"
    )]
    GitDependency { name: String, origin: String },
    #[error(
        "{index} is not a configured registry; add it to registries.allowed in host or user configuration, which a repository cannot do"
    )]
    UnknownRegistry { index: String },
}

/// Renders an inventory diff in the shape the approval prompt uses.
pub fn render_inventory_diff(drift: &[AccessDrift]) -> Vec<String> {
    drift
        .iter()
        .filter(|item| item.is_dependency_drift())
        .map(AccessDrift::render)
        .collect()
}

/// Whether any drift in the set requires human confirmation before a build may
/// run against the new bundle.
///
/// Fetching a hostile crate is harmless until something executes it, and the
/// confirmation sits in between (Phase 3 deliverable 5).
pub fn requires_confirmation_before_build(drift: &[AccessDrift]) -> bool {
    drift.iter().any(AccessDrift::is_dependency_drift)
}

/// Convenience constructor used by the snapshot and fetch code paths.
pub fn code_exec_entry(
    crate_name: &str,
    version: &str,
    kind: CodeExecKind,
    source_blake3: clyde_core::Digest,
) -> CodeExecEntry {
    CodeExecEntry {
        crate_name: crate_name.to_owned(),
        version: version.to_owned(),
        kind,
        source_blake3,
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
    use chrono::Utc;
    use clyde_core::Digest;
    use clyde_core::baseline::{BaselineOrigin, BaselinePathEntry, GrantSource};
    use clyde_core::ids::{self, ActorId};
    use clyde_core::task::TaskType;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn entry(name: &str, version: &str, content: &str) -> CodeExecEntry {
        code_exec_entry(
            name,
            version,
            CodeExecKind::BuildScript,
            Digest::of_bytes(content.as_bytes()),
        )
    }

    fn inventory(entries: Vec<CodeExecEntry>) -> CodeExecInventory {
        CodeExecInventory {
            lockfile_digest: None,
            entries,
        }
        .canonicalised()
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
    fn an_unchanged_inventory_produces_no_drift() {
        let pinned = inventory(vec![entry("ring", "0.17.8", "a")]);
        assert!(inventory_drift(&pinned, &pinned).is_empty());
    }

    #[test]
    fn a_dependency_gaining_a_build_script_is_new_code_execution() {
        // The `dep-gains-buildscript` fixture property.
        let pinned = inventory(vec![entry("ring", "0.17.8", "a")]);
        let current = inventory(vec![
            entry("ring", "0.17.8", "a"),
            entry("serde_derive_internals", "0.29.1", "b"),
        ]);
        let drift = inventory_drift(&pinned, &current);
        assert_eq!(drift.len(), 1);
        assert!(matches!(
            drift.first(),
            Some(AccessDrift::NewCodeExecCrate { .. })
        ));
        assert!(requires_confirmation_before_build(&drift));
    }

    #[test]
    fn a_version_change_and_a_content_change_are_reported_differently() {
        let pinned = inventory(vec![
            entry("ring", "0.17.8", "a"),
            entry("zstd-sys", "2.0.10", "z"),
        ]);
        let current = inventory(vec![
            entry("ring", "0.17.9", "a2"),
            // The `dep-same-version-tampered` fixture property: identical
            // version, different source content.
            entry("zstd-sys", "2.0.10", "TAMPERED"),
        ]);
        let drift = inventory_drift(&pinned, &current);
        assert_eq!(drift.len(), 2);
        let rendered = render_inventory_diff(&drift);
        assert!(
            rendered.iter().any(|line| line.starts_with('~')),
            "a version change renders as ~: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.starts_with('!')),
            "a same-version content change renders as !: {rendered:?}"
        );
        assert!(drift.iter().any(AccessDrift::is_tampering_signal));
    }

    #[test]
    fn removing_a_code_executing_crate_is_not_drift() {
        let pinned = inventory(vec![
            entry("ring", "0.17.8", "a"),
            entry("gone", "1.0.0", "g"),
        ]);
        let current = inventory(vec![entry("ring", "0.17.8", "a")]);
        assert!(
            inventory_drift(&pinned, &current).is_empty(),
            "less build-time code execution is not a change to prompt about"
        );
    }

    #[test]
    fn new_files_inside_a_grant_are_never_path_drift() {
        // The `churn` fixture property: zero drift prompts across a loop that
        // adds modules, tests, and files inside the approved scope.
        let baseline = baseline(vec![BaselinePathEntry::SubtreeGrant {
            path: path("crates/core"),
            source: GrantSource::MissionEditScope,
        }]);
        let reads = [
            "crates/core/src/lib.rs",
            "crates/core/src/brand_new.rs",
            "crates/core/tests/new_test.rs",
            "crates/core/src/nested/deeper/mod.rs",
        ]
        .into_iter()
        .map(path)
        .collect::<Vec<_>>();
        assert!(path_drift(&baseline, &reads).is_empty());
    }

    #[test]
    fn a_read_outside_grants_and_pins_is_drift() {
        let baseline = baseline(vec![
            BaselinePathEntry::SubtreeGrant {
                path: path("crates/core"),
                source: GrantSource::MissionEditScope,
            },
            BaselinePathEntry::FilePin {
                path: path("docs/schema.sql"),
                blake3: None,
                reason: "include_str! target".to_owned(),
            },
        ]);
        let reads = vec![
            path("crates/core/src/lib.rs"),
            path("docs/schema.sql"),
            path("crates/proto/schema.sql"),
        ];
        let drift = path_drift(&baseline, &reads);
        assert_eq!(drift.len(), 1);
        match drift.first() {
            Some(AccessDrift::PathOutsideBaseline { path }) => {
                assert_eq!(path.as_str(), "crates/proto/schema.sql")
            }
            other => panic!("unexpected drift: {other:?}"),
        }
        assert!(
            !requires_confirmation_before_build(&drift),
            "path drift is not dependency drift"
        );
    }

    #[test]
    fn duplicate_observed_reads_are_reported_once() {
        let baseline = baseline(vec![BaselinePathEntry::SubtreeGrant {
            path: path("crates/core"),
            source: GrantSource::MissionEditScope,
        }]);
        let reads = vec![path("outside/a"), path("outside/a"), path("outside/a")];
        assert_eq!(path_drift(&baseline, &reads).len(), 1);
    }

    #[test]
    fn a_path_dependency_inside_scope_is_not_closure_drift() {
        let baseline = baseline(vec![BaselinePathEntry::SubtreeGrant {
            path: path("crates"),
            source: GrantSource::MissionReadScope,
        }]);
        assert!(closure_drift(&baseline, &path("crates/other"), "clyde-core").is_none());
        assert!(matches!(
            closure_drift(&baseline, &path("vendor/thing"), "clyde-core"),
            Some(AccessDrift::ClosureLeftApprovedScope { .. })
        ));
    }

    fn registry(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source: PackageSource::Registry {
                index: "sparse+https://index.crates.io/".to_owned(),
            },
        }
    }

    #[test]
    fn an_identical_lockfile_is_unchanged() {
        let summary = LockfileSummary {
            packages: vec![registry("serde", "1.0.0")],
        };
        let change = classify_lockfile_change(&summary, &summary);
        assert!(change.is_unchanged());
        assert_eq!(
            change.render_summary(),
            "0 additions, 0 version changes, 0 source changes"
        );
    }

    #[test]
    fn additions_version_changes_and_source_changes_are_distinguished() {
        let previous = LockfileSummary {
            packages: vec![
                registry("serde", "1.0.0"),
                registry("ring", "0.17.8"),
                registry("zstd-sys", "2.0.10"),
                registry("dropped", "1.0.0"),
            ],
        };
        let current = LockfileSummary {
            packages: vec![
                registry("serde", "1.0.0"),
                registry("ring", "0.17.9"),
                LockedPackage {
                    name: "zstd-sys".to_owned(),
                    version: "2.0.10".to_owned(),
                    source: PackageSource::Registry {
                        index: "sparse+https://mirror.example.test/".to_owned(),
                    },
                },
                registry("new-crate", "0.1.0"),
            ],
        };
        let change = classify_lockfile_change(&previous, &current);
        assert_eq!(change.additions.len(), 1);
        assert_eq!(change.version_changes.len(), 1);
        assert_eq!(change.source_changes.len(), 1);
        assert_eq!(change.removals.len(), 1);
        assert!(
            change
                .new_registries
                .contains("sparse+https://mirror.example.test/")
        );
        assert!(!change.is_unchanged());
    }

    #[test]
    fn a_git_dependency_is_never_pre_approvable_and_is_refused() {
        let previous = LockfileSummary::default();
        let current = LockfileSummary {
            packages: vec![LockedPackage {
                name: "sketchy".to_owned(),
                version: "0.1.0".to_owned(),
                source: PackageSource::Git {
                    url: "https://example.test/sketchy".to_owned(),
                    rev: Some("abc".to_owned()),
                },
            }],
        };
        let change = classify_lockfile_change(&previous, &current);
        assert_eq!(change.new_git_dependencies.len(), 1);
        assert!(!change.is_pre_approvable(true, true));
        let refusal = fetch_admissibility(&change, &crate::egress::default_registry_hosts())
            .expect_err("git dependencies are refused");
        assert!(matches!(refusal, FetchRefusal::GitDependency { .. }));
        assert!(refusal.to_string().contains("not supported"));
    }

    #[test]
    fn an_unknown_registry_is_refused_with_an_actionable_message() {
        let previous = LockfileSummary::default();
        let current = LockfileSummary {
            packages: vec![LockedPackage {
                name: "thing".to_owned(),
                version: "1.0.0".to_owned(),
                source: PackageSource::Registry {
                    index: "sparse+https://evil.test/index/".to_owned(),
                },
            }],
        };
        let change = classify_lockfile_change(&previous, &current);
        let refusal = fetch_admissibility(&change, &crate::egress::default_registry_hosts())
            .expect_err("unknown registry refused");
        assert!(matches!(refusal, FetchRefusal::UnknownRegistry { .. }));
        assert!(refusal.to_string().contains("registries.allowed"));
    }

    #[test]
    fn a_first_fetch_always_reaches_a_human() {
        // Every source in a first fetch is one the lockfile has not used before,
        // which is exactly the case the prompt exists to distinguish.
        let previous = LockfileSummary::default();
        let current = LockfileSummary {
            packages: vec![registry("serde", "1.0.0")],
        };
        let change = classify_lockfile_change(&previous, &current);
        assert!(fetch_admissibility(&change, &crate::egress::default_registry_hosts()).is_ok());
        assert!(!change.is_pre_approvable(true, true));
    }

    #[test]
    fn an_addition_from_an_already_used_registry_is_pre_approvable() {
        let previous = LockfileSummary {
            packages: vec![registry("serde", "1.0.0")],
        };
        let current = LockfileSummary {
            packages: vec![registry("serde", "1.0.0"), registry("new-crate", "0.1.0")],
        };
        let change = classify_lockfile_change(&previous, &current);
        assert!(change.new_registries.is_empty());
        assert!(change.is_pre_approvable(true, true));
        assert!(
            !change.is_pre_approvable(false, true),
            "additions were not pre-approved by configuration"
        );
    }

    #[test]
    fn a_source_change_is_never_pre_approvable() {
        let previous = LockfileSummary {
            packages: vec![registry("zstd-sys", "2.0.10")],
        };
        let current = LockfileSummary {
            packages: vec![LockedPackage {
                name: "zstd-sys".to_owned(),
                version: "2.0.10".to_owned(),
                source: PackageSource::Registry {
                    index: "sparse+https://index.crates.io/other/".to_owned(),
                },
            }],
        };
        let change = classify_lockfile_change(&previous, &current);
        assert!(!change.source_changes.is_empty());
        assert!(!change.is_pre_approvable(true, true));
    }

    #[test]
    fn registry_host_extraction_is_fail_closed() {
        assert_eq!(
            registry_host("sparse+https://index.crates.io/"),
            Some("index.crates.io".to_owned())
        );
        assert_eq!(
            registry_host("registry+https://user:pw@index.crates.io:443/x"),
            Some("index.crates.io".to_owned())
        );
        assert_eq!(registry_host(""), None);
        // A value with no host cannot match an allowlist entry, so it is refused.
        let change = LockfileChange {
            new_registries: ["nonsense".to_owned()].into_iter().collect(),
            ..LockfileChange::default()
        };
        assert!(fetch_admissibility(&change, &crate::egress::default_registry_hosts()).is_err());
    }
}
