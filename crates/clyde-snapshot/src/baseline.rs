//! Baseline proposals.
//!
//! Clyde proposes the computed build closure as the initial baseline for human
//! confirmation. That static-proposal path exists specifically so learn mode is
//! rarely needed, since a learn run is a wide-scope execution of exactly the code
//! you are trying to constrain (D18).
//!
//! The two tiers are constructed here, and the distinction is the whole point:
//! first-party project code becomes **subtree grants**, which are not
//! drift-sensitive, and everything outside them becomes individually confirmed
//! **pins**.

use chrono::Utc;
use clyde_core::baseline::{
    BaselineOrigin, BaselinePathEntry, BaselineProposal, CodeExecInventory, GrantSource,
};
use clyde_core::ids::WorkspaceId;
use clyde_core::mission::MissionScope;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;

use crate::cargo::closure::BuildClosure;

/// Builds a static baseline proposal.
///
/// Grants come from the mission envelope's approved scope intersected with what
/// the build closure needs; anything the closure needs from outside that scope
/// becomes a pin, with a reason a later reviewer can read.
pub fn propose_from_closure(
    workspace: WorkspaceId,
    task: TaskType,
    target: RepoPath,
    scope: &MissionScope,
    closure: &BuildClosure,
) -> BaselineProposal {
    let mut paths: Vec<BaselinePathEntry> = Vec::new();
    let mut rationale = Vec::new();

    // The human already approved these subtrees when they approved the mission
    // envelope. That is where the review happened — once, over a scope rather
    // than over a file list.
    for path in &scope.edit_paths {
        paths.push(BaselinePathEntry::SubtreeGrant {
            path: path.clone(),
            source: GrantSource::MissionEditScope,
        });
    }
    for path in &scope.read_paths {
        paths.push(BaselinePathEntry::SubtreeGrant {
            path: path.clone(),
            source: GrantSource::MissionReadScope,
        });
    }

    // Source subtrees the build closure requires, where they sit inside the
    // approved scope. A subtree outside it is a widening of what the mission
    // reaches into and becomes a pin instead, so a human sees it.
    for subtree in &closure.source_subtrees {
        if already_granted(&paths, subtree) {
            continue;
        }
        if scope.may_read(subtree) {
            paths.push(BaselinePathEntry::SubtreeGrant {
                path: subtree.clone(),
                source: GrantSource::BuildClosure,
            });
            rationale.push(format!(
                "{subtree} is required by the build closure and is inside the approved scope"
            ));
        } else {
            paths.push(BaselinePathEntry::SubtreePin {
                path: subtree.clone(),
                reason: format!(
                    "the build closure for {target} requires source from {subtree}, which is outside the mission's approved scope"
                ),
            });
            rationale.push(format!(
                "{subtree} is OUTSIDE the approved scope and is proposed as a pin"
            ));
        }
    }

    // Individual files cargo discovers by walking upward. These are pins rather
    // than grants even when they sit inside the scope's parent directories,
    // because admitting a whole directory to reach one manifest would widen the
    // read set well past what the build needs.
    for file in closure.individual_files() {
        if already_granted(&paths, &file) {
            continue;
        }
        paths.push(BaselinePathEntry::FilePin {
            path: file.clone(),
            blake3: None,
            reason: format!("cargo requires {file} to construct the workspace graph"),
        });
    }

    BaselineProposal {
        workspace,
        task,
        target,
        paths,
        inventory: CodeExecInventory::default(),
        origin: BaselineOrigin::StaticClosure,
        proposed_at: Utc::now(),
        rationale,
    }
}

/// Builds a proposal from observed reads (learn mode).
///
/// Learn mode is a privilege: admin channel only, never reachable by an actor,
/// never selected by Clyde as a fallback, single run, distinctly marked in the
/// audit log, and without effect until a human confirms the proposal.
pub fn propose_from_observation(base: BaselineProposal, observed: &[RepoPath]) -> BaselineProposal {
    let mut proposal = base;
    proposal.origin = BaselineOrigin::Learned;
    for path in observed {
        if already_granted(&proposal.paths, path) {
            continue;
        }
        proposal.paths.push(BaselinePathEntry::FilePin {
            path: path.clone(),
            blake3: None,
            reason: "observed during a learn-mode run; static analysis could not see this read"
                .to_owned(),
        });
        proposal
            .rationale
            .push(format!("{path} was read during the learn run"));
    }
    proposal
}

/// Whether an entry already admits `candidate`.
fn already_granted(entries: &[BaselinePathEntry], candidate: &RepoPath) -> bool {
    entries.iter().any(|entry| entry.admits(candidate))
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
    use clyde_core::ids;

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn scope() -> MissionScope {
        MissionScope {
            edit_paths: [path("crates/core")].into_iter().collect(),
            read_paths: [path("crates/shared")].into_iter().collect(),
        }
    }

    fn closure() -> BuildClosure {
        BuildClosure {
            root_manifest: Some(path("Cargo.toml")),
            config_files: vec![path("Cargo.lock"), path(".cargo/config.toml")],
            member_manifests: vec![path("crates/other/Cargo.toml")],
            source_subtrees: vec![path("crates/core"), path("crates/shared")],
            path_dependencies: vec![("clyde-core".to_owned(), path("crates/shared"))],
        }
    }

    fn proposal() -> BaselineProposal {
        propose_from_closure(
            ids::new::workspace_id().unwrap(),
            TaskType::RustCheck,
            path("crates/core"),
            &scope(),
            &closure(),
        )
    }

    #[test]
    fn approved_scope_becomes_subtree_grants() {
        let proposal = proposal();
        let grants: Vec<&RepoPath> = proposal
            .paths
            .iter()
            .filter_map(|entry| match entry {
                BaselinePathEntry::SubtreeGrant { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert!(grants.contains(&&path("crates/core")));
        assert!(grants.contains(&&path("crates/shared")));
    }

    #[test]
    fn manifests_cargo_walks_up_to_find_become_pins_not_a_root_grant() {
        let proposal = proposal();
        let pins: Vec<&RepoPath> = proposal
            .paths
            .iter()
            .filter_map(|entry| match entry {
                BaselinePathEntry::FilePin { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert!(pins.contains(&&path("Cargo.toml")));
        assert!(pins.contains(&&path("Cargo.lock")));
        assert!(pins.contains(&&path("crates/other/Cargo.toml")));
        // Granting the repository root to reach one manifest would widen the
        // read set far past what the build needs.
        assert!(
            !proposal.paths.iter().any(|entry| matches!(
                entry,
                BaselinePathEntry::SubtreeGrant { path, .. } if path.is_root()
            )),
            "the repository root must never be granted to reach a manifest"
        );
    }

    #[test]
    fn a_closure_subtree_outside_the_approved_scope_is_a_pin_a_human_must_see() {
        let mut closure = closure();
        closure.source_subtrees.push(path("vendor/thing"));
        let proposal = propose_from_closure(
            ids::new::workspace_id().unwrap(),
            TaskType::RustCheck,
            path("crates/core"),
            &scope(),
            &closure,
        );
        let pin = proposal
            .paths
            .iter()
            .find(|entry| entry.path() == &path("vendor/thing"))
            .expect("the outside subtree must appear");
        assert!(matches!(pin, BaselinePathEntry::SubtreePin { .. }));
        assert!(pin.is_drift_sensitive());
        assert!(
            proposal
                .rationale
                .iter()
                .any(|line| line.contains("OUTSIDE")),
            "the human must be able to see which entries widen the mission's reach"
        );
    }

    #[test]
    fn a_proposal_is_confirmable_and_admits_what_it_names() {
        let proposal = proposal();
        let baseline = proposal
            .confirm(
                clyde_core::ids::ActorId::parse("human:andrew").unwrap(),
                Utc::now(),
            )
            .expect("a human confirms it");
        assert!(baseline.admits(&path("crates/core/src/lib.rs")));
        assert!(baseline.admits(&path("Cargo.lock")));
        assert!(!baseline.admits(&path("crates/other/src/lib.rs")));
        assert!(
            baseline.admits(&path("crates/other/Cargo.toml")),
            "manifests only, not the other member's sources"
        );
    }

    #[test]
    fn nothing_is_duplicated_when_the_closure_repeats_the_scope() {
        let proposal = proposal();
        let mut paths: Vec<String> = proposal
            .paths
            .iter()
            .map(|entry| entry.path().to_string())
            .collect();
        let before = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), before);
    }

    #[test]
    fn learn_mode_adds_observed_reads_as_pins_and_marks_the_origin() {
        let observed = vec![
            path("docs/schema.sql"),
            // Already inside a grant: not added, because a file inside a granted
            // subtree is ordinary work.
            path("crates/core/src/lib.rs"),
        ];
        let learned = propose_from_observation(proposal(), &observed);
        assert_eq!(learned.origin, BaselineOrigin::Learned);
        let pins: Vec<&RepoPath> = learned
            .paths
            .iter()
            .filter_map(|entry| match entry {
                BaselinePathEntry::FilePin { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert!(pins.contains(&&path("docs/schema.sql")));
        assert!(!pins.contains(&&path("crates/core/src/lib.rs")));
    }
}
