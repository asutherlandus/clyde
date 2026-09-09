//! Actors.
//!
//! Actors hold no authority. Authority is always a property of an active lease
//! bound to an actor (schema reference: Actor).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ValidationError;
use crate::ids::{ActorId, DeclaredActorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    SubAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub id: ActorId,
    pub kind: ActorKind,
    pub display_name: String,
    /// Set for sub-agents.
    pub parent: Option<ActorId>,
    pub created_at: DateTime<Utc>,
}

const MAX_DISPLAY_NAME: usize = 128;

impl Actor {
    /// Validates the actor, including that `kind` agrees with the shape of `id`.
    ///
    /// The identifier's shape is what the transport and the audit log show, so a
    /// record whose declared kind disagrees with its identifier would make the
    /// audit trail misleading.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.display_name.trim().is_empty() {
            return Err(ValidationError::EmptyField {
                field: "display_name",
            });
        }
        if self.display_name.len() > MAX_DISPLAY_NAME {
            return Err(ValidationError::FieldTooLong {
                field: "display_name",
                max: MAX_DISPLAY_NAME,
            });
        }
        let declared = match self.id.declared_kind() {
            DeclaredActorKind::Human => ActorKind::Human,
            DeclaredActorKind::Agent => ActorKind::Agent,
            DeclaredActorKind::SubAgent => ActorKind::SubAgent,
        };
        if declared != self.kind {
            return Err(ValidationError::MalformedId {
                kind: "actor",
                value: self.id.to_string(),
                expected: "an identifier shape matching the declared actor kind",
            });
        }
        match (self.kind, &self.parent) {
            (ActorKind::SubAgent, None) => Err(ValidationError::EmptyField { field: "parent" }),
            (ActorKind::Human | ActorKind::Agent, Some(_)) => Err(ValidationError::MalformedId {
                kind: "actor",
                value: self.id.to_string(),
                expected: "no parent for a human or top-level agent",
            }),
            _ => Ok(()),
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self.kind, ActorKind::Human)
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

    fn actor(id: &str, kind: ActorKind, parent: Option<&str>) -> Actor {
        Actor {
            id: ActorId::parse(id).unwrap(),
            kind,
            display_name: "test".to_owned(),
            parent: parent.map(|p| ActorId::parse(p).unwrap()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn kind_must_agree_with_the_identifier_shape() {
        assert!(
            actor("human:andrew", ActorKind::Human, None)
                .validate()
                .is_ok()
        );
        assert!(
            actor("agent:claude", ActorKind::Agent, None)
                .validate()
                .is_ok()
        );
        assert!(
            actor("agent:claude/1", ActorKind::SubAgent, Some("agent:claude"))
                .validate()
                .is_ok()
        );
        // An agent claiming to be human is the case worth rejecting: approvals
        // require a human actor.
        assert!(
            actor("agent:claude", ActorKind::Human, None)
                .validate()
                .is_err()
        );
        assert!(
            actor("human:andrew", ActorKind::Agent, None)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn subagents_require_a_parent() {
        assert!(
            actor("agent:claude/1", ActorKind::SubAgent, None)
                .validate()
                .is_err()
        );
        assert!(
            actor("human:andrew", ActorKind::Human, Some("human:other"))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn display_name_is_bounded() {
        let mut a = actor("human:andrew", ActorKind::Human, None);
        a.display_name = "   ".to_owned();
        assert!(a.validate().is_err());
        a.display_name = "x".repeat(200);
        assert!(a.validate().is_err());
    }
}

/// How a request was authenticated.
///
/// [`ActorKind`] says what an actor *is*; this says how a given request reached
/// the daemon, and only the second is a security fact about the request
/// (D25). Every side-effecting record carries one, so "who ran this" is
/// answerable from the record rather than inferred from the lease.
///
/// It is determined by the daemon from the connection it arrived on. There is
/// no code path that lets a requester assert its own principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Principal {
    /// The admin socket, authenticated by `SO_PEERCRED`. Never reachable from a
    /// sandbox, because the admin socket is never mounted into one.
    Operator { uid: u32 },
    /// The actor socket, authenticated by a session token. Covers an agent
    /// Clyde hosts, an agent running outside Clyde, and CI.
    Session {
        session_actor: ActorId,
        /// Whether Clyde launched this actor. Observed, not declared: it is what
        /// distinguishes a warden-hosted agent from an external driver in review.
        hosted: bool,
    },
}

impl Principal {
    /// A short name for rendering and for audit payloads.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Operator { .. } => "operator",
            Self::Session { hosted: true, .. } => "hosted-agent",
            Self::Session { hosted: false, .. } => "external-actor",
        }
    }

    /// Whether this principal is a human on the admin socket.
    ///
    /// Used to decide that an approval is a self-confirmation rather than an
    /// approval ([D2 amendment](../../docs/builder/decisions.md)), never to
    /// widen or narrow what may run.
    pub fn is_operator(&self) -> bool {
        matches!(self, Self::Operator { .. })
    }
}

impl std::fmt::Display for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operator { uid } => write!(f, "operator(uid {uid})"),
            Self::Session { session_actor, .. } => {
                write!(f, "{}({session_actor})", self.kind_name())
            }
        }
    }
}
