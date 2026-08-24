//! Sub-agent derivation (`request_subagent`).
//!
//! A derived lease can never exceed its parent in any dimension, cannot spawn
//! further sub-agents, and cannot request publication. The narrowing is real
//! rather than advisory: a narrower `edit_paths` yields a narrower writable
//! mount set, and the sub-agent cannot write outside it whatever it attempts.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::Utc;
use clyde_api::mcp::ToolResult;
use clyde_core::actor::{Actor, ActorKind};
use clyde_core::audit::AuditEventKind;
use clyde_core::budget::BudgetCost;
use clyde_core::ids;
use clyde_core::mission::MissionScope;
use clyde_core::repo_path::RepoPath;
use clyde_core::task::TaskType;
use clyde_policy::derive::{SubagentAsk, derive_lease, narrowed_request};
use clyde_store::ResolvedSession;

use crate::daemon::Daemon;
use crate::error::{DaemonError, Result};
use crate::{actor_api, audit, missions};

/// Handles a `request_subagent` call.
pub async fn request(
    daemon: &Arc<Daemon>,
    session: &ResolvedSession,
    arguments: &serde_json::Value,
) -> Result<ToolResult> {
    #[derive(serde::Deserialize)]
    struct Args {
        purpose: String,
        edit_paths: Vec<String>,
        #[serde(default)]
        read_paths: Vec<String>,
        #[serde(default)]
        tasks: Vec<String>,
    }
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| DaemonError::invalid(error.to_string()))?;

    if !session.lease.authority.may_spawn_subagents {
        return Err(DaemonError::Denied(vec![
            clyde_core::decision::PolicyReason::AuthorityFlagNotHeld {
                flag: "may_spawn_subagents".to_owned(),
            },
        ]));
    }

    let parse_all = |paths: &[String]| -> Result<BTreeSet<RepoPath>> {
        paths
            .iter()
            .map(|path| RepoPath::parse(path).map_err(DaemonError::from))
            .collect()
    };
    let scope = MissionScope {
        edit_paths: parse_all(&args.edit_paths)?,
        read_paths: parse_all(&args.read_paths)?,
    };
    let tasks: BTreeSet<TaskType> = if args.tasks.is_empty() {
        session.lease.task_scope.clone()
    } else {
        args.tasks
            .iter()
            .map(|name| TaskType::parse(name).map_err(DaemonError::from))
            .collect::<Result<_>>()?
    };

    // The child's index is the number of sub-agents already spawned, so
    // identifiers are stable and reviewable.
    let index = u32::from(session.lease.usage.subagents).saturating_add(1);
    let actor = session.lease.actor.subagent(index)?;

    let ask = SubagentAsk {
        id: ids::new::lease_id()?,
        actor: actor.clone(),
        issued_by: clyde_core::ids::ActorId::parse("human:clyde")?,
        now: Utc::now(),
        scope,
        tasks,
        budget: session.lease.budget,
        purpose: args.purpose,
    };
    let derivation = narrowed_request(&session.lease, ask);
    let derived = derive_lease(&session.lease, derivation).map_err(|reason| {
        // A derivation failure is a policy denial with a structured reason, not
        // an internal error.
        DaemonError::Denied(vec![reason])
    })?;

    // The parallel slot and the cumulative count are charged to the parent, so a
    // sub-agent's existence is spent from the parent's budget.
    daemon
        .store
        .charge_lease(&session.lease.id, &BudgetCost::one_subagent())?;

    daemon.store.upsert_actor(Actor {
        id: actor.clone(),
        kind: ActorKind::SubAgent,
        display_name: format!("sub-agent {index}"),
        parent: Some(session.lease.actor.clone()),
        created_at: Utc::now(),
    })?;
    daemon.store.insert_lease(derived.clone())?;
    daemon
        .store
        .set_lease_state(&derived.id, clyde_core::lease::LeaseState::Active)?;
    let derived = daemon.store.get_lease(&derived.id)?;

    audit::record(
        daemon.store.as_ref(),
        audit::draft(
            AuditEventKind::LeaseDerived,
            serde_json::json!({
                "parent": session.lease.id.as_str(),
                "actor": actor.as_str(),
                "edit_paths": derived.repo_scope.edit_paths.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "tasks": derived.task_scope.iter().map(|task| task.name()).collect::<Vec<_>>(),
            }),
        )
        .mission(session.mission.id.clone())
        .lease(derived.id.clone())
        .actor(actor),
    );

    // The token is written into the sub-agent's own sandbox and is never
    // returned here: no query returns a token.
    let token = missions::bind_session(daemon, &derived)?;
    let _ = missions::write_token_file(&daemon.paths.sandbox_runtime(), &derived.id, &token)?;

    Ok(ToolResult::json(&actor_api::subagent_view(&derived)))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    #[test]
    fn a_subagent_identifier_is_derived_from_its_parent() {
        let parent = clyde_core::ids::ActorId::parse("agent:claude").unwrap();
        assert_eq!(parent.subagent(1).unwrap().as_str(), "agent:claude/1");
        // One level only: deriving from a sub-agent replaces the index rather
        // than nesting, and derivation itself refuses a derived parent.
        assert_eq!(
            parent.subagent(1).unwrap().subagent(2).unwrap().as_str(),
            "agent:claude/2"
        );
    }
}
