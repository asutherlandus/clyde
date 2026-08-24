//! Typed newtype identifiers.
//!
//! Every entity identifier is a validated newtype, never a bare `String` at an
//! API boundary (Phase 0 deliverable 4). Construction goes through `parse`,
//! which rejects malformed values, so an identifier value in hand is known to be
//! well formed.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ValidationError;

/// Upper bound on any identifier, so untrusted input cannot allocate without
/// limit through an identifier field.
const MAX_ID_BYTES: usize = 128;

/// Generates a validated newtype over `String`.
///
/// `$kind` is the human-readable name used in diagnostics; `$expected`
/// describes the accepted shape and appears in the error a caller sees.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal, $expected:literal, $validate:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parses and validates an identifier.
            pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value: String = value.into();
                if value.is_empty() {
                    return Err(ValidationError::EmptyId { kind: $kind });
                }
                if value.len() > MAX_ID_BYTES {
                    return Err(ValidationError::IdTooLong {
                        kind: $kind,
                        max: MAX_ID_BYTES,
                    });
                }
                let validate: fn(&str) -> bool = $validate;
                if !validate(&value) {
                    return Err(ValidationError::MalformedId {
                        kind: $kind,
                        value,
                        expected: $expected,
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// A 26-character Crockford base32 ULID.
fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes().all(|b| {
            b.is_ascii_digit()
                || (b.is_ascii_uppercase() && b != b'I' && b != b'L' && b != b'O' && b != b'U')
        })
}

fn prefixed_ulid(prefix: &str) -> impl Fn(&str) -> bool + '_ {
    move |s: &str| match s.strip_prefix(prefix) {
        Some(rest) => is_ulid(rest),
        None => false,
    }
}

fn is_blake3_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Content-addressed identifier shape: `<prefix>blake3:<64 hex>`.
fn prefixed_blake3(prefix: &str) -> impl Fn(&str) -> bool + '_ {
    move |s: &str| match s.strip_prefix(prefix) {
        Some(rest) => match rest.strip_prefix("blake3:") {
            Some(hex) => is_blake3_hex(hex),
            None => false,
        },
        None => false,
    }
}

string_id!(
    /// Mission identifier, `m-<ulid>`.
    MissionId, "mission", "m-<ulid>", |s| prefixed_ulid("m-")(s)
);
string_id!(
    /// Lease identifier, `l-<ulid>`.
    LeaseId, "lease", "l-<ulid>", |s| prefixed_ulid("l-")(s)
);
string_id!(
    /// Task run identifier, `t-<ulid>`.
    TaskRunId, "task run", "t-<ulid>", |s| prefixed_ulid("t-")(s)
);
string_id!(
    /// Approval identifier, `ap-<ulid>`.
    ApprovalId, "approval", "ap-<ulid>", |s| prefixed_ulid("ap-")(s)
);
string_id!(
    /// Workspace identifier, `w-<ulid>`.
    WorkspaceId, "workspace", "w-<ulid>", |s| prefixed_ulid("w-")(s)
);
string_id!(
    /// Brokered operation identifier, `bo-<ulid>`.
    BrokerOpId, "brokered operation", "bo-<ulid>", |s| prefixed_ulid("bo-")(s)
);
string_id!(
    /// Policy decision identifier, `pd-<ulid>`.
    PolicyDecisionId, "policy decision", "pd-<ulid>", |s| prefixed_ulid("pd-")(s)
);
string_id!(
    /// Snapshot identifier, `s-blake3:<hex>`. Identity is the manifest content.
    SnapshotId, "snapshot", "s-blake3:<64 hex>", |s| prefixed_blake3("s-")(s)
);
string_id!(
    /// Artifact identifier, `a-blake3:<hex>`. Identity is the content.
    ArtifactId, "artifact", "a-blake3:<64 hex>", |s| prefixed_blake3("a-")(s)
);
string_id!(
    /// Actor identifier: `human:<name>`, `agent:<name>`, or `agent:<name>/<n>`.
    ActorId, "actor", "human:<name> | agent:<name> | agent:<name>/<n>", is_actor_id
);

fn is_actor_id(s: &str) -> bool {
    let Some((kind, rest)) = s.split_once(':') else {
        return false;
    };
    match kind {
        "human" => is_actor_name(rest),
        "agent" => match rest.split_once('/') {
            Some((name, index)) => {
                is_actor_name(name)
                    && !index.is_empty()
                    && index.bytes().all(|b| b.is_ascii_digit())
            }
            None => is_actor_name(rest),
        },
        _ => false,
    }
}

fn is_actor_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Fresh-identifier constructors.
///
/// ULIDs are used where creation order matters; content hashes where identity
/// *is* the content, and those identifiers are built by the content-addressed
/// stores rather than here.
pub mod new {
    use super::*;

    fn ulid() -> String {
        ulid::Ulid::new().to_string()
    }

    /// Builds an identifier from a freshly generated ULID.
    ///
    /// The `parse` call cannot fail for a generated ULID, but the result is
    /// still propagated rather than unwrapped so no panic path exists.
    macro_rules! fresh {
        ($fn_name:ident -> $ty:ty, $prefix:literal) => {
            pub fn $fn_name() -> Result<$ty, ValidationError> {
                <$ty>::parse(format!(concat!($prefix, "{}"), ulid()))
            }
        };
    }

    fresh!(mission_id -> MissionId, "m-");
    fresh!(lease_id -> LeaseId, "l-");
    fresh!(task_run_id -> TaskRunId, "t-");
    fresh!(approval_id -> ApprovalId, "ap-");
    fresh!(workspace_id -> WorkspaceId, "w-");
    fresh!(broker_op_id -> BrokerOpId, "bo-");
    fresh!(policy_decision_id -> PolicyDecisionId, "pd-");
}

impl ActorId {
    /// The actor kind encoded in the identifier.
    ///
    /// The kind is part of the identifier's validated shape, so this is a
    /// classification of known-good input rather than a parse.
    pub fn declared_kind(&self) -> DeclaredActorKind {
        if self.0.starts_with("human:") {
            DeclaredActorKind::Human
        } else if self.0.contains('/') {
            DeclaredActorKind::SubAgent
        } else {
            DeclaredActorKind::Agent
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self.declared_kind(), DeclaredActorKind::Human)
    }

    /// Derives the identifier for the `n`th sub-agent of this actor.
    pub fn subagent(&self, index: u32) -> Result<ActorId, ValidationError> {
        let base = self.0.split('/').next().unwrap_or(self.0.as_str());
        ActorId::parse(format!("{base}/{index}"))
    }
}

/// The actor kind implied by an [`ActorId`]'s shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredActorKind {
    Human,
    Agent,
    SubAgent,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn accepts_well_formed_ids() {
        assert!(MissionId::parse(format!("m-{ULID}")).is_ok());
        assert!(LeaseId::parse(format!("l-{ULID}")).is_ok());
        assert!(ActorId::parse("human:andrew").is_ok());
        assert!(ActorId::parse("agent:claude").is_ok());
        assert!(ActorId::parse("agent:claude/3").is_ok());
        assert!(SnapshotId::parse(format!("s-blake3:{}", "ab".repeat(32))).is_ok());
        assert!(ArtifactId::parse(format!("a-blake3:{}", "0f".repeat(32))).is_ok());
    }

    #[test]
    fn rejects_wrong_prefix() {
        let err = MissionId::parse(format!("l-{ULID}")).expect_err("wrong prefix must be rejected");
        assert!(matches!(
            err,
            ValidationError::MalformedId {
                kind: "mission",
                ..
            }
        ));
    }

    #[test]
    fn rejects_empty_and_oversized() {
        assert_eq!(
            MissionId::parse(""),
            Err(ValidationError::EmptyId { kind: "mission" })
        );
        let long = format!("m-{}", "A".repeat(200));
        assert_eq!(
            MissionId::parse(long),
            Err(ValidationError::IdTooLong {
                kind: "mission",
                max: MAX_ID_BYTES
            })
        );
    }

    #[test]
    fn rejects_ulid_with_excluded_letters() {
        // Crockford base32 excludes I, L, O and U.
        assert!(MissionId::parse("m-01ARZ3NDEKTSV4RRFFQ69G5FAI").is_err());
        assert!(MissionId::parse("m-lowercaseulidnotaccepted").is_err());
    }

    #[test]
    fn rejects_malformed_actor_ids() {
        for bad in [
            "andrew",
            "root:andrew",
            "human:",
            "agent:claude/",
            "agent:claude/x",
            "human:andrew;rm -rf /",
        ] {
            assert!(ActorId::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn rejects_malformed_content_ids() {
        assert!(SnapshotId::parse("s-blake3:short").is_err());
        assert!(SnapshotId::parse(format!("s-sha256:{}", "ab".repeat(32))).is_err());
        // Uppercase hex is rejected so a digest has exactly one encoding.
        assert!(SnapshotId::parse(format!("s-blake3:{}", "AB".repeat(32))).is_err());
    }

    #[test]
    fn declared_kind_follows_shape() {
        assert_eq!(
            ActorId::parse("human:a").unwrap().declared_kind(),
            DeclaredActorKind::Human
        );
        assert_eq!(
            ActorId::parse("agent:a").unwrap().declared_kind(),
            DeclaredActorKind::Agent
        );
        assert_eq!(
            ActorId::parse("agent:a/1").unwrap().declared_kind(),
            DeclaredActorKind::SubAgent
        );
    }

    #[test]
    fn subagent_derivation_does_not_nest() {
        let parent = ActorId::parse("agent:claude").unwrap();
        let child = parent.subagent(2).unwrap();
        assert_eq!(child.as_str(), "agent:claude/2");
        // One level of derivation only (derivation rule 6): deriving from a
        // sub-agent replaces the index rather than nesting.
        assert_eq!(child.subagent(5).unwrap().as_str(), "agent:claude/5");
    }

    #[test]
    fn fresh_ids_round_trip() {
        let id = new::mission_id().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: MissionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn deserialisation_validates() {
        let err = serde_json::from_str::<MissionId>("\"nope\"");
        assert!(err.is_err(), "deserialisation must validate, not trust");
    }
}
