//! `git.commit.prepare`.
//!
//! Commit creation is a trusted clyded operation, not an agent operation, and
//! `.git` is read-only inside workspace environments — otherwise an agent could
//! plant a `pre-push` hook (D8).
//!
//! The commit is built with plumbing against a temporary index, so the
//! developer's own index is never disturbed by Clyde preparing a proposal.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::diff::{DiffStat, WorkspaceDiff, workspace_diff};
use crate::{GitError, GitRunner, Identity, Result};

/// A commit proposal, stored as an artifact and shown for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitProposal {
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    /// Paths the commit would include, workspace-relative.
    pub files: Vec<String>,
    pub stat: DiffStat,
    /// The commit this would be built on.
    pub parent: String,
    /// The tree the proposal builds, which binds the approval to content rather
    /// than to a message.
    pub tree: String,
    pub prepared_at: DateTime<Utc>,
}

impl CommitProposal {
    /// Digest over the normalised proposal, so an approval covers exactly this
    /// content.
    pub fn digest(
        &self,
    ) -> std::result::Result<clyde_core::Digest, clyde_core::digest::CanonicalError> {
        let normalised = serde_json::json!({
            "message": self.message,
            "author_name": self.author_name,
            "author_email": self.author_email,
            "files": self.files,
            "parent": self.parent,
            "tree": self.tree,
        });
        clyde_core::Digest::of_canonical("clyde.commit-proposal.v1", &normalised)
    }
}

const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Builds a commit proposal from the live working tree, scoped to `paths`.
///
/// Nothing is committed: the tree object is written, so the proposal names
/// exactly what would be committed, and the commit itself is created only after
/// a human approves.
pub async fn prepare_commit_proposal(
    runner: &GitRunner,
    repository: &Path,
    scratch: &Path,
    message: &str,
    identity: &Identity,
    paths: &[String],
) -> Result<CommitProposal> {
    if message.trim().is_empty() || message.len() > MAX_MESSAGE_BYTES {
        return Err(GitError::Invalid {
            kind: "commit message",
            value: message.chars().take(64).collect(),
        });
    }
    let parent = runner.rev_parse(repository, "HEAD").await?;
    let index = temporary_index(scratch)?;

    // Start from HEAD so the proposal is a delta, then stage only the scoped
    // paths, so a change outside the mission's scope cannot ride along.
    runner
        .run(
            runner
                .command(["read-tree", "HEAD"])
                .in_repository(repository)
                .with_index_file(&index),
        )
        .await?;
    let mut add = vec!["add".to_owned(), "--all".to_owned(), "--".to_owned()];
    if paths.is_empty() {
        add.push(".".to_owned());
    } else {
        add.extend(paths.iter().cloned());
    }
    runner
        .run(
            runner
                .command(add)
                .in_repository(repository)
                .with_index_file(&index),
        )
        .await?;
    let tree = runner
        .run(
            runner
                .command(["write-tree"])
                .in_repository(repository)
                .with_index_file(&index),
        )
        .await?
        .first_line()
        .to_owned();

    let diff = workspace_diff(runner, repository, paths).await?;
    let files = collect_files(&diff);
    let _ = std::fs::remove_file(&index);

    Ok(CommitProposal {
        message: message.to_owned(),
        author_name: identity.name.clone(),
        author_email: identity.email.clone(),
        files,
        stat: diff.stat,
        parent,
        tree,
        prepared_at: Utc::now(),
    })
}

/// Creates the commit a proposal describes and moves the branch to it.
///
/// Called only after approval. The branch update is conditional on the parent
/// still being where the proposal said it was, so a tree that moved underneath
/// the approval fails rather than silently rebasing it.
pub async fn create_commit(
    runner: &GitRunner,
    repository: &Path,
    proposal: &CommitProposal,
    identity: &Identity,
) -> Result<String> {
    let commit = runner
        .run(
            runner
                .command([
                    "commit-tree".to_owned(),
                    proposal.tree.clone(),
                    "-p".to_owned(),
                    proposal.parent.clone(),
                    "-m".to_owned(),
                    proposal.message.clone(),
                ])
                .in_repository(repository)
                .as_identity(identity.clone()),
        )
        .await?
        .first_line()
        .to_owned();
    clyde_core::task::validate_git_object_id(&commit).map_err(|_| GitError::Unparsable {
        subcommand: "commit-tree".to_owned(),
        detail: "did not return a commit id".to_owned(),
    })?;

    let branch = runner
        .current_branch(repository)
        .await?
        .ok_or_else(|| GitError::Invalid {
            kind: "branch",
            value: "detached HEAD".to_owned(),
        })?;
    runner
        .run(
            runner
                .command([
                    "update-ref".to_owned(),
                    format!("refs/heads/{branch}"),
                    commit.clone(),
                    proposal.parent.clone(),
                ])
                .in_repository(repository),
        )
        .await?;
    Ok(commit)
}

fn collect_files(diff: &WorkspaceDiff) -> Vec<String> {
    let mut files = diff.changed_paths.clone();
    files.extend(diff.untracked_paths.iter().cloned());
    files.sort();
    files.dedup();
    files
}

/// A temporary index file, so the developer's index is untouched.
fn temporary_index(scratch: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(scratch)
        .map_err(|error| GitError::io("creating the git scratch directory", error))?;
    let path = scratch.join(format!("index-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Ok(path)
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
    fn a_proposal_digest_covers_content_not_just_the_message() {
        let base = CommitProposal {
            message: "fix".to_owned(),
            author_name: "Andrew".to_owned(),
            author_email: "a@example.test".to_owned(),
            files: vec!["src/lib.rs".to_owned()],
            stat: DiffStat::default(),
            parent: "a".repeat(40),
            tree: "b".repeat(40),
            prepared_at: Utc::now(),
        };
        let same_message_other_tree = CommitProposal {
            tree: "c".repeat(40),
            ..base.clone()
        };
        assert_ne!(
            base.digest().unwrap(),
            same_message_other_tree.digest().unwrap(),
            "an approval must not carry over to different content"
        );
        let later = CommitProposal {
            prepared_at: Utc::now() + chrono::Duration::seconds(30),
            ..base.clone()
        };
        assert_eq!(
            base.digest().unwrap(),
            later.digest().unwrap(),
            "the preparation timestamp is not part of what is approved"
        );
    }
}
