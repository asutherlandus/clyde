//! Audit helpers.
//!
//! Every payload is constructed here from already-redacted values. There is no
//! path that serialises a credential-bearing type into a payload, which is
//! enforced by those types not implementing `Serialize` at all.

use clyde_core::audit::{AuditEvent, AuditEventDraft, AuditEventKind};
use clyde_store::Store;

/// Appends an event, logging rather than failing the operation if the append
/// fails.
///
/// An audit write that fails is serious, but a mission operation that half
/// succeeded because its audit record could not be written is worse. The failure
/// is surfaced through the daemon log and through `audit.verify`, which is where
/// an operator would look.
pub fn record(store: &dyn Store, draft: AuditEventDraft) -> Option<AuditEvent> {
    match store.append_audit(draft) {
        Ok(event) => Some(event),
        Err(error) => {
            tracing::error!(error = %error, "audit append failed");
            None
        }
    }
}

/// Builds a draft with a payload, for the common case.
pub fn draft(kind: AuditEventKind, payload: serde_json::Value) -> AuditEventDraft {
    AuditEventDraft::new(kind).payload(payload)
}

/// Renders an event for the operator's timeline.
///
/// The payload is summarised rather than dumped: a timeline is for reading.
pub fn describe(event: &AuditEvent) -> String {
    let subject = event
        .mission
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| event.workspace.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| "-".to_owned());
    let detail = match &event.payload {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}={}", render_value(value)))
            .collect::<Vec<_>>()
            .join(" "),
        other => render_value(other),
    };
    format!("{} {subject} {detail}", event.kind.name())
        .trim_end()
        .to_owned()
}

fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => {
            items.iter().map(render_value).collect::<Vec<_>>().join(",")
        }
        other => other.to_string(),
    }
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
    use clyde_core::ids;
    use clyde_store::MemoryStore;

    #[test]
    fn events_are_appended_and_described() {
        let store = MemoryStore::new();
        let mission = ids::new::mission_id().unwrap();
        let event = record(
            &store,
            draft(
                AuditEventKind::MissionApproved,
                serde_json::json!({"by": "human:andrew"}),
            )
            .mission(mission.clone()),
        )
        .expect("appended");
        assert_eq!(event.seq, 1);
        let rendered = describe(&event);
        assert!(rendered.starts_with("mission.approved"));
        assert!(rendered.contains(mission.as_str()));
        assert!(rendered.contains("by=human:andrew"));
    }

    #[test]
    fn an_event_with_no_payload_renders_cleanly() {
        let store = MemoryStore::new();
        let event =
            record(&store, AuditEventDraft::new(AuditEventKind::DaemonStarted)).expect("appended");
        assert_eq!(describe(&event), "daemon.started -");
    }
}
