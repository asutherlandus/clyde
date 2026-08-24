//! The operator surface, driven over a real socket.
//!
//! The unit tests cover dispatch; this covers the path a person actually uses —
//! framing, peer checking, and the whole mission lifecycle through JSON-RPC.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use clyde_api::admin::methods;
use clyde_api::codec::{RequestReader, ResponseWriter};
use clyde_api::jsonrpc::{Id, Request, Response};
use tokio::net::{UnixListener, UnixStream};

/// A client that speaks the daemon's own codec.
struct Operator {
    socket: PathBuf,
    next_id: i64,
}

impl Operator {
    async fn call(&mut self, method: &str, params: serde_json::Value) -> Response {
        let stream = UnixStream::connect(&self.socket).await.expect("connect");
        let (read_half, write_half) = stream.into_split();
        let mut reader = RequestReader::new(read_half);
        let mut writer = ResponseWriter::new(write_half);
        self.next_id += 1;
        let request = Request::new(Id::Number(self.next_id), method, params);
        writer.send_value(&request).await.expect("send");
        reader.next_json::<Response>().await.expect("receive")
    }

    async fn ok(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let response = self.call(method, params).await;
        assert!(
            response.error.is_none(),
            "{method} failed: {:?}",
            response.error
        );
        response.result.unwrap_or(serde_json::Value::Null)
    }
}

/// Serves the admin surface on a short socket path.
///
/// Short because a unix socket path has a hard length limit and a temporary
/// directory under a long prefix exceeds it — which is the failure the daemon
/// now diagnoses explicitly.
async fn serve(harness: &support::Harness, name: &str) -> Operator {
    // Named per test: the tests in this binary run concurrently, and a shared
    // path would have one test remove the socket another is still using.
    let socket = PathBuf::from(format!(
        "/tmp/clyde-admin-{}-{name}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind");
    let daemon = Arc::clone(&harness.daemon);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let daemon = Arc::clone(&daemon);
            tokio::spawn(async move { clyded::admin_api::serve(daemon, stream).await });
        }
    });
    Operator { socket, next_id: 0 }
}

#[tokio::test]
async fn the_whole_operator_lifecycle_works_over_the_socket() {
    let harness = support::Harness::with_toolchain();
    let mut operator = serve(&harness, "lifecycle").await;

    // A workspace directory that is not a git repository, which is a case the
    // closeout has to tolerate rather than refuse.
    let project = support::copy_fixture("churn", harness.dir.path());
    let registered = operator
        .ok(
            methods::WORKSPACE_REGISTER,
            serde_json::json!({"root": project.display().to_string()}),
        )
        .await;
    let workspace = registered["workspace"].as_str().unwrap().to_owned();
    assert_eq!(registered["existing"], false);

    // Registering the same root twice is idempotent rather than an error.
    let again = operator
        .ok(
            methods::WORKSPACE_REGISTER,
            serde_json::json!({"root": project.display().to_string()}),
        )
        .await;
    assert_eq!(again["existing"], true);

    let envelope = operator
        .ok(
            methods::MISSION_CREATE,
            serde_json::json!({
                "workspace": workspace,
                "objective": "tidy the core crate",
                "edit_paths": ["crates/core"],
                "tasks": ["rust.check"],
                "model_api": false,
            }),
        )
        .await;
    let mission = envelope["mission"].as_str().unwrap().to_owned();
    assert_eq!(envelope["state"], "awaiting_approval");
    // The envelope states what it cannot promise.
    let caveats = envelope["caveats"].as_array().unwrap();
    assert!(
        caveats
            .iter()
            .any(|caveat| caveat.as_str().unwrap_or("").contains("mount topology")),
        "{caveats:?}"
    );
    assert!(
        caveats
            .iter()
            .any(|caveat| caveat.as_str().unwrap_or("").contains("no access baseline")),
        "an unconfirmed baseline must be visible before approval: {caveats:?}"
    );

    let approved = operator
        .ok(
            methods::MISSION_APPROVE,
            serde_json::json!({"mission": mission}),
        )
        .await;
    assert_eq!(approved["state"], "active");
    // No agent command is configured, so the agent does not start — and that is
    // reported rather than rolling back the approval the human just gave.
    assert!(
        approved["agent"]
            .as_str()
            .unwrap_or("")
            .starts_with("not started"),
        "{approved:?}"
    );

    let proposal = operator
        .ok(
            methods::ACCESS_PROPOSE,
            serde_json::json!({
                "workspace": workspace,
                "task": "rust.check",
                "target": "crates/core",
            }),
        )
        .await;
    assert_eq!(proposal["confirmed"], false);
    assert!(!proposal["grants"].as_array().unwrap().is_empty());

    let confirmed = operator
        .ok(
            methods::ACCESS_CONFIRM,
            serde_json::json!({
                "workspace": workspace,
                "task": "rust.check",
                "target": "crates/core",
            }),
        )
        .await;
    assert_eq!(confirmed["confirmed"], true);
    assert!(
        confirmed["confirmed_by"]
            .as_str()
            .unwrap_or("")
            .starts_with("human:"),
        "the confirmer's identity comes from the peer, not from the request"
    );

    // Closing a mission over a workspace with no commits still closes it, and
    // says why there is no diff.
    let closed = operator
        .ok(
            methods::MISSION_CLOSE,
            serde_json::json!({"mission": mission}),
        )
        .await;
    assert_eq!(closed["state"], "completed");

    let review = operator
        .ok(
            methods::MISSION_REVIEW,
            serde_json::json!({"mission": mission}),
        )
        .await;
    assert_eq!(review["state"], "completed");
    assert_eq!(review["audit_intact"], true);

    let verified = operator
        .ok(methods::AUDIT_VERIFY, serde_json::json!({}))
        .await;
    assert_eq!(verified["intact"], true);

    let timeline = operator
        .ok(
            methods::AUDIT_SHOW,
            serde_json::json!({"mission": mission, "limit": 50}),
        )
        .await;
    let kinds: Vec<&str> = timeline
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["kind"].as_str())
        .collect();
    for expected in [
        "mission.proposed",
        "mission.approved",
        "lease.issued",
        "mission.activated",
        "mission.closed",
    ] {
        assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
    }

    let _ = std::fs::remove_file(&operator.socket);
}

#[tokio::test]
async fn an_unknown_method_and_a_malformed_request_are_refused_cleanly() {
    let harness = support::Harness::new();
    let mut operator = serve(&harness, "errors").await;

    let response = operator.call("nonsense", serde_json::json!({})).await;
    let error = response.error.expect("an error");
    assert!(error.message.contains("not an operator method"));

    // A request with the wrong parameters is a bad request, not an internal
    // failure: the caller can act on the first and cannot on the second.
    let response = operator
        .call(methods::MISSION_STATUS, serde_json::json!({"wrong": 1}))
        .await;
    let error = response.error.expect("an error");
    assert_eq!(error.code, clyde_api::jsonrpc::codes::INVALID_PARAMS);

    let _ = std::fs::remove_file(&operator.socket);
}

#[tokio::test]
async fn one_active_mission_per_workspace_is_enforced_through_the_api() {
    let harness = support::Harness::new();
    let mut operator = serve(&harness, "one-mission").await;
    let project = support::copy_fixture("single-crate", harness.dir.path());
    let registered = operator
        .ok(
            methods::WORKSPACE_REGISTER,
            serde_json::json!({"root": project.display().to_string()}),
        )
        .await;
    let workspace = registered["workspace"].as_str().unwrap().to_owned();

    let create = serde_json::json!({
        "workspace": workspace,
        "objective": "first",
        "edit_paths": ["src"],
        "model_api": false,
    });
    operator.ok(methods::MISSION_CREATE, create.clone()).await;

    let response = operator.call(methods::MISSION_CREATE, create).await;
    let error = response.error.expect("a second mission must be refused");
    assert!(
        error.message.contains("non-terminal state"),
        "{}",
        error.message
    );

    let _ = std::fs::remove_file(&operator.socket);
}
