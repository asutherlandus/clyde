//! `clyde-git`: the one place in the codebase that constructs a git command.
//!
//! A repository's `.git/config` and `.git/hooks` are attacker-controlled content
//! in this threat model — an agent, or any prior compromise, may have written
//! them. `git push` executes local hooks and honours repository configuration,
//! so a naive implementation runs untrusted code with credentials in scope. That
//! is the exact privilege pivot the design exists to prevent (D8).
//!
//! Every invocation here is sanitised the same way, and the sanitisation is
//! applied by construction rather than by remembering to pass a flag:
//!
//! - system and global configuration are neutralised, so `url.*.insteadOf`,
//!   `core.sshCommand`, and similar rewrites cannot redirect the transport;
//! - hooks are disabled, including the object-transfer hook `upload-pack` would
//!   otherwise run in the *source* repository;
//! - the environment is cleared, so an inherited `GIT_*` variable cannot
//!   reintroduce any of the above;
//! - configuration overrides travel in `GIT_CONFIG_COUNT`/`KEY`/`VALUE`
//!   environment variables rather than `-c` arguments, because those are
//!   inherited by the child processes git spawns — which is where `upload-pack`
//!   lives.
//!
//! There is deliberately no way to build an unsanitised invocation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod commit;
pub mod diff;
pub mod push;

pub use commit::{CommitProposal, prepare_commit_proposal};
pub use diff::{DiffStat, WorkspaceDiff};

/// Errors from running git.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git was not found; add it to the flake devShell")]
    NotFound,

    #[error("{path:?} is not a git repository")]
    NotARepository { path: PathBuf },

    #[error("git {subcommand} failed with status {status}: {stderr}")]
    Failed {
        subcommand: String,
        status: i32,
        stderr: String,
    },

    #[error("git {subcommand} produced output that could not be parsed: {detail}")]
    Unparsable { subcommand: String, detail: String },

    #[error("input/output error running git: {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{value:?} is not a valid {kind}")]
    Invalid { kind: &'static str, value: String },
}

impl GitError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, GitError>;

/// Author and committer identity for a commit.
///
/// Passed explicitly because global configuration is neutralised: a commit's
/// identity is an input Clyde records, not something read from a file the agent
/// could have written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Result<Self> {
        let name: String = name.into();
        let email: String = email.into();
        // Newlines would let a caller inject additional headers into the commit
        // object, and an empty identity produces a commit git will not accept.
        let clean =
            |value: &str| !value.trim().is_empty() && !value.contains(['\n', '\r', '<', '>']);
        if !clean(&name) {
            return Err(GitError::Invalid {
                kind: "author name",
                value: name,
            });
        }
        if !clean(&email) {
            return Err(GitError::Invalid {
                kind: "author email",
                value: email,
            });
        }
        Ok(Self { name, email })
    }
}

/// The configuration overrides applied to every invocation.
///
/// These travel as environment variables so that git's child processes inherit
/// them. `-c` arguments would not reach `upload-pack` in the source repository,
/// which is exactly where the object-transfer hook runs.
fn sanitising_config() -> Vec<(String, String)> {
    let overrides: [(&str, &str); 8] = [
        // No hooks, in this repository or any repository git talks to.
        ("core.hooksPath", "/dev/null"),
        // The object-transfer hook, which runs during fetch in the *source*
        // repository and is the subtle half of hostile-repository hardening.
        ("uploadpack.packObjectsHook", ""),
        ("uploadpack.allowFilter", "false"),
        // Transport rewrites and custom transport commands.
        ("core.sshCommand", "ssh"),
        ("core.fsmonitor", "false"),
        ("core.askpass", ""),
        // Filters run arbitrary commands on checkout and add.
        ("filter.lfs.process", ""),
        ("protocol.file.allow", "always"),
    ];
    let mut variables = vec![("GIT_CONFIG_COUNT".to_owned(), overrides.len().to_string())];
    for (index, (key, value)) in overrides.into_iter().enumerate() {
        variables.push((format!("GIT_CONFIG_KEY_{index}"), key.to_owned()));
        variables.push((format!("GIT_CONFIG_VALUE_{index}"), value.to_owned()));
    }
    variables
}

/// A git invocation. Constructed only through [`GitRunner`], so the
/// sanitisation cannot be bypassed.
#[derive(Debug, Clone)]
pub struct Invocation {
    args: Vec<String>,
    repository: Option<PathBuf>,
    /// `GIT_DIR`, for operations against a bare repository.
    git_dir: Option<PathBuf>,
    /// `GIT_INDEX_FILE`, so building a tree never disturbs the developer's index.
    index_file: Option<PathBuf>,
    identity: Option<Identity>,
    /// Extra environment, used for the author and committer dates.
    extra_env: BTreeMap<String, String>,
}

impl Invocation {
    fn new(args: Vec<String>) -> Self {
        Self {
            args,
            repository: None,
            git_dir: None,
            index_file: None,
            identity: None,
            extra_env: BTreeMap::new(),
        }
    }

    pub fn in_repository(mut self, path: impl Into<PathBuf>) -> Self {
        self.repository = Some(path.into());
        self
    }

    pub fn with_git_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.git_dir = Some(path.into());
        self
    }

    pub fn with_index_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.index_file = Some(path.into());
        self
    }

    pub fn as_identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.insert(key.into(), value.into());
        self
    }

    fn subcommand(&self) -> String {
        self.args.first().cloned().unwrap_or_default()
    }
}

/// Output of a completed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

impl GitOutput {
    /// The trimmed first line of stdout, which is what most plumbing commands
    /// return.
    pub fn first_line(&self) -> &str {
        self.stdout.lines().next().unwrap_or("").trim()
    }
}

/// Runs sanitised git commands.
#[derive(Debug, Clone)]
pub struct GitRunner {
    git: PathBuf,
}

impl GitRunner {
    pub fn new(git: PathBuf) -> Self {
        Self { git }
    }

    /// Finds git on `PATH`.
    pub fn discover() -> Result<Self> {
        let path = std::env::var_os("PATH").ok_or(GitError::NotFound)?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("git"))
            .find(|candidate| candidate.is_file())
            .map(Self::new)
            .ok_or(GitError::NotFound)
    }

    pub fn program(&self) -> &Path {
        &self.git
    }

    /// Builds an invocation. This is the only constructor.
    pub fn command<I, S>(&self, args: I) -> Invocation
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Invocation::new(args.into_iter().map(Into::into).collect())
    }

    /// Runs an invocation, returning its output.
    ///
    /// A non-zero status is an error rather than a value, so a caller cannot
    /// accidentally proceed on a failed git command.
    pub async fn run(&self, invocation: Invocation) -> Result<GitOutput> {
        let completed = self.run_allowing_failure(invocation).await?;
        if completed.output.status != 0 {
            return Err(GitError::Failed {
                subcommand: completed.subcommand,
                status: completed.output.status,
                stderr: completed.output.stderr,
            });
        }
        Ok(completed.output)
    }

    /// Runs an invocation, returning its output even on failure.
    ///
    /// Used where a non-zero status is information rather than an error, such as
    /// `diff --quiet`.
    pub async fn run_allowing_failure(&self, invocation: Invocation) -> Result<Completed> {
        let subcommand = invocation.subcommand();
        let mut command = tokio::process::Command::new(&self.git);

        // Everything git needs is set explicitly; nothing is inherited.
        command.env_clear();
        command.env("PATH", parent_dir(&self.git));
        // A non-existent HOME means global configuration cannot be found even if
        // the neutralising variables were somehow dropped.
        command.env("HOME", "/nonexistent");
        command.env("GIT_CONFIG_NOSYSTEM", "1");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null");
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_ASKPASS", "");
        command.env("GIT_FLUSH", "1");
        command.env("LC_ALL", "C");
        // No credential helper may run: brokered operations supply their own
        // credential inside the broker and nowhere else.
        command.env("GIT_CONFIG_PARAMETERS", "");
        for (key, value) in sanitising_config() {
            command.env(key, value);
        }
        for (key, value) in &invocation.extra_env {
            command.env(key, value);
        }
        if let Some(identity) = &invocation.identity {
            command.env("GIT_AUTHOR_NAME", &identity.name);
            command.env("GIT_AUTHOR_EMAIL", &identity.email);
            command.env("GIT_COMMITTER_NAME", &identity.name);
            command.env("GIT_COMMITTER_EMAIL", &identity.email);
        }
        if let Some(index) = &invocation.index_file {
            command.env("GIT_INDEX_FILE", index);
        }
        if let Some(dir) = &invocation.git_dir {
            command.env("GIT_DIR", dir);
        }
        if let Some(repository) = &invocation.repository {
            command.arg("-C").arg(repository);
        }
        command.arg("--no-optional-locks");
        command.args(&invocation.args);
        command.stdin(std::process::Stdio::null());

        let output = command.output().await.map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => GitError::NotFound,
            _ => GitError::io(format!("running git {subcommand}"), error),
        })?;
        Ok(Completed {
            subcommand,
            output: GitOutput {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                status: output.status.code().unwrap_or(-1),
            },
        })
    }

    /// Whether `path` is a git work tree.
    pub async fn is_repository(&self, path: &Path) -> bool {
        self.run(
            self.command(["rev-parse", "--is-inside-work-tree"])
                .in_repository(path),
        )
        .await
        .is_ok_and(|output| output.first_line() == "true")
    }

    /// Resolves a revision to a full object id.
    pub async fn rev_parse(&self, repository: &Path, revision: &str) -> Result<String> {
        validate_revision(revision)?;
        let output = self
            .run(
                self.command(["rev-parse", "--verify", &format!("{revision}^{{commit}}")])
                    .in_repository(repository),
            )
            .await?;
        let id = output.first_line().to_owned();
        clyde_core::task::validate_git_object_id(&id).map_err(|_| GitError::Unparsable {
            subcommand: "rev-parse".to_owned(),
            detail: "did not return an object id".to_owned(),
        })?;
        Ok(id)
    }

    /// The tree object a commit points at, which is what an approval is bound to
    /// alongside the commit id.
    pub async fn tree_of(&self, repository: &Path, commit: &str) -> Result<String> {
        clyde_core::task::validate_git_object_id(commit).map_err(|_| GitError::Invalid {
            kind: "commit id",
            value: commit.to_owned(),
        })?;
        let output = self
            .run(
                self.command(["rev-parse", &format!("{commit}^{{tree}}")])
                    .in_repository(repository),
            )
            .await?;
        Ok(output.first_line().to_owned())
    }

    /// The current branch, or `None` on a detached head.
    pub async fn current_branch(&self, repository: &Path) -> Result<Option<String>> {
        let completed = self
            .run_allowing_failure(
                self.command(["symbolic-ref", "--quiet", "--short", "HEAD"])
                    .in_repository(repository),
            )
            .await?;
        if completed.output.status != 0 {
            return Ok(None);
        }
        let branch = completed.output.first_line().to_owned();
        Ok((!branch.is_empty()).then_some(branch))
    }
}

/// A completed invocation, including a failing one.
#[derive(Debug, Clone)]
pub struct Completed {
    pub subcommand: String,
    pub output: GitOutput,
}

fn parent_dir(program: &Path) -> PathBuf {
    program
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/usr/bin"))
}

/// Rejects revision strings that could be read as options or shell input.
///
/// Revisions reach git as arguments; a leading dash would be parsed as a flag.
pub fn validate_revision(revision: &str) -> Result<()> {
    let invalid = revision.is_empty()
        || revision.starts_with('-')
        || revision.contains(['\n', '\r', '\0', ' ', ';', '|', '&', '$', '`']);
    if invalid {
        return Err(GitError::Invalid {
            kind: "revision",
            value: revision.to_owned(),
        });
    }
    Ok(())
}

/// Rejects refspecs that are not a plain `refs/heads/<name>` target.
///
/// Push refspecs are matched against a branch allowlist, and that match is only
/// meaningful if the refspec cannot also be a force-push, a deletion, or a
/// multi-ref update.
pub fn validate_branch_refspec(refspec: &str) -> Result<String> {
    let Some(branch) = refspec.strip_prefix("refs/heads/") else {
        return Err(GitError::Invalid {
            kind: "branch refspec",
            value: refspec.to_owned(),
        });
    };
    let invalid = branch.is_empty()
        || branch.starts_with('-')
        || branch.starts_with('.')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
        || branch
            .chars()
            .any(|c| c.is_control() || " ~^:?*[\\\0+".contains(c));
    if invalid {
        return Err(GitError::Invalid {
            kind: "branch refspec",
            value: refspec.to_owned(),
        });
    }
    Ok(branch.to_owned())
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
    fn identities_reject_injection_shapes() {
        assert!(Identity::new("Andrew", "a@example.test").is_ok());
        for (name, email) in [
            ("", "a@example.test"),
            ("Andrew", ""),
            ("Andrew\nX-Header: y", "a@example.test"),
            ("Andrew", "a@example.test>\nnewline"),
            ("An<drew", "a@example.test"),
        ] {
            assert!(
                Identity::new(name, email).is_err(),
                "{name:?}/{email:?} must be rejected"
            );
        }
    }

    #[test]
    fn revisions_that_look_like_options_or_shell_input_are_refused() {
        assert!(validate_revision("HEAD").is_ok());
        assert!(validate_revision("refs/heads/main").is_ok());
        for bad in [
            "",
            "--upload-pack=evil",
            "-x",
            "HEAD; rm -rf /",
            "a\nb",
            "a b",
        ] {
            assert!(validate_revision(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn branch_refspecs_must_be_plain_branch_targets() {
        assert_eq!(
            validate_branch_refspec("refs/heads/feature/x").unwrap(),
            "feature/x"
        );
        for bad in [
            "refs/heads/",
            "+refs/heads/main",
            "refs/heads/main:refs/heads/other",
            ":refs/heads/main",
            "refs/tags/v1",
            "main",
            "refs/heads/../escape",
            "refs/heads/main.lock",
            "refs/heads/with space",
            "refs/heads/star*",
        ] {
            assert!(
                validate_branch_refspec(bad).is_err(),
                "{bad:?} must be rejected: it is not a plain branch target"
            );
        }
    }

    #[test]
    fn the_sanitising_configuration_disables_hooks_and_transport_rewrites() {
        let config = sanitising_config();
        let rendered: BTreeMap<String, String> = config.into_iter().collect();
        let count: usize = rendered
            .get("GIT_CONFIG_COUNT")
            .and_then(|value| value.parse().ok())
            .unwrap();
        let keys: Vec<&String> = (0..count)
            .filter_map(|index| rendered.get(&format!("GIT_CONFIG_KEY_{index}")))
            .collect();
        assert!(keys.iter().any(|key| key.as_str() == "core.hooksPath"));
        assert!(
            keys.iter()
                .any(|key| key.as_str() == "uploadpack.packObjectsHook"),
            "the object-transfer hook runs in the source repository during fetch"
        );
        assert!(keys.iter().any(|key| key.as_str() == "core.sshCommand"));
        assert_eq!(keys.len(), count);
    }

    #[test]
    fn a_git_output_exposes_its_first_line() {
        let output = GitOutput {
            stdout: "abc123\nrest\n".to_owned(),
            stderr: String::new(),
            status: 0,
        };
        assert_eq!(output.first_line(), "abc123");
    }
}
