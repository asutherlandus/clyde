//! The Phase 4 exit criterion: an agent prepares a commit, requests a push, and
//! the push succeeds only after a human approves it on the admin channel.
//!
//! This runs a real broker against a real git remote, with a workspace whose
//! hooks and configuration are hostile.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use clyde_broker_api::{BrokerRequest, BrokerResponse, GitPushRequest, RefusalReason};
use clyde_brokerd::Broker;
use clyde_core::approval::Decision;
use clyde_core::broker::BrokerOpState;
use clyde_core::task::TaskType;
use clyde_core::workspace::{VcsKind, Workspace};

/// Runs git directly, to build the fixture. Deliberately not the sanitised
/// runner: the fixture is meant to be hostile.
fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .args(args)
        .output()
        .expect("git is in the flake devShell");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    harness: support::Harness,
    workspace: Workspace,
    remote: PathBuf,
    markers: PathBuf,
}

/// A git workspace with hostile hooks and configuration, plus a bare remote.
fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt as _;

    let harness = support::Harness::with_toolchain();
    let root = support::copy_fixture("churn", harness.dir.path());
    let remote = harness.dir.path().join("remote.git");
    let markers = harness.dir.path().join("markers");
    std::fs::create_dir_all(&markers).unwrap();

    git(
        harness.dir.path(),
        &["init", "--bare", "--quiet", remote.to_str().unwrap()],
    );
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "--quiet", "-m", "initial"]);

    for hook in [
        "pre-push",
        "post-commit",
        "pre-receive",
        "reference-transaction",
    ] {
        let path = root.join(".git/hooks").join(hook);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ntouch {}/hook-{hook}\nexit 0\n",
                markers.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut config = std::fs::read_to_string(root.join(".git/config")).unwrap();
    config.push_str(&format!(
        "\n[url \"{markers}/decoy.git\"]\n\tinsteadOf = \"{remote}\"\n[core]\n\tsshCommand = \"sh -c 'touch {markers}/ssh-command'\"\n",
        markers = markers.display(),
        remote = remote.display()
    ));
    std::fs::write(root.join(".git/config"), config).unwrap();

    let workspace = Workspace {
        id: clyde_core::ids::new::workspace_id().unwrap(),
        root,
        vcs: VcsKind::Git {
            default_remote: Some("origin".to_owned()),
            default_branch: Some("main".to_owned()),
        },
        registered_at: chrono::Utc::now(),
        policy_digest: None,
    };
    harness
        .daemon
        .store
        .register_workspace(workspace.clone())
        .unwrap();

    Fixture {
        harness,
        workspace,
        remote,
        markers,
    }
}

/// A broker configured to accept `origin` and `feature/*`.
fn broker(fixture: &Fixture) -> Arc<Broker> {
    let mut config = fixture.harness.config(&fixture.workspace);
    config.push.remotes = ["origin".to_owned()].into_iter().collect();
    config.push.branch_patterns = ["feature/*".to_owned()].into_iter().collect();
    config.broker.remotes = [(
        "origin".to_owned(),
        fixture.remote.to_string_lossy().to_string(),
    )]
    .into_iter()
    .collect();
    Arc::new(Broker {
        config,
        // A local path remote needs no credential, which lets the test exercise
        // everything except the credential itself.
        credential: clyde_git::push::Credential::SshKey {
            path: fixture.harness.dir.path().join("unused.key"),
        },
        git: clyde_git::GitRunner::discover().unwrap(),
        store: Arc::clone(&fixture.harness.daemon.store),
        scratch: fixture.harness.dir.path().join("broker-scratch"),
    })
}

fn markers(fixture: &Fixture) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&fixture.markers) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name != "decoy.git")
        .collect()
}

fn head(repo: &Path) -> String {
    String::from_utf8(
        Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned()
}

/// Drives the daemon side of a push, returning the request the broker receives.
async fn request_push(
    fixture: &Fixture,
    mission: &clyde_core::mission::Mission,
    lease: &clyde_core::lease::Lease,
    branch: &str,
) -> (clyde_core::ids::ApprovalId, GitPushRequest) {
    let commit = head(&fixture.workspace.root);
    let session = clyde_store::ResolvedSession {
        session: clyde_core::session::ActorSession {
            actor: lease.actor.clone(),
            lease: lease.id.clone(),
            token_hash: clyde_core::session::SessionToken::generate()
                .unwrap()
                .hash(),
            issued_at: lease.issued_at,
            expires_at: lease.expires_at,
            revoked_at: None,
            sandbox: None,
        },
        lease: lease.clone(),
        mission: mission.clone(),
    };
    let result = clyded::publish::request(
        &fixture.harness.daemon,
        &session,
        &serde_json::json!({
            "commit": commit,
            "remote": "origin",
            "refspec": format!("refs/heads/{branch}"),
        }),
    )
    .await
    .expect("the request is accepted");
    let text = match &result.content[0] {
        clyde_api::mcp::Content::Text { text } => text.clone(),
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let approval = clyde_core::ids::ApprovalId::parse(value["approval"].as_str().unwrap()).unwrap();

    let operation = fixture
        .harness
        .daemon
        .store
        .list_broker_ops(&mission.id)
        .unwrap()
        .into_iter()
        .find(|operation| operation.approval == approval)
        .unwrap();
    let record = fixture
        .harness
        .daemon
        .store
        .get_approval(&approval)
        .unwrap();
    let tree = fixture
        .harness
        .daemon
        .git
        .tree_of(&fixture.workspace.root, &commit)
        .await
        .unwrap();

    (
        approval.clone(),
        GitPushRequest {
            operation: operation.id,
            mission: mission.id.clone(),
            lease: lease.id.clone(),
            approval,
            request_digest: record.request.request_digest.clone(),
            workspace: fixture.workspace.root.clone(),
            remote: "origin".to_owned(),
            remote_url: fixture.remote.to_string_lossy().to_string(),
            refspec: format!("refs/heads/{branch}"),
            commit,
            expected_tree: tree,
        },
    )
}

/// Configures a mission that may publish.
fn publishing_mission(
    fixture: &Fixture,
) -> (clyde_core::mission::Mission, clyde_core::lease::Lease) {
    let (mission, lease) = fixture.harness.mission(
        &fixture.workspace.id,
        &["crates/core"],
        &[TaskType::RustCheck, TaskType::GitPush],
    );
    // The mission's envelope permits publication, so the lease carries the flag
    // and the credential scope the policy requires.
    let mut lease = lease;
    lease.authority.may_request_publish = true;
    lease.credential_scope = clyde_core::classification::CredentialPolicy::BrokeredGitPush;
    (mission, lease)
}

#[tokio::test]
async fn a_push_happens_only_after_a_human_approves_and_runs_nothing_hostile() {
    let fixture = fixture();
    let (mission, lease) = publishing_mission(&fixture);
    let broker = broker(&fixture);

    // Passing task evidence for this tree, which the prompt shows.
    let context = fixture
        .harness
        .context(&fixture.workspace, &mission, &lease);
    fixture.harness.confirm_baseline(
        &fixture.workspace,
        &mission,
        TaskType::RustCheck,
        "crates/core",
    );
    clyded::tasks::run_task(
        &fixture.harness.daemon,
        &context,
        TaskType::RustCheck,
        clyde_core::repo_path::RepoPath::parse("crates/core").unwrap(),
        clyde_core::task::TaskOptions::RustCheck {
            package: None,
            all_targets: false,
        },
    )
    .await
    .expect("the check passes");

    let (approval, push_request) = request_push(&fixture, &mission, &lease, "feature/x").await;

    // Nothing has been pushed yet: the request created an approval and nothing
    // else. It is not a path to the broker.
    assert!(
        String::from_utf8(
            Command::new("git")
                .args(["--git-dir", fixture.remote.to_str().unwrap(), "branch"])
                .output()
                .unwrap()
                .stdout
        )
        .unwrap()
        .trim()
        .is_empty(),
        "the remote must be untouched before approval"
    );

    // The prompt carries what the human needs.
    let requested = fixture
        .harness
        .audit(&mission.id)
        .into_iter()
        .find(|event| event.kind.name() == "approval.requested")
        .expect("the request is recorded");
    assert_eq!(requested.payload["subject"], "brokered_operation");

    // Before approval, the broker refuses: there is no decision to verify.
    let refused = broker
        .handle(BrokerRequest::GitPush(Box::new(push_request.clone())))
        .await;
    assert!(
        matches!(
            refused,
            BrokerResponse::Refused {
                reason: RefusalReason::ApprovalInvalid { .. }
            }
        ),
        "{refused:?}"
    );

    // A human approves on the admin channel.
    clyded::approvals::decide(
        &fixture.harness.daemon,
        &approval,
        &fixture.harness.operator,
        Decision::ApproveOnce,
        None,
    )
    .unwrap();

    // clyded moves the operation to executing, which is the state the broker
    // verifies for itself.
    fixture
        .harness
        .daemon
        .store
        .transition_broker_op(
            &push_request.operation,
            BrokerOpState::Approved,
            None,
            chrono::Utc::now(),
        )
        .unwrap();
    fixture
        .harness
        .daemon
        .store
        .transition_broker_op(
            &push_request.operation,
            BrokerOpState::Executing,
            None,
            chrono::Utc::now(),
        )
        .unwrap();

    let response = broker
        .handle(BrokerRequest::GitPush(Box::new(push_request.clone())))
        .await;
    match response {
        BrokerResponse::Pushed(outcome) => {
            assert_eq!(outcome.commit, push_request.commit);
        }
        other => panic!("the push must succeed after approval: {other:?}"),
    }

    // It landed on the allowlisted remote, not on the decoy the workspace
    // repository's url.insteadOf names.
    let landed = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                fixture.remote.to_str().unwrap(),
                "rev-parse",
                "refs/heads/feature/x",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert_eq!(landed, push_request.commit);

    // And nothing in the hostile repository ran.
    assert!(
        markers(&fixture).is_empty(),
        "hostile hooks or configuration executed: {:?}",
        markers(&fixture)
    );
}

#[tokio::test]
async fn a_protected_branch_is_refused_before_a_human_is_asked() {
    let fixture = fixture();
    let (mission, lease) = publishing_mission(&fixture);

    let commit = head(&fixture.workspace.root);
    let session = clyde_store::ResolvedSession {
        session: clyde_core::session::ActorSession {
            actor: lease.actor.clone(),
            lease: lease.id.clone(),
            token_hash: clyde_core::session::SessionToken::generate()
                .unwrap()
                .hash(),
            issued_at: lease.issued_at,
            expires_at: lease.expires_at,
            revoked_at: None,
            sandbox: None,
        },
        lease: lease.clone(),
        mission: mission.clone(),
    };
    let error = clyded::publish::request(
        &fixture.harness.daemon,
        &session,
        &serde_json::json!({
            "commit": commit,
            "remote": "origin",
            "refspec": "refs/heads/main",
        }),
    )
    .await
    .expect_err("main is protected");
    assert!(error.to_string().contains("protected-branch"), "{error}");
    // No approval was created: a human is not asked to decide something that
    // would be refused anyway.
    assert!(
        fixture
            .harness
            .daemon
            .store
            .list_pending_approvals(chrono::Utc::now())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_broker_refuses_a_tampered_request() {
    let fixture = fixture();
    let (mission, lease) = publishing_mission(&fixture);
    let broker = broker(&fixture);
    let (approval, push_request) = request_push(&fixture, &mission, &lease, "feature/x").await;

    clyded::approvals::decide(
        &fixture.harness.daemon,
        &approval,
        &fixture.harness.operator,
        Decision::ApproveOnce,
        None,
    )
    .unwrap();
    for state in [BrokerOpState::Approved, BrokerOpState::Executing] {
        fixture
            .harness
            .daemon
            .store
            .transition_broker_op(&push_request.operation, state, None, chrono::Utc::now())
            .unwrap();
    }

    // Each of these differs from what the human approved, and each is refused.
    let tampered = [
        GitPushRequest {
            refspec: "refs/heads/feature/other".to_owned(),
            ..push_request.clone()
        },
        GitPushRequest {
            commit: "a".repeat(40),
            ..push_request.clone()
        },
        GitPushRequest {
            remote: "elsewhere".to_owned(),
            ..push_request.clone()
        },
        GitPushRequest {
            expected_tree: "b".repeat(40),
            ..push_request.clone()
        },
    ];
    for request in tampered {
        let response = broker
            .handle(BrokerRequest::GitPush(Box::new(request.clone())))
            .await;
        assert!(
            matches!(response, BrokerResponse::Refused { .. }),
            "an altered request must be refused: {:?} -> {response:?}",
            request.refspec
        );
    }

    // Nothing reached the remote.
    let branches = String::from_utf8(
        Command::new("git")
            .args(["--git-dir", fixture.remote.to_str().unwrap(), "branch"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(branches.trim().is_empty(), "{branches}");
}

#[tokio::test]
async fn a_replayed_push_is_refused_by_the_operations_state() {
    let fixture = fixture();
    let (mission, lease) = publishing_mission(&fixture);
    let broker = broker(&fixture);
    let (approval, push_request) = request_push(&fixture, &mission, &lease, "feature/x").await;

    clyded::approvals::decide(
        &fixture.harness.daemon,
        &approval,
        &fixture.harness.operator,
        Decision::ApproveOnce,
        None,
    )
    .unwrap();
    for state in [BrokerOpState::Approved, BrokerOpState::Executing] {
        fixture
            .harness
            .daemon
            .store
            .transition_broker_op(&push_request.operation, state, None, chrono::Utc::now())
            .unwrap();
    }
    assert!(matches!(
        broker
            .handle(BrokerRequest::GitPush(Box::new(push_request.clone())))
            .await,
        BrokerResponse::Pushed(_)
    ));

    // clyded records the outcome, which takes the operation terminal.
    fixture
        .harness
        .daemon
        .store
        .transition_broker_op(
            &push_request.operation,
            BrokerOpState::Succeeded,
            None,
            chrono::Utc::now(),
        )
        .unwrap();

    let replay = broker
        .handle(BrokerRequest::GitPush(Box::new(push_request)))
        .await;
    match replay {
        BrokerResponse::Refused {
            reason: RefusalReason::ApprovalInvalid { detail },
        } => assert!(detail.contains("replay"), "{detail}"),
        other => panic!("a replay must be refused: {other:?}"),
    }
}

#[tokio::test]
async fn the_broker_reports_its_capabilities_without_revealing_anything() {
    let fixture = fixture();
    let broker = broker(&fixture);
    let response = broker.handle(BrokerRequest::Capabilities).await;
    match response {
        BrokerResponse::Capabilities(capabilities) => {
            assert_eq!(capabilities.operations, vec!["git_push".to_owned()]);
            assert!(capabilities.holds_credential);
            assert_eq!(capabilities.credential_kind.as_deref(), Some("ssh-key"));
            let encoded = serde_json::to_string(&capabilities).unwrap();
            assert!(!encoded.contains("BEGIN"), "{encoded}");
            assert!(capabilities.protected_branches.contains(&"main".to_owned()));
        }
        other => panic!("unexpected: {other:?}"),
    }
}
