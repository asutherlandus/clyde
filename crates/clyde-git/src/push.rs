//! Brokered push, from a sanitised temporary repository (Phase 4 deliverable 5).
//!
//! A repository's `.git/config` and `.git/hooks` are attacker-controlled, and
//! `git push` executes local hooks and honours repository configuration. So the
//! broker never pushes from the workspace repository. It:
//!
//! 1. creates an empty repository in broker-owned scratch;
//! 2. fetches the approved commit from the workspace repository by path, with
//!    hooks and the object-transfer hook neutralised;
//! 3. verifies the fetched commit id and tree digest match the approval;
//! 4. adds the allowlisted remote explicitly, ignoring any remote the workspace
//!    repository configured;
//! 5. pushes with hooks disabled and system and global configuration
//!    neutralised;
//! 6. destroys the scratch repository.

use std::path::{Path, PathBuf};

use crate::{GitError, GitRunner, Result, validate_branch_refspec};

/// What a brokered push does, after validation and approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    /// The workspace repository the commit is fetched from.
    pub workspace: PathBuf,
    /// The resolved remote URL. Comes from the allowlist, never from the
    /// workspace repository's configuration.
    pub remote_url: String,
    /// `refs/heads/<branch>`.
    pub refspec: String,
    pub commit: String,
    /// The tree the approval covered. A mismatch aborts before the push.
    pub expected_tree: String,
}

/// The result of a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushResult {
    pub commit: String,
    pub refspec: String,
    pub summary: String,
}

/// Performs a brokered push from a sanitised temporary repository.
pub async fn push(
    runner: &GitRunner,
    scratch_root: &Path,
    request: &PushRequest,
) -> Result<PushResult> {
    clyde_core::task::validate_git_object_id(&request.commit).map_err(|_| GitError::Invalid {
        kind: "commit id",
        value: request.commit.clone(),
    })?;
    let branch = validate_branch_refspec(&request.refspec)?;
    validate_remote_url(&request.remote_url)?;

    let scratch = ScratchRepository::create(scratch_root, &request.commit)?;
    let result = push_from_scratch(runner, scratch.path(), request, &branch).await;
    // The scratch repository is destroyed whether or not the push succeeded, so
    // a failed push leaves no partially populated object store behind.
    scratch.destroy();
    result
}

async fn push_from_scratch(
    runner: &GitRunner,
    scratch: &Path,
    request: &PushRequest,
    branch: &str,
) -> Result<PushResult> {
    runner
        .run(
            runner
                .command(["init", "--bare", "--quiet"])
                .with_git_dir(scratch),
        )
        .await?;

    // Fetch by path. The sanitising configuration travels in the environment, so
    // the `upload-pack` process git spawns in the *workspace* repository
    // inherits it and runs no hook.
    let workspace = request.workspace.to_string_lossy().to_string();
    runner
        .run(
            runner
                .command([
                    "fetch".to_owned(),
                    "--no-tags".to_owned(),
                    "--no-write-fetch-head".to_owned(),
                    "--quiet".to_owned(),
                    workspace,
                    format!("{}:refs/clyde/pending", request.commit),
                ])
                .with_git_dir(scratch),
        )
        .await?;

    // Verify what actually arrived, rather than trusting that the fetch brought
    // what the approval described.
    let fetched = runner
        .run(
            runner
                .command(["rev-parse", "--verify", "refs/clyde/pending^{commit}"])
                .with_git_dir(scratch),
        )
        .await?
        .first_line()
        .to_owned();
    if fetched != request.commit {
        return Err(GitError::Invalid {
            kind: "fetched commit",
            value: fetched,
        });
    }
    let tree = runner
        .run(
            runner
                .command(["rev-parse", "refs/clyde/pending^{tree}"])
                .with_git_dir(scratch),
        )
        .await?
        .first_line()
        .to_owned();
    if tree != request.expected_tree {
        return Err(GitError::Invalid {
            kind: "commit tree",
            value: tree,
        });
    }

    // The remote is added explicitly from the allowlist. Any remote the
    // workspace repository configured is irrelevant: this repository has none.
    runner
        .run(
            runner
                .command([
                    "remote".to_owned(),
                    "add".to_owned(),
                    "target".to_owned(),
                    request.remote_url.clone(),
                ])
                .with_git_dir(scratch),
        )
        .await?;

    let output = runner
        .run(
            runner
                .command([
                    "push".to_owned(),
                    "--quiet".to_owned(),
                    // No force, no delete, no mirror: the refspec is a plain
                    // fast-forward of one branch.
                    "target".to_owned(),
                    format!("{}:refs/heads/{branch}", request.commit),
                ])
                .with_git_dir(scratch),
        )
        .await?;

    Ok(PushResult {
        commit: request.commit.clone(),
        refspec: request.refspec.clone(),
        summary: if output.stderr.trim().is_empty() {
            format!("pushed {} to {branch}", short(&request.commit))
        } else {
            output.stderr.trim().to_owned()
        },
    })
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// Rejects remote URLs whose transport could run a local command.
///
/// The allowlist is what decides *which* remote, but a URL that reached the
/// allowlist through configuration should still not be able to name `ext::` or a
/// local path with an embedded command.
pub fn validate_remote_url(url: &str) -> Result<()> {
    let invalid = url.is_empty()
        || url.starts_with('-')
        || url.starts_with("ext::")
        || url.contains('\n')
        || url.contains('\0');
    if invalid {
        return Err(GitError::Invalid {
            kind: "remote url",
            value: url.to_owned(),
        });
    }
    Ok(())
}

/// A temporary bare repository that removes itself.
#[derive(Debug)]
struct ScratchRepository {
    path: PathBuf,
}

impl ScratchRepository {
    fn create(root: &Path, commit: &str) -> Result<Self> {
        std::fs::create_dir_all(root)
            .map_err(|error| GitError::io("creating the broker scratch root", error))?;
        let path = root.join(format!("push-{}-{}", short(commit), std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .map_err(|error| GitError::io("creating the scratch repository", error))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn destroy(self) {
        let _ = std::fs::remove_dir_all(&self.path);
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
    fn remote_urls_that_could_run_a_command_are_refused() {
        assert!(validate_remote_url("git@github.test:org/repo.git").is_ok());
        assert!(validate_remote_url("https://github.test/org/repo.git").is_ok());
        for bad in [
            "",
            "ext::sh -c 'touch /tmp/pwned'",
            "--upload-pack=evil",
            "https://x/\nrepo",
        ] {
            assert!(validate_remote_url(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_scratch_repository_removes_itself() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = ScratchRepository::create(dir.path(), &"a".repeat(40)).unwrap();
        let path = scratch.path().to_path_buf();
        assert!(path.exists());
        scratch.destroy();
        assert!(!path.exists());
    }

    #[test]
    fn short_commits_do_not_panic_on_odd_input() {
        assert_eq!(short(&"a".repeat(40)), "aaaaaaaaaaaa");
        assert_eq!(short("abc"), "abc");
    }
}
