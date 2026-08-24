//! Absolute snapshot exclusions.
//!
//! This is the authoritative encoding of the canonical exclusion list (Phase 2a
//! deliverable 3). Exclusions are applied *before* grants and cannot be admitted
//! by one: a subtree grant over `backend/auth` does not admit
//! `backend/auth/.env.local`.
//!
//! Repository configuration may **add** exclusions and never remove them, since
//! narrowing is always permitted (D14).

use std::collections::BTreeSet;

use clyde_core::repo_path::RepoPath;

/// Why a path was excluded. Recorded on the snapshot manifest so a reviewer can
/// see which rule fired.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExclusionRule {
    /// Build output. Belongs in the mission cache, not the snapshot.
    BuildOutput(&'static str),
    /// Secret-shaped. Never an input to a build.
    SecretShaped(&'static str),
    /// Repository history. Never available to build tasks and not pinnable (D21).
    GitDirectory,
    /// Clyde's own state must never be a build input.
    ClydeState,
    /// A pattern added by configuration.
    Configured(String),
}

impl ExclusionRule {
    /// Stable name for the manifest and for diagnostics.
    pub fn name(&self) -> String {
        match self {
            Self::BuildOutput(pattern) => format!("build-output:{pattern}"),
            Self::SecretShaped(pattern) => format!("secret-shaped:{pattern}"),
            Self::GitDirectory => "git-directory:.git".to_owned(),
            Self::ClydeState => "clyde-state:.clyde/state".to_owned(),
            Self::Configured(pattern) => format!("configured:{pattern}"),
        }
    }

    /// Whether this rule is the `.git` exclusion, which needs its own
    /// diagnostic: a build reading `.git` is neither drift nor a bug in the
    /// user's code, and git metadata is a known MVP gap (D21).
    pub fn is_git(&self) -> bool {
        matches!(self, Self::GitDirectory)
    }
}

/// Directory names whose entire subtree is build output.
const BUILD_OUTPUT_DIRS: [&str; 4] = ["target", "node_modules", "dist", "build"];

/// Exact file names that are secret-shaped.
const SECRET_FILES: [&str; 4] = [".env", "credentials.json", ".netrc", ".npmrc"];

/// Extensions that carry key material.
const SECRET_EXTENSIONS: [&str; 5] = ["pem", "key", "p12", "pfx", "jks"];

/// Directory names that hold secrets by convention.
const SECRET_DIRS: [&str; 3] = ["secrets", ".ssh", ".gnupg"];

/// Whether `path` is absolutely excluded, and by which rule.
///
/// Checks every component, not just the leaf, so `target/debug/x` and
/// `crates/core/target/debug/x` are both excluded.
pub fn absolute_exclusion(path: &RepoPath) -> Option<ExclusionRule> {
    for component in path.components() {
        if component == ".git" {
            return Some(ExclusionRule::GitDirectory);
        }
        if let Some(matched) = BUILD_OUTPUT_DIRS
            .into_iter()
            .find(|candidate| *candidate == component)
        {
            return Some(ExclusionRule::BuildOutput(matched));
        }
        if let Some(matched) = SECRET_DIRS
            .into_iter()
            .find(|candidate| *candidate == component)
        {
            return Some(ExclusionRule::SecretShaped(matched));
        }
        if let Some(matched) = SECRET_FILES
            .into_iter()
            .find(|candidate| *candidate == component)
        {
            return Some(ExclusionRule::SecretShaped(matched));
        }
        // `.env.local`, `.env.production`, and friends.
        if component.starts_with(".env.") || component == ".env" {
            return Some(ExclusionRule::SecretShaped(".env*"));
        }
        // `id_rsa`, `id_ed25519`, and their `.pub` companions: the private form
        // is the risk, and excluding both is cheaper than distinguishing them.
        if component.starts_with("id_rsa") || component.starts_with("id_ed25519") {
            return Some(ExclusionRule::SecretShaped("id_rsa*"));
        }
        if let Some(extension) = component.rsplit_once('.').map(|(_, ext)| ext)
            && let Some(matched) = SECRET_EXTENSIONS
                .into_iter()
                .find(|candidate| candidate.eq_ignore_ascii_case(extension))
        {
            return Some(ExclusionRule::SecretShaped(matched));
        }
    }

    // `.clyde/state`: Clyde's own state, including access baselines, must never
    // be a build input.
    let mut components = path.components();
    if components.next() == Some(".clyde") && components.next() == Some("state") {
        return Some(ExclusionRule::ClydeState);
    }

    None
}

/// Whether `path` matches a configured exclusion pattern.
///
/// Patterns are deliberately simple: an exact path, a subtree prefix, or a
/// `*.ext` suffix. A full glob language would be a source of surprises in a
/// security control.
pub fn configured_exclusion(path: &RepoPath, patterns: &BTreeSet<String>) -> Option<ExclusionRule> {
    patterns
        .iter()
        .find(|pattern| matches_pattern(path, pattern))
        .map(|pattern| ExclusionRule::Configured(pattern.clone()))
}

fn matches_pattern(path: &RepoPath, pattern: &str) -> bool {
    if let Some(extension) = pattern.strip_prefix("*.") {
        return path
            .file_name()
            .and_then(|name| name.rsplit_once('.'))
            .is_some_and(|(_, ext)| ext == extension);
    }
    match RepoPath::parse(pattern) {
        Ok(prefix) => path.is_within(&prefix),
        Err(_) => false,
    }
}

/// Whether `path` is excluded by any rule, absolute or configured.
///
/// Absolute rules are checked first, so a configured pattern cannot shadow one
/// and change the reported reason.
pub fn exclusion_for(path: &RepoPath, configured: &BTreeSet<String>) -> Option<ExclusionRule> {
    absolute_exclusion(path).or_else(|| configured_exclusion(path, configured))
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

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    #[test]
    fn git_is_excluded_anywhere_and_reported_specifically() {
        let rule = absolute_exclusion(&path(".git/config")).expect("excluded");
        assert!(rule.is_git());
        assert!(absolute_exclusion(&path("submodule/.git/HEAD")).is_some());
        // The exclusion is absolute: no configuration or pin can admit it (D21).
        assert!(
            absolute_exclusion(&path(".git")).is_some_and(|rule| rule.is_git()),
            ".git itself must be excluded"
        );
    }

    #[test]
    fn secret_shaped_files_inside_a_granted_subtree_are_excluded() {
        // The property the `secret-shaped-files` fixture asserts: a grant over
        // backend/auth does not admit backend/auth/.env.local.
        for candidate in [
            "backend/auth/.env.local",
            "backend/auth/.env",
            "config/server.pem",
            "config/server.KEY",
            "deploy/id_rsa",
            "deploy/id_ed25519.pub",
            "secrets/token",
            "home/.ssh/config",
            "home/.gnupg/secring.gpg",
            "credentials.json",
            "app/.netrc",
        ] {
            assert!(
                absolute_exclusion(&path(candidate)).is_some(),
                "{candidate} must be excluded"
            );
        }
    }

    #[test]
    fn ordinary_source_files_are_not_excluded() {
        for candidate in [
            "crates/core/src/lib.rs",
            "docs/readme.md",
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "rust-toolchain.toml",
            "tests/fixtures/env.rs",
            "src/environment.rs",
            "src/keyboard.rs",
        ] {
            assert_eq!(
                absolute_exclusion(&path(candidate)),
                None,
                "{candidate} must not be excluded"
            );
        }
    }

    #[test]
    fn build_output_is_excluded_at_any_depth() {
        assert!(absolute_exclusion(&path("target/debug/app")).is_some());
        assert!(absolute_exclusion(&path("crates/core/target/debug/app")).is_some());
        assert!(absolute_exclusion(&path("web/node_modules/react/index.js")).is_some());
        assert!(absolute_exclusion(&path("dist/bundle.js")).is_some());
    }

    #[test]
    fn clyde_state_is_excluded_but_policy_is_not() {
        assert_eq!(
            absolute_exclusion(&path(".clyde/state/db")),
            Some(ExclusionRule::ClydeState)
        );
        assert_eq!(absolute_exclusion(&path(".clyde/policy.toml")), None);
    }

    #[test]
    fn configured_patterns_add_exclusions() {
        let patterns: BTreeSet<String> = ["fixtures/large".to_owned(), "*.bin".to_owned()]
            .into_iter()
            .collect();
        assert!(configured_exclusion(&path("fixtures/large/blob"), &patterns).is_some());
        assert!(configured_exclusion(&path("data/model.bin"), &patterns).is_some());
        assert!(configured_exclusion(&path("src/lib.rs"), &patterns).is_none());
        // A malformed pattern is inert rather than an error, since a repository
        // adding a bad pattern should not break a build.
        let bad: BTreeSet<String> = ["../escape".to_owned()].into_iter().collect();
        assert!(configured_exclusion(&path("src/lib.rs"), &bad).is_none());
    }

    #[test]
    fn absolute_rules_take_precedence_over_configured_ones() {
        let patterns: BTreeSet<String> = [".git".to_owned()].into_iter().collect();
        let rule = exclusion_for(&path(".git/config"), &patterns).expect("excluded");
        assert!(
            rule.is_git(),
            "the reported reason must be the absolute rule, so the diagnostic is right"
        );
    }

    #[test]
    fn rule_names_are_stable() {
        assert_eq!(ExclusionRule::GitDirectory.name(), "git-directory:.git");
        assert_eq!(
            ExclusionRule::SecretShaped(".env*").name(),
            "secret-shaped:.env*"
        );
        assert_eq!(
            ExclusionRule::Configured("*.bin".to_owned()).name(),
            "configured:*.bin"
        );
    }
}
