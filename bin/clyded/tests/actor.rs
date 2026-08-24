//! The actor surface, driven over a real socket as an agent would.
//!
//! This is the Phase 1 exit criterion in test form: an MCP client with no
//! Clyde-specific modification connects, discovers its tools, reads its mission,
//! runs a typed task, and raises an escalation — and cannot do any of the things
//! the boundary exists to prevent.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use clyde_api::codec::{RequestReader, ResponseWriter};
use clyde_api::jsonrpc::{Id, Request, Response};
use clyde_core::lease::Lease;
use clyde_core::task::TaskType;
use tokio::net::{UnixListener, UnixStream};

/// An MCP client speaking the daemon's codec.
struct Agent {
    reader: RequestReader<tokio::net::unix::OwnedReadHalf>,
    writer: ResponseWriter<tokio::net::unix::OwnedWriteHalf>,
    next_id: i64,
}

impl Agent {
    async fn connect(socket: &PathBuf, token: &str) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (read_half, write_half) = stream.into_split();
        let mut agent = Self {
            reader: RequestReader::new(read_half),
            writer: ResponseWriter::new(write_half),
            next_id: 0,
        };
        let response = agent
            .call("initialize", serde_json::json!({"token": token}))
            .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        agent
    }

    async fn call(&mut self, method: &str, params: serde_json::Value) -> Response {
        self.next_id += 1;
        let request = Request::new(Id::Number(self.next_id), method, params);
        self.writer.send_value(&request).await.expect("send");
        self.reader.next_json::<Response>().await.expect("receive")
    }

    /// Calls a tool and returns its parsed JSON result.
    async fn tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let response = self
            .call(
                "tools/call",
                serde_json::json!({"name": name, "arguments": arguments}),
            )
            .await;
        let result = response.result.expect("a tool result");
        assert_eq!(
            result["isError"],
            serde_json::Value::Null,
            "tool {name} returned an error: {result}"
        );
        parse_text(&result)
    }

    /// Calls a tool expected to fail, returning the rendered denial.
    async fn tool_error(&mut self, name: &str, arguments: serde_json::Value) -> String {
        let response = self
            .call(
                "tools/call",
                serde_json::json!({"name": name, "arguments": arguments}),
            )
            .await;
        let result = response.result.expect("a tool result");
        assert_eq!(result["isError"], true, "expected a denial: {result}");
        result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
}

/// Tool results carry JSON as text, which is what every MCP client can display.
fn parse_text(result: &serde_json::Value) -> serde_json::Value {
    let text = result["content"][0]["text"].as_str().unwrap_or("{}");
    serde_json::from_str(text).unwrap_or(serde_json::Value::String(text.to_owned()))
}

async fn serve(harness: &support::Harness, name: &str) -> PathBuf {
    let socket = PathBuf::from(format!(
        "/tmp/clyde-actor-{}-{name}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind");
    let daemon = Arc::clone(&harness.daemon);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let daemon = Arc::clone(&daemon);
            tokio::spawn(async move { clyded::actor_api::serve(daemon, stream).await });
        }
    });
    socket
}

/// Binds a session and returns the token an agent would read from its file.
fn token_for(harness: &support::Harness, lease: &Lease) -> String {
    let token = clyded::missions::bind_session(&harness.daemon, lease).expect("a session");
    let path = clyded::missions::write_token_file(
        &harness.daemon.paths.sandbox_runtime(),
        &lease.id,
        &token,
    )
    .expect("a token file");
    std::fs::read_to_string(path)
        .expect("readable")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn an_mcp_client_discovers_its_tools_and_drives_the_mission() {
    let harness = support::Harness::with_toolchain();
    let socket = serve(&harness, "drive").await;
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, "crates/core");
    let token = token_for(&harness, &lease);

    let mut agent = Agent::connect(&socket, &token).await;

    // Tool discovery, as any MCP client does after initialize.
    let tools = agent.call("tools/list", serde_json::json!({})).await;
    let names: Vec<&str> = tools.result.as_ref().unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"run_task"));
    assert!(names.contains(&"request_escalation"));
    assert!(
        !names.contains(&"edit_files") && !names.contains(&"read_code"),
        "editing is mount topology, not an API: {names:?}"
    );

    // The mission the agent sees is its lease's scope.
    let status = agent.tool("mission_status", serde_json::json!({})).await;
    assert_eq!(status["edit_paths"][0], "crates/core");
    assert_eq!(status["egress_profile"], "none");
    assert!(status["budget"]["task_runs_remaining"].as_u64().unwrap() > 0);

    // What it may do now, and what would need an escalation.
    let capabilities = agent.tool("list_capabilities", serde_json::json!({})).await;
    let check = capabilities
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task"] == "rust.check")
        .expect("rust.check is listed");
    assert_eq!(check["available"], true);
    let push = capabilities
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task"] == "git.push")
        .expect("git.push is listed");
    assert_eq!(push["available"], false);
    assert!(
        push["reason"].as_str().is_some(),
        "an unavailable capability must say why"
    );

    // A typed task, which is the only path to executing project code.
    let run = agent
        .tool(
            "run_task",
            serde_json::json!({"task": "rust.check", "path": "crates/core"}),
        )
        .await;
    let task_run = run["task_run"].as_str().unwrap().to_owned();
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(run["classification"], "success");
    assert!(
        run["policy_digest"]
            .as_str()
            .is_some_and(|digest| !digest.is_empty()),
        "the result names the policy that actually applied"
    );

    let status = agent
        .tool("task_status", serde_json::json!({"task_run": task_run}))
        .await;
    assert_eq!(status["state"], "succeeded");

    let logs = agent
        .tool(
            "task_logs",
            serde_json::json!({"task_run": task_run, "stream": "stderr"}),
        )
        .await;
    assert_eq!(logs["complete"], true);

    let artifacts = agent.tool("list_artifacts", serde_json::json!({})).await;
    assert!(!artifacts.as_array().unwrap().is_empty());
    assert!(
        !serde_json::to_string(&artifacts)
            .unwrap()
            .contains("content_ref"),
        "artifacts are read through the API, not by walking a store"
    );

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn a_denied_task_returns_reasons_and_a_next_step() {
    let harness = support::Harness::with_toolchain();
    let socket = serve(&harness, "denied").await;
    let workspace = harness.register("churn");
    let (_, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let token = token_for(&harness, &lease);
    let mut agent = Agent::connect(&socket, &token).await;

    // No baseline is confirmed, so the task is refused.
    let denial = agent
        .tool_error(
            "run_task",
            serde_json::json!({"task": "rust.check", "path": "crates/core"}),
        )
        .await;
    assert!(denial.contains("Denied: run_task"), "{denial}");
    assert!(
        denial.contains("What you can do instead"),
        "a denial with no path forward makes an agent loop: {denial}"
    );

    // A task outside the envelope names the escalation as the next step.
    let denial = agent
        .tool_error(
            "run_task",
            serde_json::json!({"task": "rust.test.unit", "path": "crates/core"}),
        )
        .await;
    assert!(denial.contains("request_escalation"), "{denial}");

    // A push is not reachable through run_task at all: it goes through
    // request_publish, which creates an approval rather than doing anything.
    let refusal = agent
        .tool_error(
            "run_task",
            serde_json::json!({"task": "git.push", "path": "."}),
        )
        .await;
    assert!(refusal.contains("request_publish"), "{refusal}");

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn an_escalation_reaches_the_operator_with_the_prior_failure_attached() {
    let harness = support::Harness::with_toolchain();
    let socket = serve(&harness, "escalation").await;
    let workspace = harness.register("missing-dep");
    // The whole crate is the target here, so the mission grants the root.
    let (mission, lease) = harness.mission(
        &workspace.id,
        &["."],
        &[TaskType::RustCheck, TaskType::RustResolveDeps],
    );
    harness.confirm_baseline(&workspace, &mission, TaskType::RustCheck, ".");
    let token = token_for(&harness, &lease);
    let mut agent = Agent::connect(&socket, &token).await;

    // The offline build fails because a crate is not in the bundle. That
    // classification is what drives the escalation.
    let run = agent
        .tool(
            "run_task",
            serde_json::json!({"task": "rust.check", "path": "."}),
        )
        .await;
    assert_eq!(
        run["classification"], "missing_dependencies",
        "the entry point to Phase 3: {run}"
    );

    let escalation = agent
        .tool(
            "request_escalation",
            serde_json::json!({
                "task": "rust.resolve-deps",
                "reason": "rust.check cannot proceed offline",
            }),
        )
        .await;
    assert!(escalation["approval"].as_str().is_some());

    // The operator sees it, with the exact host allowlist, the caveats, and what
    // went wrong — a prompt that omits the failure invites reflexive approval.
    let pending = harness
        .daemon
        .store
        .list_pending_approvals(chrono::Utc::now())
        .unwrap();
    assert_eq!(pending.len(), 1);
    let record = harness.daemon.store.get_approval(&pending[0].id).unwrap();
    let events = harness.audit(&mission.id);
    let requested = events
        .iter()
        .find(|event| event.kind.name() == "approval.requested")
        .expect("the request is recorded");
    assert_eq!(requested.payload["egress_profile"], "rust-registry");
    assert!(
        requested.payload["egress_hosts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|host| host == "static.crates.io"),
        "the prompt names the exact allowlist: {}",
        requested.payload
    );
    assert!(
        requested.payload["prior_failure"]
            .as_str()
            .unwrap_or("")
            .contains("missing_dependencies"),
        "the prompt says what went wrong: {}",
        requested.payload
    );

    // And the agent cannot decide it.
    let error = clyded::approvals::decide(
        &harness.daemon,
        &record.request.id,
        &lease.actor,
        clyde_core::approval::Decision::ApproveOnce,
        None,
    )
    .expect_err("an actor must not decide its own escalation");
    assert!(error.to_string().contains("only a human"));

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn a_revoked_lease_stops_the_agent_on_its_next_request() {
    let harness = support::Harness::with_toolchain();
    let socket = serve(&harness, "revoked").await;
    let workspace = harness.register("churn");
    let (mission, lease) = harness.mission(&workspace.id, &["crates/core"], &[TaskType::RustCheck]);
    let token = token_for(&harness, &lease);
    let mut agent = Agent::connect(&socket, &token).await;

    // The connection works before revocation.
    let status = agent.tool("mission_status", serde_json::json!({})).await;
    assert_eq!(status["state"], "active");

    clyded::missions::revoke(&harness.daemon, &mission.id, &harness.operator).unwrap();

    // The *same connection* stops working, because the session is re-resolved on
    // every request rather than cached at initialize.
    let response = agent
        .call(
            "tools/call",
            serde_json::json!({"name": "mission_status", "arguments": {}}),
        )
        .await;
    let error = response.error.expect("revocation is immediate");
    assert_eq!(error.code, clyde_api::jsonrpc::codes::UNAUTHENTICATED);

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn an_unknown_or_absent_token_is_refused_identically() {
    let harness = support::Harness::new();
    let socket = serve(&harness, "auth").await;

    // No token at all.
    let stream = UnixStream::connect(&socket).await.unwrap();
    let (read_half, write_half) = stream.into_split();
    let mut reader = RequestReader::new(read_half);
    let mut writer = ResponseWriter::new(write_half);
    let request = Request::new(Id::Number(1), "initialize", serde_json::json!({}));
    writer.send_value(&request).await.unwrap();
    let absent = reader.next_json::<Response>().await.unwrap();
    let absent = absent.error.expect("refused");

    // A well-formed token that belongs to nothing.
    let stream = UnixStream::connect(&socket).await.unwrap();
    let (read_half, write_half) = stream.into_split();
    let mut reader = RequestReader::new(read_half);
    let mut writer = ResponseWriter::new(write_half);
    let request = Request::new(
        Id::Number(1),
        "initialize",
        serde_json::json!({"token": "ab".repeat(32)}),
    );
    writer.send_value(&request).await.unwrap();
    let unknown = reader.next_json::<Response>().await.unwrap();
    let unknown = unknown.error.expect("refused");

    assert_eq!(absent.code, unknown.code);
    assert_eq!(
        absent.message, unknown.message,
        "the caller must not be able to tell which"
    );
    assert!(absent.data.is_none());

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn an_agent_cannot_see_another_missions_task() {
    let harness = support::Harness::with_toolchain();
    let socket = serve(&harness, "isolation").await;

    // Two workspaces, because a workspace holds one active mission.
    let first_workspace = harness.register("churn");
    let (first_mission, first_lease) = harness.mission(
        &first_workspace.id,
        &["crates/core"],
        &[TaskType::RustCheck],
    );
    harness.confirm_baseline(
        &first_workspace,
        &first_mission,
        TaskType::RustCheck,
        "crates/core",
    );
    let second_workspace = harness.register("single-crate");
    let (_, second_lease) = harness.mission(&second_workspace.id, &["src"], &[TaskType::RustCheck]);

    let mut first = Agent::connect(&socket, &token_for(&harness, &first_lease)).await;
    let run = first
        .tool(
            "run_task",
            serde_json::json!({"task": "rust.check", "path": "crates/core"}),
        )
        .await;
    let task_run = run["task_run"].as_str().unwrap().to_owned();

    let mut second = Agent::connect(&socket, &token_for(&harness, &second_lease)).await;
    let denial = second
        .tool_error("task_status", serde_json::json!({"task_run": task_run}))
        .await;
    assert!(
        denial.contains("no such task run"),
        "an actor must not learn that another mission's task exists: {denial}"
    );

    let _ = std::fs::remove_file(&socket);
}
