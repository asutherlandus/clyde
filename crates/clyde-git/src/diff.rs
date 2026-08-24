//! Diff-based edit auditing.
//!
//! With edit scope enforced by mount topology (D1), Clyde records what changed
//! rather than each write. The diff is computed by the daemon against the live
//! tree using the same sanitised invocations the broker uses, because a diff run
//! against a hostile repository would otherwise execute its filters and hooks.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{GitError, GitRunner, Result};

/// Per-file change counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl DiffStat {
    pub fn is_empty(&self) -> bool {
        self.files_changed == 0
    }

    pub fn render(&self) -> String {
        format!(
            "{} file(s) changed, {} insertion(s), {} deletion(s)",
            self.files_changed, self.insertions, self.deletions
        )
    }
}

/// A workspace diff, scoped to a set of paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDiff {
    /// Unified diff text. Bounded by the caller before it becomes an artifact.
    pub patch: String,
    pub stat: DiffStat,
    /// Paths that changed, workspace-relative.
    pub changed_paths: Vec<String>,
    /// Untracked files inside the scope, which a plain `git diff` would miss.
    pub untracked_paths: Vec<String>,
}

impl WorkspaceDiff {
    pub fn is_empty(&self) -> bool {
        self.stat.is_empty() && self.untracked_paths.is_empty()
    }
}

/// Computes the diff of the working tree against `HEAD`, restricted to `paths`.
///
/// Restricting by path is what makes the diff *scoped*: a mission's review shows
/// what changed inside the approved scope, and a change outside it — which the
/// mount topology should have prevented — shows up as absent here and present in
/// the unscoped diff, which is a signal worth having.
pub async fn workspace_diff(
    runner: &GitRunner,
    repository: &Path,
    paths: &[String],
) -> Result<WorkspaceDiff> {
    let mut args = vec![
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "HEAD".to_owned(),
    ];
    if !paths.is_empty() {
        args.push("--".to_owned());
        args.extend(paths.iter().cloned());
    }
    let patch = runner
        .run(runner.command(args.clone()).in_repository(repository))
        .await?
        .stdout;

    let mut stat_args = args.clone();
    // `--numstat` after the subcommand, before the pathspec separator.
    stat_args.insert(1, "--numstat".to_owned());
    let numstat = runner
        .run(runner.command(stat_args).in_repository(repository))
        .await?
        .stdout;
    let (stat, changed_paths) = parse_numstat(&numstat);

    let untracked_paths = untracked(runner, repository, paths).await?;

    Ok(WorkspaceDiff {
        patch,
        stat,
        changed_paths,
        untracked_paths,
    })
}

/// Untracked files inside the scope.
async fn untracked(runner: &GitRunner, repository: &Path, paths: &[String]) -> Result<Vec<String>> {
    let mut args = vec![
        "ls-files".to_owned(),
        "--others".to_owned(),
        "--exclude-standard".to_owned(),
    ];
    if !paths.is_empty() {
        args.push("--".to_owned());
        args.extend(paths.iter().cloned());
    }
    let output = runner
        .run(runner.command(args).in_repository(repository))
        .await?;
    Ok(output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Parses `git diff --numstat` output.
///
/// Binary files report `-` rather than a count, which is counted as a changed
/// file with no line counts rather than being dropped.
pub fn parse_numstat(text: &str) -> (DiffStat, Vec<String>) {
    let mut stat = DiffStat::default();
    let mut paths = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        stat.files_changed = stat.files_changed.saturating_add(1);
        stat.insertions = stat
            .insertions
            .saturating_add(added.parse::<u32>().unwrap_or(0));
        stat.deletions = stat
            .deletions
            .saturating_add(removed.parse::<u32>().unwrap_or(0));
        paths.push(path.trim().to_owned());
    }
    (stat, paths)
}

/// The set of paths with uncommitted changes, tracked or not.
pub async fn dirty_paths(runner: &GitRunner, repository: &Path) -> Result<Vec<String>> {
    let output = runner
        .run(
            runner
                .command(["status", "--porcelain=v1", "--untracked-files=all"])
                .in_repository(repository),
        )
        .await?;
    output
        .stdout
        .lines()
        .filter(|line| line.len() > 3)
        .map(|line| {
            line.get(3..)
                .map(|path| path.trim().to_owned())
                .ok_or_else(|| GitError::Unparsable {
                    subcommand: "status".to_owned(),
                    detail: "short status line".to_owned(),
                })
        })
        .collect()
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
    fn numstat_parsing_counts_files_and_lines() {
        let (stat, paths) = parse_numstat("3\t1\tsrc/lib.rs\n0\t7\tsrc/old.rs\n");
        assert_eq!(stat.files_changed, 2);
        assert_eq!(stat.insertions, 3);
        assert_eq!(stat.deletions, 8);
        assert_eq!(paths, vec!["src/lib.rs", "src/old.rs"]);
        assert!(!stat.is_empty());
        assert!(stat.render().contains("2 file(s)"));
    }

    #[test]
    fn binary_files_are_counted_without_line_numbers() {
        let (stat, paths) = parse_numstat("-\t-\tassets/logo.png\n");
        assert_eq!(stat.files_changed, 1);
        assert_eq!(stat.insertions, 0);
        assert_eq!(paths, vec!["assets/logo.png"]);
    }

    #[test]
    fn empty_output_is_an_empty_diff() {
        let (stat, paths) = parse_numstat("");
        assert!(stat.is_empty());
        assert!(paths.is_empty());
        assert!(WorkspaceDiff::default().is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped_rather_than_failing_the_diff() {
        let (stat, _) = parse_numstat("garbage\n1\t1\tok.rs\n");
        assert_eq!(stat.files_changed, 1);
    }
}
