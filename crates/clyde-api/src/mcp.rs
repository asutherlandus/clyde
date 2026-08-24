//! The MCP tool surface (D10).
//!
//! Tool descriptions are part of the security UX: each states the policy
//! consequences of calling it — whether it can trigger network access, whether it
//! requires approval, what it will and will not do. The agent's understanding of
//! its constraints comes from here, and a vague description produces an agent
//! that asks for the wrong things.
//!
//! There is deliberately **no** file reading or editing tool. The agent uses its
//! own filesystem tools against the mounted workspace, and scope is enforced by
//! mount topology (D1) — which is also why an off-the-shelf agent works with no
//! Clyde-specific modification.

use serde::{Deserialize, Serialize};

/// The MCP protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server identity, returned from `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// The `initialize` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Shown to the agent at connection time. States the boundary it is inside,
    /// because an agent that does not know it is sandboxed will waste turns
    /// discovering it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// A tool definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// The `tools/call` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError", default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ToolResult {
    /// A successful result carrying JSON, rendered as text because that is what
    /// every MCP client can display.
    pub fn json(value: &impl Serialize) -> Self {
        let text = serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| "{\"error\":\"result could not be encoded\"}".to_owned());
        Self {
            content: vec![Content::text(text)],
            is_error: false,
        }
    }

    /// A denial. Rendered as an error result rather than a protocol error, so the
    /// agent receives it as a tool outcome it can reason about.
    pub fn denial(view: &crate::views::DenialView) -> Self {
        let mut lines = vec![format!("Denied: {}", view.denied)];
        lines.extend(view.reasons.iter().map(|reason| format!("  - {reason}")));
        if !view.alternatives.is_empty() {
            lines.push("What you can do instead:".to_owned());
            lines.extend(
                view.alternatives
                    .iter()
                    .map(|alternative| format!("  - {alternative}")),
            );
        }
        Self {
            content: vec![Content::text(lines.join("\n"))],
            is_error: true,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![Content::text(message.into())],
            is_error: true,
        }
    }
}

/// A content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text { text: String },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// The instructions an agent receives on connection.
pub fn instructions() -> String {
    [
        "You are running inside a Clyde workspace environment.",
        "",
        "What that means, concretely:",
        "- Your writable paths are exactly the ones this lease grants. A write outside them fails at the kernel, not at a policy check, so there is nothing to work around.",
        "- This environment has no Rust toolchain, no package managers, no browser, and no signing or container tooling. Building or testing project code is only possible through run_task.",
        "- Your only network access is the configured model API, through a proxy that records every connection. You do not hold the credential.",
        "- You cannot approve anything. Approvals happen on a separate socket that is not mounted here.",
        "",
        "Read and edit files with your ordinary tools. Use run_task for anything that needs the project's build toolchain, and request_escalation when a task is refused and you need a human to widen the boundary.",
    ]
    .join("\n")
}

/// Every tool, in a stable order.
///
/// Each description names the policy consequences, because that is where the
/// agent's understanding of its constraints comes from.
pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "mission_status".to_owned(),
            description: "Report the current mission: objective, the paths this lease may edit and read, which task types it may request, the egress profile in force, expiry, and remaining budget. Read-only. No network access, no approval required.".to_owned(),
            input_schema: object_schema(&[], &[]),
        },
        Tool {
            name: "list_capabilities".to_owned(),
            description: "List what this lease may do right now and what would need an escalation. For each task type it reports whether it is available, whether a human must approve each run, what network access it would have, and — when it is unavailable — the reason and the narrower or escalated alternative. Read-only. No network access, no approval required.".to_owned(),
            input_schema: object_schema(&[], &[]),
        },
        Tool {
            name: "run_task".to_owned(),
            description: "Request a typed task. This is the only way to execute project build or test code: your own environment has no toolchain. Compilation and testing run against an immutable snapshot with no network access at all. A task whose policy requires human approval returns a pending approval rather than running. Returns a task run id; poll task_status.".to_owned(),
            input_schema: object_schema(
                &[
                    ("task", "string", "Task type, such as rust.check or rust.test.unit."),
                    ("path", "string", "Workspace-relative build target or path."),
                ],
                &[(
                    "options",
                    "object",
                    "Task-specific options, validated per task type.",
                )],
            ),
        },
        Tool {
            name: "task_status".to_owned(),
            description: "Report a task run's state and outcome, including a structured failure classification that distinguishes a compile error in your code from missing dependencies, blocked egress, access-baseline drift, or a Clyde-side failure. The result names the policy digest that actually applied. Read-only.".to_owned(),
            input_schema: object_schema(
                &[("task_run", "string", "Task run id from run_task.")],
                &[],
            ),
        },
        Tool {
            name: "task_logs".to_owned(),
            description: "Read a task run's logs, optionally from a byte offset so you can page through a long build. Logs are bounded; truncation is reported rather than silent. Read-only.".to_owned(),
            input_schema: object_schema(
                &[("task_run", "string", "Task run id.")],
                &[
                    ("stream", "string", "Either stdout or stderr. Defaults to stderr, where cargo writes its diagnostics."),
                    ("offset", "integer", "Byte offset to read from."),
                ],
            ),
        },
        Tool {
            name: "list_artifacts".to_owned(),
            description: "List artifacts produced by this mission's task runs, each tagged with the trust class of the environment that produced it. Artifacts are read through this API, not through a mounted store. Read-only.".to_owned(),
            input_schema: object_schema(&[], &[]),
        },
        Tool {
            name: "request_escalation".to_owned(),
            description: "Ask a human to widen this lease for a specific task, with a reason and any narrower alternatives you considered. This creates a pending approval on the operator's channel; it does not grant anything and it does not block. You cannot approve your own request. Use this when a task is denied and the denial names an escalation as the next step.".to_owned(),
            input_schema: object_schema(
                &[
                    ("task", "string", "Task type to request."),
                    ("reason", "string", "Why it is needed. The human sees this verbatim."),
                ],
                &[(
                    "alternatives",
                    "array",
                    "Narrower options you considered and why they do not suffice.",
                )],
            ),
        },
        Tool {
            name: "request_subagent".to_owned(),
            description: "Ask for a derived lease and a sub-agent session with a narrower scope than your own. The derived lease can never exceed yours in any dimension, cannot spawn further sub-agents, and cannot request publication. Its budget comes out of your remaining budget.".to_owned(),
            input_schema: object_schema(
                &[
                    ("purpose", "string", "What the sub-agent is for."),
                    ("edit_paths", "array", "Workspace-relative paths it may write. Must be inside yours."),
                ],
                &[
                    ("read_paths", "array", "Additional read-only paths, inside yours."),
                    ("tasks", "array", "Task types to grant, intersected with yours."),
                ],
            ),
        },
        Tool {
            name: "request_publish".to_owned(),
            description: "Ask for a brokered git push of a prepared commit. This creates an approval request; the push itself happens in a separate broker process that holds the credential, and no credential is ever reachable from this environment. A human must approve the exact commit, remote, and branch; an approval does not carry over to a different commit.".to_owned(),
            input_schema: object_schema(
                &[
                    ("commit", "string", "Commit id from commit_prepare."),
                    ("remote", "string", "Remote name. Must be in the configured allowlist."),
                    ("refspec", "string", "Target ref, as refs/heads/<branch>."),
                ],
                &[("reason", "string", "Why this should be published.")],
            ),
        },
        Tool {
            name: "commit_prepare".to_owned(),
            description: "Ask Clyde to build a commit proposal from the working tree, scoped to this lease's edit paths. Commit creation is a trusted Clyde operation: .git is read-only in this environment, so you cannot create commits or plant hooks yourself. Returns a proposal for a human to approve.".to_owned(),
            input_schema: object_schema(
                &[("message", "string", "Commit message.")],
                &[(
                    "paths",
                    "array",
                    "Workspace-relative paths to include. Defaults to this lease's edit paths.",
                )],
            ),
        },
    ]
}

/// Builds a JSON Schema object with the given required and optional properties.
fn object_schema(
    required: &[(&str, &str, &str)],
    optional: &[(&str, &str, &str)],
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for (name, kind, description) in required.iter().chain(optional.iter()) {
        properties.insert(
            (*name).to_owned(),
            serde_json::json!({"type": kind, "description": description}),
        );
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required.iter().map(|(name, _, _)| *name).collect::<Vec<_>>(),
        "additionalProperties": false,
    })
}

/// Whether a tool name is one this server serves.
pub fn is_known_tool(name: &str) -> bool {
    tools().iter().any(|tool| tool.name == name)
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
    fn the_tool_set_matches_the_documented_surface() {
        let names: Vec<String> = tools().into_iter().map(|tool| tool.name).collect();
        let expected = [
            "mission_status",
            "list_capabilities",
            "run_task",
            "task_status",
            "task_logs",
            "list_artifacts",
            "request_escalation",
            "request_subagent",
            "request_publish",
            "commit_prepare",
        ];
        assert_eq!(names.len(), expected.len());
        for name in expected {
            assert!(names.iter().any(|candidate| candidate == name), "{name}");
        }
    }

    #[test]
    fn there_is_no_file_reading_or_editing_tool() {
        // Editing authority is mount topology, not an API call. A tool here
        // would mean an agent's writes went through a request handler, and the
        // "off-the-shelf agent works unmodified" property would be lost.
        for forbidden in [
            "read_code",
            "edit_files",
            "write_file",
            "read_file",
            "shell",
        ] {
            assert!(!is_known_tool(forbidden), "{forbidden} must not exist");
        }
    }

    #[test]
    fn every_description_states_its_policy_consequences() {
        for tool in tools() {
            let description = tool.description.to_lowercase();
            let mentions_policy = description.contains("network")
                || description.contains("approval")
                || description.contains("read-only")
                || description.contains("credential")
                || description.contains("lease");
            assert!(
                mentions_policy,
                "{}'s description says nothing about its policy consequences",
                tool.name
            );
        }
    }

    #[test]
    fn side_effecting_tools_say_what_a_human_must_approve() {
        for name in ["request_publish", "request_escalation"] {
            let tool = tools()
                .into_iter()
                .find(|tool| tool.name == name)
                .expect("tool exists");
            assert!(
                tool.description.contains("human") || tool.description.contains("approv"),
                "{name} must name the approval step"
            );
        }
        let publish = tools()
            .into_iter()
            .find(|tool| tool.name == "request_publish")
            .unwrap();
        assert!(
            publish.description.contains("does not carry over"),
            "the agent should know an approval is bound to one commit"
        );
    }

    #[test]
    fn schemas_reject_unknown_properties() {
        for tool in tools() {
            assert_eq!(
                tool.input_schema["additionalProperties"],
                serde_json::Value::Bool(false),
                "{} accepts unknown properties",
                tool.name
            );
            assert_eq!(tool.input_schema["type"], "object");
        }
    }

    #[test]
    fn run_task_requires_a_task_and_a_path() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name == "run_task")
            .unwrap();
        let required: Vec<&str> = tool.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert!(required.contains(&"task"));
        assert!(required.contains(&"path"));
    }

    #[test]
    fn the_connection_instructions_state_the_boundary() {
        let text = instructions();
        assert!(text.contains("no Rust toolchain"));
        assert!(text.contains("cannot approve anything"));
        assert!(text.contains("do not hold the credential"));
        assert!(
            text.contains("fails at the kernel"),
            "an agent that thinks scope is advisory will try to work around it"
        );
    }

    #[test]
    fn a_denial_result_is_a_tool_error_with_a_next_step() {
        let view = crate::views::DenialView {
            denied: "rust.check".to_owned(),
            reasons: vec!["not in this lease's task scope".to_owned()],
            alternatives: vec!["call request_escalation".to_owned()],
        };
        let result = ToolResult::denial(&view);
        assert!(result.is_error);
        let Content::Text { text } = &result.content[0];
        assert!(text.contains("Denied: rust.check"));
        assert!(text.contains("What you can do instead"));
    }

    #[test]
    fn a_json_result_is_not_an_error_and_omits_the_flag() {
        let result = ToolResult::json(&serde_json::json!({"ok": true}));
        assert!(!result.is_error);
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("isError"));
    }

    #[test]
    fn initialize_advertises_tools_and_the_protocol_version() {
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: "clyded".to_owned(),
                version: "0.1.0".to_owned(),
            },
            instructions: Some(instructions()),
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(encoded["capabilities"]["tools"]["listChanged"], false);
    }
}
