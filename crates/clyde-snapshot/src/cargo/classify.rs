//! Structured failure classification (Phase 2a deliverable 6).
//!
//! Phase 3's escalation flow keys off this, so the distinction that matters most
//! is "compile failed because the code is wrong" versus "compile failed because
//! dependencies are not present".
//!
//! Compiler diagnostics are read from cargo's JSON output and matched on
//! diagnostic *level and code*, not on rendered text, which changes between
//! toolchain versions. Cargo's own failures — a missing dependency, an offline
//! refusal — have no JSON form and are matched on stderr; those matches are
//! deliberately anchored on the stable half of the message and are covered by
//! fixture tests so a toolchain bump breaks a test rather than a user's build.

use clyde_core::baseline::AccessDrift;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskFailureClass;

/// How the sandboxed process finished.
///
/// A local shape rather than a dependency on `clyde-sandbox`, so classification
/// stays a pure function over values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitSummary {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub signal: Option<i32>,
}

impl ExitSummary {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && self.signal.is_none()
    }
}

/// Everything classification looks at.
#[derive(Debug, Clone, Copy)]
pub struct ClassificationInput<'a> {
    pub exit: ExitSummary,
    /// Cargo's JSON message stream.
    pub stdout: &'a str,
    /// Cargo's own diagnostics.
    pub stderr: &'a str,
    /// Whether the proxy refused a destination during this run. For a
    /// `none`-profile task any egress attempt at all is a finding.
    pub egress_denied: bool,
    /// Paths that exist in the workspace but were excluded from the snapshot,
    /// with the reason. Used to turn an `ENOENT` into a named escalation rather
    /// than a raw cargo error.
    pub excluded: &'a [(RepoPath, String)],
}

/// The classification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub class: TaskFailureClass,
    /// Redaction-safe, bounded summary for the task outcome.
    pub summary: String,
    /// Drift detected from the failure, for the escalation flow.
    pub drift: Vec<AccessDrift>,
    /// Crate names cargo said it could not obtain.
    pub missing_dependencies: Vec<String>,
    /// Compiler diagnostic codes, so a caller can report `E0433` rather than a
    /// wall of text.
    pub diagnostic_codes: Vec<String>,
}

impl Classification {
    fn simple(class: TaskFailureClass, summary: impl Into<String>) -> Self {
        Self {
            class,
            summary: summary.into(),
            drift: Vec::new(),
            missing_dependencies: Vec::new(),
            diagnostic_codes: Vec::new(),
        }
    }
}

/// Classifies a finished task run.
///
/// Order matters: host and policy conditions are recognised before anything is
/// attributed to the user's code, so "Clyde is broken" is never reported as
/// "your code is broken".
pub fn classify(input: ClassificationInput<'_>) -> Classification {
    if input.exit.success() {
        return Classification::simple(TaskFailureClass::Success, "completed successfully");
    }

    if input.exit.timed_out {
        return Classification::simple(
            TaskFailureClass::ResourceExhausted,
            "the task exceeded its wall-clock limit and was terminated",
        );
    }
    if input.exit.signal == Some(9) {
        return Classification::simple(
            TaskFailureClass::ResourceExhausted,
            "the task was killed, which on a cgroup-limited sandbox usually means it hit its memory limit",
        );
    }

    // A `none`-profile task that attempted egress is a finding, not a routine
    // error: it is either a misconfiguration or a hostile dependency probing for
    // a way out.
    if input.egress_denied || mentions_offline_network(input.stderr) {
        return Classification::simple(
            TaskFailureClass::EgressBlocked,
            "the task attempted network access, which its egress profile does not permit",
        );
    }

    // `.git` is never available to a build task, and that failure is neither
    // drift nor a bug in the user's code (D21).
    if let Some(reason) = git_metadata_failure(&input) {
        return Classification::simple(TaskFailureClass::GitMetadataUnavailable, reason);
    }

    let missing = missing_dependencies(input.stderr);
    if !missing.is_empty() {
        return Classification {
            class: TaskFailureClass::MissingDependencies,
            summary: format!(
                "cargo cannot proceed offline: {} not present in the dependency bundle",
                missing.join(", ")
            ),
            drift: Vec::new(),
            missing_dependencies: missing,
            diagnostic_codes: Vec::new(),
        };
    }

    // A read outside the confirmed baseline surfaces as an ENOENT. Under
    // materialisation enforcement this is inference rather than observation, so
    // it only fires when the path is one Clyde knows it excluded.
    let drift = inferred_drift(&input);
    if !drift.is_empty() {
        let rendered = drift
            .iter()
            .map(AccessDrift::render)
            .collect::<Vec<_>>()
            .join("; ");
        return Classification {
            class: TaskFailureClass::AccessBaselineDrift,
            summary: format!(
                "the build tried to read a path outside the confirmed access baseline: {rendered}"
            ),
            drift,
            missing_dependencies: Vec::new(),
            diagnostic_codes: Vec::new(),
        };
    }

    let codes = diagnostic_codes(input.stdout);
    if has_compiler_errors(input.stdout) || has_test_failures(input.stdout) {
        return Classification {
            class: TaskFailureClass::ProjectCodeError,
            summary: if codes.is_empty() {
                "the project's code failed to compile or its tests failed".to_owned()
            } else {
                format!("compilation failed: {}", codes.join(", "))
            },
            drift: Vec::new(),
            missing_dependencies: Vec::new(),
            diagnostic_codes: codes,
        };
    }

    // A non-zero exit with no diagnostic Clyde understands is not attributed to
    // the user's code: an unexplained failure is Clyde's problem to explain.
    Classification::simple(
        TaskFailureClass::Internal,
        format!(
            "the task exited with status {} and produced no diagnostic Clyde could classify",
            input.exit.code.unwrap_or(-1)
        ),
    )
}

/// Whether cargo refused because it would have needed the network.
///
/// Matched on the stable half of cargo's offline message.
fn mentions_offline_network(stderr: &str) -> bool {
    stderr.contains("--offline was specified")
        || stderr.contains("attempting to make an HTTP request")
        || stderr.contains("failed to send request")
}

/// Crate names cargo said it could not obtain.
fn missing_dependencies(stderr: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let line = line.trim();
        // `no matching package named `serde` found`
        if let Some(rest) = line.strip_prefix("no matching package named ")
            && let Some(name) = between_backticks(rest)
        {
            names.push(name);
            continue;
        }
        // `error: no matching package named `serde` found`
        if let Some(index) = line.find("no matching package named ")
            && let Some(rest) = line.get(index + "no matching package named ".len()..)
            && let Some(name) = between_backticks(rest)
        {
            names.push(name);
            continue;
        }
        // `failed to get `serde` as a dependency of package `x``
        if let Some(index) = line.find("failed to get ")
            && let Some(rest) = line.get(index + "failed to get ".len()..)
            && let Some(name) = between_backticks(rest)
        {
            names.push(name);
            continue;
        }
        // `package `serde v1.0.0` is not in the cache`
        if line.contains("is not in the cache")
            && let Some(name) = between_backticks(line)
        {
            names.push(name.split_whitespace().next().unwrap_or(&name).to_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}

fn between_backticks(text: &str) -> Option<String> {
    let start = text.find('`')? + 1;
    let rest = text.get(start..)?;
    let end = rest.find('`')?;
    rest.get(..end).map(str::to_owned)
}

/// Whether the failure was a build reading `.git`.
fn git_metadata_failure(input: &ClassificationInput<'_>) -> Option<String> {
    let excluded_git = input
        .excluded
        .iter()
        .find(|(path, _)| path.components().any(|component| component == ".git"));
    let mentions_git = input.stderr.contains("/.git")
        || input.stderr.contains("not a git repository")
        || input.stdout.contains("not a git repository");
    if excluded_git.is_some() && mentions_git {
        return Some(
            ".git is never available to build tasks, so git metadata could not be read. This is not access drift and not a bug in your code; git metadata support is a known gap (D21)"
                .to_owned(),
        );
    }
    if mentions_git && input.stderr.contains("No such file or directory") {
        return Some(
            "the build tried to read repository history, which is never available to a build task (D21)"
                .to_owned(),
        );
    }
    None
}

/// Infers path drift from an `ENOENT` naming a path Clyde excluded.
fn inferred_drift(input: &ClassificationInput<'_>) -> Vec<AccessDrift> {
    if !input.stderr.contains("No such file or directory")
        && !input.stderr.contains("os error 2")
        && !input.stdout.contains("No such file or directory")
    {
        return Vec::new();
    }
    input
        .excluded
        .iter()
        .filter(|(path, _)| {
            // `.git` has its own diagnostic and is not drift.
            !path.components().any(|component| component == ".git")
        })
        .filter(|(path, _)| {
            input.stderr.contains(path.as_str()) || input.stdout.contains(path.as_str())
        })
        .map(|(path, _)| AccessDrift::PathOutsideBaseline { path: path.clone() })
        .collect()
}

/// Whether cargo's JSON stream carries a compiler error.
fn has_compiler_errors(stdout: &str) -> bool {
    json_messages(stdout).any(|message| {
        message
            .get("message")
            .and_then(|inner| inner.get("level"))
            .and_then(|level| level.as_str())
            == Some("error")
    })
}

/// Diagnostic codes, which is what a user can act on.
fn diagnostic_codes(stdout: &str) -> Vec<String> {
    let mut codes: Vec<String> = json_messages(stdout)
        .filter_map(|message| {
            let inner = message.get("message")?;
            if inner.get("level")?.as_str()? != "error" {
                return None;
            }
            inner.get("code")?.get("code")?.as_str().map(str::to_owned)
        })
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

/// Whether a test binary reported failures.
///
/// libtest's JSON output uses `"type":"suite","event":"failed"`; the
/// human-readable form is matched too, since `cargo test` does not emit JSON for
/// the test run itself unless asked.
fn has_test_failures(stdout: &str) -> bool {
    let json = json_messages(stdout).any(|message| {
        message.get("type").and_then(|value| value.as_str()) == Some("suite")
            && message.get("event").and_then(|value| value.as_str()) == Some("failed")
    });
    json || stdout.contains("test result: FAILED")
}

/// Parses cargo's newline-delimited JSON, skipping lines that are not JSON.
fn json_messages(stdout: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    stdout
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
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

    fn input<'a>(
        code: i32,
        stdout: &'a str,
        stderr: &'a str,
        excluded: &'a [(RepoPath, String)],
    ) -> ClassificationInput<'a> {
        ClassificationInput {
            exit: ExitSummary {
                code: Some(code),
                timed_out: false,
                signal: None,
            },
            stdout,
            stderr,
            egress_denied: false,
            excluded,
        }
    }

    #[test]
    fn a_clean_exit_is_success() {
        let result = classify(input(0, "", "", &[]));
        assert_eq!(result.class, TaskFailureClass::Success);
    }

    #[test]
    fn a_compiler_error_is_the_users_code_and_reports_its_code() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0433"},"rendered":"error[E0433]: failed to resolve"}}
{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused_variables"}}}
"#;
        let result = classify(input(101, stdout, "error: could not compile `app`", &[]));
        assert_eq!(result.class, TaskFailureClass::ProjectCodeError);
        assert_eq!(result.diagnostic_codes, vec!["E0433".to_owned()]);
        assert!(result.summary.contains("E0433"));
    }

    #[test]
    fn warnings_alone_are_not_a_project_error() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"dead_code"}}}"#;
        let result = classify(input(101, stdout, "", &[]));
        assert_eq!(
            result.class,
            TaskFailureClass::Internal,
            "an unexplained failure is Clyde's problem to explain, not the user's code"
        );
    }

    #[test]
    fn a_failing_test_suite_is_the_users_code() {
        let stdout = "running 3 tests\ntest result: FAILED. 2 passed; 1 failed\n";
        assert_eq!(
            classify(input(101, stdout, "", &[])).class,
            TaskFailureClass::ProjectCodeError
        );
        let json = r#"{"type":"suite","event":"failed","passed":2,"failed":1}"#;
        assert_eq!(
            classify(input(101, json, "", &[])).class,
            TaskFailureClass::ProjectCodeError
        );
    }

    #[test]
    fn missing_dependencies_are_recognised_and_named() {
        // The `missing-dep` fixture property, and the entry point to Phase 3.
        for stderr in [
            "error: no matching package named `serde` found\nlocation searched: registry",
            "error: failed to get `serde` as a dependency of package `app v0.1.0`",
            "error: package `serde v1.0.0` is not in the cache",
        ] {
            let result = classify(input(101, "", stderr, &[]));
            assert_eq!(
                result.class,
                TaskFailureClass::MissingDependencies,
                "stderr: {stderr}"
            );
            assert!(
                result
                    .missing_dependencies
                    .iter()
                    .any(|name| name.starts_with("serde")),
                "the crate must be named so the escalation can say what is missing: {:?}",
                result.missing_dependencies
            );
        }
    }

    #[test]
    fn an_offline_network_attempt_is_egress_blocked_not_a_missing_dependency() {
        let stderr = "error: attempting to make an HTTP request, but --offline was specified";
        assert_eq!(
            classify(input(101, "", stderr, &[])).class,
            TaskFailureClass::EgressBlocked
        );
    }

    #[test]
    fn a_recorded_egress_denial_classifies_the_run_even_without_stderr() {
        let mut input = input(101, "", "", &[]);
        input.egress_denied = true;
        let result = classify(input);
        assert_eq!(result.class, TaskFailureClass::EgressBlocked);
    }

    #[test]
    fn reading_git_is_diagnosed_specifically() {
        let excluded = vec![(
            RepoPath::parse(".git").unwrap(),
            ".git is never available".to_owned(),
        )];
        let stderr = "error: failed to run custom build command\n  fatal: not a git repository";
        let result = classify(input(101, "", stderr, &excluded));
        assert_eq!(result.class, TaskFailureClass::GitMetadataUnavailable);
        assert!(
            result.summary.contains("not access drift") && result.summary.contains("known gap"),
            "the diagnostic must stop someone hunting for a bug that does not exist: {}",
            result.summary
        );
    }

    #[test]
    fn an_enoent_on_an_excluded_path_becomes_named_drift() {
        let excluded = vec![(
            RepoPath::parse("crates/proto/schema.sql").unwrap(),
            "outside the baseline".to_owned(),
        )];
        let stderr =
            "error: couldn't read crates/proto/schema.sql: No such file or directory (os error 2)";
        let result = classify(input(101, "", stderr, &excluded));
        assert_eq!(result.class, TaskFailureClass::AccessBaselineDrift);
        assert_eq!(result.drift.len(), 1);
        assert!(
            result.summary.contains("crates/proto/schema.sql"),
            "the escalation names the path rather than surfacing a raw cargo error"
        );
    }

    #[test]
    fn an_enoent_on_an_unrelated_path_is_not_drift() {
        let excluded = vec![(
            RepoPath::parse("docs/other.sql").unwrap(),
            "outside".to_owned(),
        )];
        let stderr = "error: couldn't read src/generated.rs: No such file or directory";
        let result = classify(input(101, "", stderr, &excluded));
        assert_ne!(result.class, TaskFailureClass::AccessBaselineDrift);
    }

    #[test]
    fn resource_exhaustion_is_never_reported_as_the_users_code() {
        let timed_out = ClassificationInput {
            exit: ExitSummary {
                code: None,
                timed_out: true,
                signal: None,
            },
            stdout: "",
            stderr: "",
            egress_denied: false,
            excluded: &[],
        };
        assert_eq!(
            classify(timed_out).class,
            TaskFailureClass::ResourceExhausted
        );
        let killed = ClassificationInput {
            exit: ExitSummary {
                code: None,
                timed_out: false,
                signal: Some(9),
            },
            ..timed_out
        };
        let result = classify(killed);
        assert_eq!(result.class, TaskFailureClass::ResourceExhausted);
        assert!(result.summary.contains("memory limit"));
    }

    #[test]
    fn every_non_success_class_is_reachable_and_none_blames_the_user_wrongly() {
        let cases = [
            TaskFailureClass::ProjectCodeError,
            TaskFailureClass::MissingDependencies,
            TaskFailureClass::EgressBlocked,
            TaskFailureClass::GitMetadataUnavailable,
            TaskFailureClass::AccessBaselineDrift,
            TaskFailureClass::ResourceExhausted,
            TaskFailureClass::Internal,
        ];
        for class in cases {
            assert_eq!(
                class.is_users_code(),
                class == TaskFailureClass::ProjectCodeError,
                "{class} is misattributed"
            );
        }
    }

    #[test]
    fn non_json_output_lines_are_skipped_rather_than_failing_classification() {
        let stdout = "warning: some human text\n{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\"}}\nnot json\n";
        assert_eq!(
            classify(input(101, stdout, "", &[])).class,
            TaskFailureClass::ProjectCodeError
        );
    }
}
