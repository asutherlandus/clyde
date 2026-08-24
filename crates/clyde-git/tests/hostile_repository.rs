//! Hostile-repository hardening, against a real git.
//!
//! The `hostile-git` fixture property: a repository whose `.git/hooks` and
//! `.git/config` are attacker-controlled must execute nothing during a brokered
//! push, and the push must go to the allowlisted remote rather than to whatever
//! the repository's own configuration names.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use clyde_git::push::{PushRequest, push};
use clyde_git::{GitRunner, Identity};

/// Runs git directly, for building the fixture. Deliberately *not* the sanitised
/// runner: the fixture is meant to be hostile, and building it needs ordinary
/// git behaviour.
fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .args(args)
        .output()
        .expect("git must be available; it is in the flake devShell");
    assert!(
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    remote: PathBuf,
    scratch: PathBuf,
    marker_dir: PathBuf,
    commit: String,
    tree: String,
}

/// Builds a workspace repository whose hooks and configuration are hostile.
fn hostile_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let remote = dir.path().join("remote.git");
    let decoy = dir.path().join("decoy.git");
    let scratch = dir.path().join("scratch");
    let marker_dir = dir.path().join("markers");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&marker_dir).unwrap();

    git(
        dir.path(),
        &["init", "--bare", "--quiet", remote.to_str().unwrap()],
    );
    git(
        dir.path(),
        &["init", "--bare", "--quiet", decoy.to_str().unwrap()],
    );
    git(&workspace, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(workspace.join("README.md"), "hello\n").unwrap();
    git(&workspace, &["add", "README.md"]);
    git(&workspace, &["commit", "--quiet", "-m", "initial"]);

    // Every hook that could run during a fetch or a push from this repository.
    for hook in [
        "pre-push",
        "post-commit",
        "pre-receive",
        "update",
        "post-receive",
        "post-update",
        "reference-transaction",
        "pre-auto-gc",
    ] {
        write_executable(
            &workspace.join(".git/hooks").join(hook),
            &format!(
                "#!/bin/sh\ntouch {}/hook-{hook}\nexit 0\n",
                marker_dir.display()
            ),
        );
    }

    // Hostile repository configuration: a transport rewrite pointing at a decoy
    // remote, a custom ssh command, and an object-transfer hook.
    let hostile_config = format!(
        r#"
[url "{decoy}"]
	insteadOf = "{remote}"
[core]
	sshCommand = "sh -c 'touch {markers}/ssh-command'"
	hooksPath = "{workspace}/.git/hooks"
[uploadpack]
	packObjectsHook = "sh -c 'touch {markers}/pack-objects-hook; exec \"$@\"' --"
[filter "evil"]
	clean = "sh -c 'touch {markers}/filter-clean'"
	smudge = "sh -c 'touch {markers}/filter-smudge'"
"#,
        decoy = decoy.display(),
        remote = remote.display(),
        markers = marker_dir.display(),
        workspace = workspace.display(),
    );
    let mut config = std::fs::read_to_string(workspace.join(".git/config")).unwrap();
    config.push_str(&hostile_config);
    std::fs::write(workspace.join(".git/config"), config).unwrap();

    let commit = String::from_utf8(
        Command::new("git")
            .current_dir(&workspace)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let tree = String::from_utf8(
        Command::new("git")
            .current_dir(&workspace)
            .args(["rev-parse", "HEAD^{tree}"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    Fixture {
        _dir: dir,
        workspace,
        remote,
        scratch,
        marker_dir,
        commit,
        tree,
    }
}

fn markers(fixture: &Fixture) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&fixture.marker_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_hostile_repository_executes_nothing_during_a_brokered_push() {
    let fixture = hostile_fixture();
    let runner = GitRunner::discover().expect("git on PATH");

    let result = push(
        &runner,
        &fixture.scratch,
        &PushRequest {
            workspace: fixture.workspace.clone(),
            remote_url: fixture.remote.to_string_lossy().to_string(),
            refspec: "refs/heads/main".to_owned(),
            commit: fixture.commit.clone(),
            expected_tree: fixture.tree.clone(),
        },
    )
    .await
    .expect("the push itself must succeed");

    assert_eq!(result.commit, fixture.commit);
    assert!(
        markers(&fixture).is_empty(),
        "hostile hooks and filters executed: {:?}",
        markers(&fixture)
    );

    // The commit reached the allowlisted remote, not the decoy the workspace
    // configuration's url.insteadOf named.
    let landed = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                fixture.remote.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert_eq!(
        landed, fixture.commit,
        "the push must land on the allowlisted remote"
    );
}

#[tokio::test]
async fn a_tree_that_does_not_match_the_approval_aborts_before_the_push() {
    let fixture = hostile_fixture();
    let runner = GitRunner::discover().expect("git on PATH");

    let error = push(
        &runner,
        &fixture.scratch,
        &PushRequest {
            workspace: fixture.workspace.clone(),
            remote_url: fixture.remote.to_string_lossy().to_string(),
            refspec: "refs/heads/main".to_owned(),
            commit: fixture.commit.clone(),
            expected_tree: "0".repeat(40),
        },
    )
    .await
    .expect_err("a tree mismatch must abort");
    assert!(error.to_string().contains("commit tree"), "{error}");

    let landed = Command::new("git")
        .args([
            "--git-dir",
            fixture.remote.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(
        !landed.status.success(),
        "nothing must have been pushed when the approval did not match"
    );
}

#[tokio::test]
async fn the_scratch_repository_is_destroyed_even_on_failure() {
    let fixture = hostile_fixture();
    let runner = GitRunner::discover().expect("git on PATH");
    let _ = push(
        &runner,
        &fixture.scratch,
        &PushRequest {
            workspace: fixture.workspace.clone(),
            remote_url: fixture.remote.to_string_lossy().to_string(),
            refspec: "refs/heads/main".to_owned(),
            commit: "a".repeat(40),
            expected_tree: fixture.tree.clone(),
        },
    )
    .await;
    let leftovers: Vec<_> = std::fs::read_dir(&fixture.scratch)
        .map(|entries| entries.filter_map(|entry| entry.ok()).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "a failed push must leave no partially populated object store"
    );
}

#[tokio::test]
async fn a_commit_proposal_is_built_without_disturbing_the_index() {
    let fixture = hostile_fixture();
    let runner = GitRunner::discover().expect("git on PATH");
    std::fs::write(fixture.workspace.join("new.rs"), "fn main() {}\n").unwrap();

    let before = std::fs::metadata(fixture.workspace.join(".git/index"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let proposal = clyde_git::prepare_commit_proposal(
        &runner,
        &fixture.workspace,
        &fixture.scratch,
        "add a file",
        &Identity::new("Andrew", "andrew@example.test").unwrap(),
        &[],
    )
    .await
    .expect("a proposal");
    let after = std::fs::metadata(fixture.workspace.join(".git/index"))
        .map(|meta| meta.len())
        .unwrap_or(0);

    assert_eq!(proposal.parent, fixture.commit);
    assert!(proposal.files.contains(&"new.rs".to_owned()));
    assert_eq!(before, after, "the developer's index must be untouched");
    assert!(
        markers(&fixture).is_empty(),
        "preparing a proposal must run no hooks: {:?}",
        markers(&fixture)
    );
}

#[tokio::test]
async fn the_workspace_diff_is_scoped_to_the_requested_paths() {
    let fixture = hostile_fixture();
    let runner = GitRunner::discover().expect("git on PATH");
    std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
    std::fs::write(fixture.workspace.join("src/in_scope.rs"), "// in\n").unwrap();
    std::fs::write(fixture.workspace.join("out_of_scope.rs"), "// out\n").unwrap();

    let scoped = clyde_git::diff::workspace_diff(&runner, &fixture.workspace, &["src".to_owned()])
        .await
        .unwrap();
    assert!(
        scoped
            .untracked_paths
            .contains(&"src/in_scope.rs".to_owned())
    );
    assert!(
        !scoped
            .untracked_paths
            .contains(&"out_of_scope.rs".to_owned()),
        "a scoped diff must not report changes outside the approved scope"
    );

    let unscoped = clyde_git::diff::workspace_diff(&runner, &fixture.workspace, &[])
        .await
        .unwrap();
    assert!(
        unscoped
            .untracked_paths
            .contains(&"out_of_scope.rs".to_owned()),
        "the unscoped diff is how an out-of-scope change becomes visible in review"
    );
}
