//! Output rendering.
//!
//! Every command supports `--json` with a stable shape, since the CLI is also
//! the integration-test harness (D13). The human rendering is a *view* of the
//! same JSON, never a separate source of truth, so the two cannot disagree.

use std::io::Write as _;

/// How to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

impl Format {
    pub fn new(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }
}

/// Writes a value.
pub fn emit(
    format: Format,
    value: &serde_json::Value,
    human: impl Fn(&serde_json::Value) -> String,
) {
    let mut stdout = std::io::stdout().lock();
    let rendered = match format {
        Format::Json => serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| "{\"error\":\"unencodable\"}".to_owned()),
        Format::Human => human(value),
    };
    let _ = writeln!(stdout, "{rendered}");
}

/// Renders a value as a key/value block, which is the fallback human view.
pub fn key_values(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}: {}", scalar(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Array(items) => items
            .iter()
            .map(key_values)
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => scalar(other),
    }
}

/// A field of a JSON object, or null when absent.
///
/// `serde_json`'s own indexing returns null for a missing key rather than
/// panicking, but going through this helper keeps the intent explicit at the
/// call sites that matter.
pub fn field<'a>(value: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    value.get(key).unwrap_or(&serde_json::Value::Null)
}

/// Renders one field.
pub fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "-".to_owned(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                "-".to_owned()
            } else {
                items.iter().map(scalar).collect::<Vec<_>>().join(", ")
            }
        }
        serde_json::Value::Object(_) => key_values(value).replace('\n', "; "),
        other => other.to_string(),
    }
}

/// Renders a mission envelope for approval.
///
/// The caveats are printed last and in full: a prompt that omits what it cannot
/// promise invites reflexive approval.
pub fn envelope(value: &serde_json::Value) -> String {
    let mut lines = vec![
        format!("mission   {}", scalar(&value["mission"])),
        format!("state     {}", scalar(&value["state"])),
        format!("objective {}", scalar(&value["objective"])),
        String::new(),
        format!("edit      {}", scalar(&value["edit_paths"])),
        format!("read      {}", scalar(&value["read_paths"])),
        format!("tasks     {}", scalar(&value["allowed_tasks"])),
        format!("egress    {}", scalar(&value["egress_profile"])),
        format!("creds     {}", scalar(&value["credential_policy"])),
        format!("expires   {}", scalar(&value["expires_at"])),
        format!("budget    {} task runs", scalar(&value["max_task_runs"])),
    ];
    let approval_required = &value["approval_required_tasks"];
    if !matches!(approval_required, serde_json::Value::Array(items) if items.is_empty()) {
        lines.push(format!("approval  {}", scalar(approval_required)));
    }
    if let serde_json::Value::Array(baselines) = &value["baselines_in_force"]
        && !baselines.is_empty()
    {
        lines.push(String::new());
        lines.push("access baselines in force:".to_owned());
        lines.extend(baselines.iter().map(|line| format!("  {}", scalar(line))));
    }
    if let serde_json::Value::Array(caveats) = &value["caveats"]
        && !caveats.is_empty()
    {
        lines.push(String::new());
        lines.push("what this does and does not promise:".to_owned());
        lines.extend(caveats.iter().map(|line| format!("  - {}", scalar(line))));
    }
    lines.join("\n")
}

/// Renders a pending approval.
pub fn approval(value: &serde_json::Value) -> String {
    let mut lines = vec![
        format!("approval  {}", scalar(&value["approval"])),
        format!("mission   {}", scalar(&value["mission"])),
        format!("actor     {}", scalar(&value["actor"])),
        format!("subject   {}", scalar(&value["subject"])),
        format!("summary   {}", scalar(&value["summary"])),
        format!("reason    {}", scalar(&value["reason"])),
    ];
    // Why the previous attempt failed comes before the decision, because that is
    // the thing a human most needs to see.
    if let Some(failure) = value["prior_failure"].as_str() {
        lines.push(format!("failed    {failure}"));
    }
    // The egress profile is always shown, even when no hosts resolved: "this
    // would reach the network" is the fact a human needs, and hiding the line
    // when the host list happens to be empty would bury it.
    match &value["egress_hosts"] {
        serde_json::Value::Array(hosts) if !hosts.is_empty() => lines.push(format!(
            "egress    {} → {}",
            scalar(&value["egress_profile"]),
            scalar(&value["egress_hosts"])
        )),
        _ => lines.push(format!("egress    {}", scalar(&value["egress_profile"]))),
    }
    lines.push(format!("creds     {}", scalar(&value["credentials"])));
    if let Some(change) = value["lockfile_change"].as_str() {
        lines.push(format!("lockfile  {change}"));
    }
    if let serde_json::Value::Array(diff) = &value["inventory_diff"]
        && !diff.is_empty()
    {
        lines.push("code execution at build time:".to_owned());
        lines.extend(diff.iter().map(|line| format!("  {}", scalar(line))));
    }
    if let serde_json::Value::Array(evidence) = &value["task_evidence"]
        && !evidence.is_empty()
    {
        lines.push("passing tasks for this tree:".to_owned());
        lines.extend(evidence.iter().map(|line| format!("  {}", scalar(line))));
    }
    if let serde_json::Value::Array(alternatives) = &value["alternatives"]
        && !alternatives.is_empty()
    {
        lines.push("narrower options:".to_owned());
        lines.extend(
            alternatives
                .iter()
                .map(|line| format!("  - {}", scalar(line))),
        );
    }
    if let serde_json::Value::Array(caveats) = &value["caveats"]
        && !caveats.is_empty()
    {
        lines.push("caveats:".to_owned());
        lines.extend(caveats.iter().map(|line| format!("  - {}", scalar(line))));
    }
    lines.push(format!("expires   {}", scalar(&value["expires_at"])));
    lines.join("\n")
}

/// Renders a list of audit entries as a timeline.
pub fn timeline(value: &serde_json::Value) -> String {
    let serde_json::Value::Array(entries) = value else {
        return key_values(value);
    };
    if entries.is_empty() {
        return "no audit events".to_owned();
    }
    entries
        .iter()
        .map(|entry| {
            let marker = if entry["high_signal"].as_bool().unwrap_or(false) {
                "!"
            } else {
                " "
            };
            format!(
                "{marker} {:>5}  {}  {}",
                scalar(&entry["seq"]),
                scalar(&entry["at"]),
                scalar(&entry["detail"])
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a mission review.
pub fn review(value: &serde_json::Value) -> String {
    let mut lines = vec![
        format!("mission   {}", scalar(&value["mission"])),
        format!("state     {}", scalar(&value["state"])),
        format!("objective {}", scalar(&value["objective"])),
        format!("diff      {}", scalar(&value["diff_stat"])),
        format!("budget    {}", scalar(&value["budget_consumed"])),
        format!(
            "audit     {}",
            if value["audit_intact"].as_bool().unwrap_or(false) {
                "chain verifies"
            } else {
                "CHAIN DOES NOT VERIFY"
            }
        ),
    ];
    for (label, key) in [
        ("files", "files_changed"),
        ("tasks", "tasks"),
        ("escalations", "escalations"),
        ("approvals", "approvals"),
        ("egress", "egress"),
        ("brokered", "brokered_operations"),
    ] {
        if let serde_json::Value::Array(items) = &value[key]
            && !items.is_empty()
        {
            lines.push(String::new());
            lines.push(format!("{label}:"));
            lines.extend(items.iter().map(|item| format!("  {}", scalar(item))));
        }
    }
    lines.join("\n")
}

/// Renders the host report.
pub fn doctor(value: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    for (label, key) in [
        ("user namespaces", "user_namespaces"),
        ("cgroup v2", "cgroup_v2"),
        ("cgroup delegation", "cgroup_delegation"),
        ("kvm", "kvm"),
        ("bubblewrap", "bubblewrap"),
        ("firecracker", "firecracker"),
        ("nix", "nix"),
        ("hardlinks", "hardlinks"),
    ] {
        let entry = &value[key];
        let state = entry["state"].as_str().unwrap_or("unknown");
        let marker = match state {
            "available" => "ok  ",
            "blocked" => "BLOCK",
            _ => "MISS",
        };
        lines.push(format!("{marker} {label:<20} {}", scalar(&entry["detail"])));
        if let Some(remedy) = entry["remedy"].as_str() {
            lines.push(format!("      {label:<20} remedy: {remedy}"));
        }
    }
    lines.push(String::new());
    for (label, key) in [
        ("workspace environment", "can_run_workspace"),
        ("build and test tasks", "can_run_build"),
        ("dependency resolution", "can_run_microvm"),
        ("brokered publishing", "broker_reachable"),
    ] {
        let capable = value[key].as_bool().unwrap_or(false);
        lines.push(format!(
            "{} {label}",
            if capable { "ok   " } else { "NO   " }
        ));
    }
    if let Some(assertion) = value["runtime_root_workspace"].as_str() {
        lines.push(format!("      workspace runtime root: {assertion}"));
    }
    if let serde_json::Value::Array(limitations) = &value["known_limitations"] {
        lines.push(String::new());
        lines.push("known limitations:".to_owned());
        lines.extend(
            limitations
                .iter()
                .map(|line| format!("  - {}", scalar(line))),
        );
    }
    lines.join("\n")
}

/// Renders a denial with its reasons and next steps.
pub fn denial(data: &serde_json::Value) -> String {
    let mut lines = vec![format!("denied: {}", scalar(&data["denied"]))];
    if let serde_json::Value::Array(reasons) = &data["reasons"] {
        lines.extend(
            reasons
                .iter()
                .map(|reason| format!("  - {}", scalar(reason))),
        );
    }
    if let serde_json::Value::Array(alternatives) = &data["alternatives"]
        && !alternatives.is_empty()
    {
        lines.push("what you can do instead:".to_owned());
        lines.extend(
            alternatives
                .iter()
                .map(|alternative| format!("  - {}", scalar(alternative))),
        );
    }
    lines.join("\n")
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
    fn an_envelope_states_its_caveats_last_and_in_full() {
        let value = serde_json::json!({
            "mission": "m-1",
            "state": "awaiting_approval",
            "objective": "tidy",
            "edit_paths": ["crates/core"],
            "read_paths": [],
            "allowed_tasks": ["rust.check"],
            "egress_profile": "model-api",
            "credential_policy": "none",
            "expires_at": "2026-01-01T00:00:00Z",
            "max_task_runs": 20,
            "approval_required_tasks": [],
            "baselines_in_force": [],
            "caveats": ["build and test tasks have no network access at all"],
        });
        let rendered = envelope(&value);
        assert!(rendered.contains("what this does and does not promise"));
        assert!(rendered.ends_with("build and test tasks have no network access at all"));
        assert!(rendered.contains("crates/core"));
    }

    #[test]
    fn an_approval_shows_the_prior_failure_before_the_decision() {
        let value = serde_json::json!({
            "approval": "ap-1",
            "mission": "m-1",
            "actor": "agent:claude",
            "subject": "task_escalation",
            "summary": "fetch dependencies",
            "reason": "rust.check failed",
            "prior_failure": "missing_dependencies: serde is not in the bundle",
            "alternatives": [],
            "egress_hosts": ["static.crates.io"],
            "egress_profile": "rust-registry",
            "credentials": "none",
            "outputs": [],
            "lockfile_change": "4 additions",
            "inventory_diff": ["+ serde_derive 1.0"],
            "task_evidence": [],
            "caveats": ["allowlisting is by destination host, not by content"],
            "expires_at": "2026-01-01T00:00:00Z",
            "request_digest": "ab",
        });
        let rendered = approval(&value);
        let failure_at = rendered.find("failed").unwrap();
        let caveats_at = rendered.find("caveats:").unwrap();
        assert!(failure_at < caveats_at);
        assert!(rendered.contains("static.crates.io"));
        assert!(rendered.contains("+ serde_derive 1.0"));
    }

    #[test]
    fn a_timeline_marks_high_signal_events() {
        let value = serde_json::json!([
            {"seq": 1, "at": "t", "kind": "k", "detail": "ordinary", "high_signal": false},
            {"seq": 2, "at": "t", "kind": "k", "detail": "drift", "high_signal": true},
        ]);
        let rendered = timeline(&value);
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(lines[0].starts_with(' '));
        assert!(lines[1].starts_with('!'));
    }

    #[test]
    fn an_empty_timeline_says_so() {
        assert_eq!(timeline(&serde_json::json!([])), "no audit events");
    }

    #[test]
    fn a_review_says_loudly_when_the_audit_chain_does_not_verify() {
        let value = serde_json::json!({
            "mission": "m-1", "state": "completed", "objective": "x",
            "diff_stat": "1 file", "budget_consumed": "1 of 10",
            "audit_intact": false,
            "files_changed": [], "tasks": [], "escalations": [],
            "approvals": [], "egress": [], "brokered_operations": [],
        });
        assert!(review(&value).contains("CHAIN DOES NOT VERIFY"));
    }

    #[test]
    fn the_doctor_report_shows_a_remedy_for_every_failing_probe() {
        let value = serde_json::json!({
            "user_namespaces": {"state": "blocked", "detail": "apparmor", "remedy": "install a profile"},
            "cgroup_v2": {"state": "available", "detail": "ok"},
            "cgroup_delegation": {"state": "unavailable", "detail": "none", "remedy": "use systemd"},
            "kvm": {"state": "unavailable", "detail": "absent", "remedy": "enable virtualisation"},
            "bubblewrap": {"state": "available", "detail": "ok"},
            "firecracker": {"state": "unavailable", "detail": "absent", "remedy": "install it"},
            "nix": {"state": "available", "detail": "ok"},
            "hardlinks": {"state": "available", "detail": "ok"},
            "can_run_workspace": false,
            "can_run_build": false,
            "can_run_microvm": false,
            "broker_reachable": false,
            "known_limitations": ["git metadata is unavailable to build tasks"],
        });
        let rendered = doctor(&value);
        assert!(rendered.contains("BLOCK"));
        assert!(rendered.contains("remedy: install a profile"));
        assert!(rendered.contains("known limitations"));
        assert!(rendered.contains("NO    build and test tasks"));
    }

    #[test]
    fn a_denial_renders_reasons_and_next_steps() {
        let value = serde_json::json!({
            "denied": "rust.check",
            "reasons": ["not in this lease's task scope"],
            "alternatives": ["call request_escalation"],
        });
        let rendered = denial(&value);
        assert!(rendered.starts_with("denied: rust.check"));
        assert!(rendered.contains("what you can do instead"));
    }

    #[test]
    fn scalars_render_arrays_and_nulls_readably() {
        assert_eq!(scalar(&serde_json::Value::Null), "-");
        assert_eq!(scalar(&serde_json::json!([])), "-");
        assert_eq!(scalar(&serde_json::json!(["a", "b"])), "a, b");
        assert_eq!(scalar(&serde_json::json!(7)), "7");
    }
}
