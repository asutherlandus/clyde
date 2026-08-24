//! `--json` output shape, driven through the real binaries.
//!
//! The CLI is also the integration-test harness, so the `--json` shape has to be
//! stable from Phase 1 (D13). This drives the actual `clyde` binary against an
//! actual `clyded` and asserts on the fields, so a rename that would break a
//! caller breaks a test here first.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The directory holding the built binaries.
///
/// The test executable lives in `target/<profile>/deps`, and the binaries are
/// one level up. `CARGO_BIN_EXE_*` only covers this package's own binaries, and
/// the daemon is a different package.
fn binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("the test executable has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(name);
    assert!(
        candidate.is_file(),
        "{candidate:?} is missing; build the workspace before running this test"
    );
    candidate
}

/// A running daemon with its own state directory.
struct Daemon {
    process: Child,
    state: PathBuf,
    _dir: tempfile::TempDir,
    project: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

impl Daemon {
    fn start(name: &str) -> Self {
        // A short path, because a unix socket path has a hard length limit that
        // a temporary directory under a long prefix exceeds.
        let state = PathBuf::from(format!("/tmp/clyde-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();

        // The project is a copy, so the test never edits the repository's own
        // fixture.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        copy_tree(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/churn"),
            &project,
        );

        let process = Command::new(binary("clyded"))
            .arg("--state-dir")
            .arg(&state)
            .arg("--log")
            .arg("error")
            .spawn()
            .expect("clyded starts");

        let daemon = Self {
            process,
            state,
            _dir: dir,
            project,
        };
        daemon.wait_for_socket();
        daemon
    }

    fn wait_for_socket(&self) {
        let socket = self.state.join("run/clyded-admin.sock");
        for _ in 0..100 {
            if socket.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("clyded did not bind its admin socket");
    }

    /// Runs `clyde --json ...` and parses the result.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = Command::new(binary("clyde"))
            .arg("--state-dir")
            .arg(&self.state)
            .arg("--json")
            .args(args)
            .output()
            .expect("clyde runs");
        assert!(
            output.status.success(),
            "clyde {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "clyde {args:?} did not emit JSON ({error}): {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    /// Runs a command expected to fail, returning stderr.
    fn failure(&self, args: &[&str]) -> String {
        let output = Command::new(binary("clyde"))
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .output()
            .expect("clyde runs");
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        String::from_utf8_lossy(&output.stderr).to_string()
    }
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap().filter_map(Result::ok) {
        let child = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &child);
        } else {
            std::fs::copy(entry.path(), &child).unwrap();
        }
    }
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} is missing or not a string in {value}"))
        .to_owned()
}

#[test]
fn every_command_emits_a_stable_json_shape() {
    let daemon = Daemon::start("shape");

    // doctor
    let doctor = daemon.json(&["doctor"]);
    for key in [
        "user_namespaces",
        "cgroup_v2",
        "cgroup_delegation",
        "kvm",
        "bubblewrap",
        "firecracker",
        "nix",
        "hardlinks",
    ] {
        assert!(doctor[key]["state"].is_string(), "doctor.{key}");
    }
    assert!(doctor["can_run_build"].is_boolean());
    assert!(doctor["known_limitations"].is_array());

    // workspace register / list
    let registered = daemon.json(&["workspace", "register", daemon.project.to_str().unwrap()]);
    let workspace = text(&registered, "workspace");
    assert_eq!(registered["existing"], false);

    let workspaces = daemon.json(&["workspace", "list"]);
    let first = &workspaces.as_array().expect("an array")[0];
    assert_eq!(text(first, "workspace"), workspace);
    assert!(first["root"].is_string());
    assert!(first["active_mission"].is_null());

    // mission create → the envelope
    let envelope = daemon.json(&[
        "mission",
        "create",
        "--workspace",
        &workspace,
        "--edit",
        "crates/core",
        "--task",
        "rust.check",
        "tidy the core crate",
    ]);
    let mission = text(&envelope, "mission");
    assert_eq!(envelope["state"], "awaiting_approval");
    for key in [
        "objective",
        "edit_paths",
        "read_paths",
        "allowed_tasks",
        "egress_profile",
        "credential_policy",
        "expires_at",
        "max_task_runs",
        "approval_required_tasks",
        "baselines_in_force",
        "caveats",
    ] {
        assert!(!envelope[key].is_null(), "envelope.{key} is missing");
    }

    // mission list / status
    let missions = daemon.json(&["mission", "list"]);
    assert_eq!(missions.as_array().unwrap().len(), 1);
    let status = daemon.json(&["mission", "status", &mission]);
    assert_eq!(text(&status, "mission"), mission);

    // mission approve
    let approved = daemon.json(&["mission", "approve", &mission]);
    assert_eq!(approved["state"], "active");
    assert!(approved["lease"].is_string());
    assert!(approved["agent"].is_string());

    // access propose / show / confirm
    let proposal = daemon.json(&[
        "access",
        "propose",
        "--workspace",
        &workspace,
        "--task",
        "rust.check",
        "crates/core",
    ]);
    assert_eq!(proposal["confirmed"], false);
    assert!(proposal["grants"].is_array());
    assert!(proposal["pins"].is_array());

    let shown = daemon.json(&[
        "access",
        "show",
        "--workspace",
        &workspace,
        "--task",
        "rust.check",
        "crates/core",
    ]);
    assert_eq!(text(&shown, "target"), "crates/core");

    let confirmed = daemon.json(&[
        "access",
        "confirm",
        "--workspace",
        &workspace,
        "--task",
        "rust.check",
        "crates/core",
    ]);
    assert_eq!(confirmed["confirmed"], true);
    assert!(confirmed["confirmed_by"].is_string());

    // approvals list — empty, and an empty array rather than null.
    let approvals = daemon.json(&["approvals", "list"]);
    assert_eq!(approvals, serde_json::json!([]));

    // deps list
    assert_eq!(daemon.json(&["deps", "list"]), serde_json::json!([]));

    // task list
    assert_eq!(
        daemon.json(&["task", "list", "--mission", &mission]),
        serde_json::json!([])
    );

    // audit show / verify
    let timeline = daemon.json(&["audit", "show", "--limit", "50"]);
    let entries = timeline.as_array().expect("an array");
    assert!(!entries.is_empty());
    for entry in entries {
        for key in ["seq", "at", "kind", "detail", "high_signal"] {
            assert!(!entry[key].is_null(), "audit entry missing {key}: {entry}");
        }
    }
    let verified = daemon.json(&["audit", "verify"]);
    assert_eq!(verified["intact"], true);

    // mission review
    let review = daemon.json(&["mission", "review", &mission]);
    for key in [
        "objective",
        "state",
        "files_changed",
        "diff_stat",
        "tasks",
        "escalations",
        "approvals",
        "egress",
        "brokered_operations",
        "budget_consumed",
        "audit_intact",
    ] {
        assert!(!review[key].is_null(), "review.{key} is missing");
    }

    // mission close
    let closed = daemon.json(&["mission", "close", &mission]);
    assert_eq!(closed["state"], "completed");
}

#[test]
fn operator_commands_refuse_inside_a_sandbox_and_say_why() {
    let daemon = Daemon::start("refuse");
    let output = Command::new(binary("clyde"))
        .arg("--state-dir")
        .arg(&daemon.state)
        .args(["approvals", "approve", "ap-01ARZ3NDEKTSV4RRFFQ69G5FAV"])
        // The marker a workspace environment has.
        .env("CLYDE_SESSION_TOKEN_FILE", "/run/clyde/session-token")
        .output()
        .expect("clyde runs");
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("cannot run inside a Clyde workspace environment"),
        "{message}"
    );
    assert!(
        message.contains("unable to approve its own request"),
        "the refusal must say why it exists: {message}"
    );
}

#[test]
fn an_actor_command_on_the_host_points_at_the_operator_commands() {
    let daemon = Daemon::start("actor");
    let message = daemon.failure(&["task", "run", "rust.check", "crates/core"]);
    assert!(
        message.contains("operator commands"),
        "a host user needs to be told what to use instead: {message}"
    );
}

#[test]
fn a_denied_request_renders_its_reasons_and_next_steps() {
    let daemon = Daemon::start("denial");
    let registered = daemon.json(&["workspace", "register", daemon.project.to_str().unwrap()]);
    let workspace = text(&registered, "workspace");
    // A mission with no scope at all is not a bounded delegation.
    let message = daemon.failure(&[
        "mission",
        "create",
        "--workspace",
        &workspace,
        "an unscoped mission",
    ]);
    assert!(message.contains("bounded delegation"), "{message}");
}

#[test]
fn doctor_works_without_a_running_daemon() {
    // Bring-up is exactly when the daemon is not running.
    let state = PathBuf::from(format!("/tmp/clyde-cli-{}-nodaemon", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let output = Command::new(binary("clyde"))
        .arg("--state-dir")
        .arg(&state)
        .args(["--json", "doctor"])
        .output()
        .expect("clyde runs");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["user_namespaces"]["state"].is_string());
    assert_eq!(
        report["daemon"], "not running; this report is a local probe",
        "the report must say it is a local probe"
    );
    let _ = std::fs::remove_dir_all(&state);
}
