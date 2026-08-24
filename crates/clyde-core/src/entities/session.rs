//! Actor sessions and capability tokens (D2).
//!
//! The plaintext token exists only in the sandbox token file and in the issuing
//! code path. It is never logged, never returned in a query result, and never
//! stored: the store holds a SHA-256 of it.

use chrono::{DateTime, Utc};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::ValidationError;
use crate::ids::{ActorId, LeaseId};
use crate::redact::Redacted;

/// A 256-bit capability token in plaintext.
///
/// Deliberately not `Serialize`, `Clone`, or `Debug`-revealing: a type that
/// cannot be serialised cannot reach an audit payload or an API response by
/// accident.
pub struct SessionToken(Redacted<[u8; 32]>);

/// Failure to generate a token.
#[derive(Debug, thiserror::Error)]
#[error("could not generate a capability token from the system entropy source")]
pub struct TokenGenerationError;

impl SessionToken {
    /// Generates a token from the operating system's entropy source.
    ///
    /// A failure here is fatal to session creation and must not be papered over
    /// with a weaker source.
    pub fn generate() -> Result<Self, TokenGenerationError> {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| TokenGenerationError)?;
        Ok(Self(Redacted::new(bytes, "session token")))
    }

    /// Parses a token presented by an actor.
    ///
    /// Accepts the lowercase hex encoding written to the sandbox token file.
    pub fn parse_hex(value: &str) -> Result<Self, ValidationError> {
        let trimmed = value.trim();
        if trimmed.len() != 64 {
            return Err(ValidationError::MalformedDigest {
                value: "<token>".to_owned(),
            });
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in trimmed.as_bytes().chunks_exact(2).enumerate() {
            let hex = std::str::from_utf8(chunk).map_err(|_| ValidationError::MalformedDigest {
                value: "<token>".to_owned(),
            })?;
            let byte =
                u8::from_str_radix(hex, 16).map_err(|_| ValidationError::MalformedDigest {
                    value: "<token>".to_owned(),
                })?;
            match bytes.get_mut(index) {
                Some(slot) => *slot = byte,
                // Unreachable given the length check above; handled rather than
                // indexed so no panic path exists.
                None => {
                    return Err(ValidationError::MalformedDigest {
                        value: "<token>".to_owned(),
                    });
                }
            }
        }
        Ok(Self(Redacted::new(bytes, "session token")))
    }

    /// The hex encoding written to `/run/clyde/session-token` (mode 0400).
    ///
    /// Stays wrapped so it cannot be logged or formatted by accident.
    pub fn to_hex(&self) -> Redacted<String> {
        let hex = self
            .0
            .expose()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Redacted::new(hex, "session token")
    }

    /// The stored form.
    pub fn hash(&self) -> TokenHash {
        let mut hasher = Sha256::new();
        hasher.update(self.0.expose());
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        TokenHash(out)
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(elided)")
    }
}

/// SHA-256 of a session token. Safe to store and compare; not safe to guess.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenHash(#[serde(with = "hex_bytes")] [u8; 32]);

impl TokenHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Non-short-circuiting comparison, since one side is attacker-supplied.
    pub fn matches(&self, other: &TokenHash) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn from_hex(value: &str) -> Result<Self, ValidationError> {
        if value.len() != 64 {
            return Err(ValidationError::MalformedDigest {
                value: "<token hash>".to_owned(),
            });
        }
        let mut bytes = [0u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let hex = std::str::from_utf8(chunk).map_err(|_| ValidationError::MalformedDigest {
                value: "<token hash>".to_owned(),
            })?;
            let byte =
                u8::from_str_radix(hex, 16).map_err(|_| ValidationError::MalformedDigest {
                    value: "<token hash>".to_owned(),
                })?;
            match bytes.get_mut(index) {
                Some(slot) => *slot = byte,
                None => {
                    return Err(ValidationError::MalformedDigest {
                        value: "<token hash>".to_owned(),
                    });
                }
            }
        }
        Ok(Self(bytes))
    }
}

/// A token hash prints as a hash, never as a token, but is still elided in
/// `Debug` so that a log line cannot be mistaken for one containing a secret.
impl std::fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenHash({}…)", &self.to_hex().get(..8).unwrap_or("")) // PANIC-JUSTIFIED: get returns Option
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let raw = String::deserialize(deserializer)?;
        super::TokenHash::from_hex(&raw)
            .map(|hash| hash.0)
            .map_err(serde::de::Error::custom)
    }
}

/// An actor session: a token bound to a lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorSession {
    pub actor: ActorId,
    pub lease: LeaseId,
    pub token_hash: TokenHash,
    pub issued_at: DateTime<Utc>,
    /// Never later than the lease's expiry: token expiry is derived from lease
    /// expiry, never independent of it.
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Identifier of the workspace-environment sandbox hosting this actor.
    pub sandbox: Option<String>,
}

impl ActorSession {
    /// Whether the session authenticates a request at `now`.
    ///
    /// An unknown, expired, or revoked token must be rejected identically, with
    /// no information about which (Phase 1 deliverable 4), so callers use this
    /// single boolean rather than inspecting the fields.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && now < self.expires_at
    }

    pub fn validate(&self, lease_expiry: DateTime<Utc>) -> Result<(), ValidationError> {
        if self.expires_at <= self.issued_at {
            return Err(ValidationError::ExpiryNotAfterIssue {
                issued_at: self.issued_at.to_rfc3339(),
                expires_at: self.expires_at.to_rfc3339(),
            });
        }
        if self.expires_at > lease_expiry {
            return Err(ValidationError::ExpiryNotAfterIssue {
                issued_at: self.expires_at.to_rfc3339(),
                expires_at: lease_expiry.to_rfc3339(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn tokens_are_distinct_and_hash_stably() {
        let a = SessionToken::generate().unwrap();
        let b = SessionToken::generate().unwrap();
        assert_ne!(a.hash().to_hex(), b.hash().to_hex());
        assert!(a.hash().matches(&a.hash()));
        assert!(!a.hash().matches(&b.hash()));
    }

    #[test]
    fn hex_round_trip_preserves_the_hash() {
        let token = SessionToken::generate().unwrap();
        let hex = token.to_hex();
        let parsed = SessionToken::parse_hex(hex.expose()).unwrap();
        assert!(parsed.hash().matches(&token.hash()));
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(SessionToken::parse_hex("").is_err());
        assert!(SessionToken::parse_hex("zz".repeat(32).as_str()).is_err());
        assert!(SessionToken::parse_hex(&"a".repeat(63)).is_err());
    }

    #[test]
    fn token_debug_and_display_never_reveal_the_secret() {
        let token = SessionToken::generate().unwrap();
        let hex = token.to_hex();
        let secret = hex.expose().clone();
        assert!(!format!("{token:?}").contains(&secret));
        assert!(!format!("{hex:?}").contains(&secret));
        assert!(!format!("{hex}").contains(&secret));
    }

    #[test]
    fn token_hash_serialises_as_hex_and_round_trips() {
        let hash = SessionToken::generate().unwrap().hash();
        let json = serde_json::to_string(&hash).unwrap();
        let back: TokenHash = serde_json::from_str(&json).unwrap();
        assert!(hash.matches(&back));
        assert!(serde_json::from_str::<TokenHash>("\"short\"").is_err());
    }

    #[test]
    fn session_validity_covers_revocation_and_expiry() {
        let issued = Utc::now();
        let session = ActorSession {
            actor: crate::ids::ActorId::parse("agent:claude").unwrap(),
            lease: crate::ids::new::lease_id().unwrap(),
            token_hash: SessionToken::generate().unwrap().hash(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::minutes(30),
            revoked_at: None,
            sandbox: None,
        };
        assert!(session.is_valid_at(issued));
        assert!(!session.is_valid_at(issued + chrono::Duration::hours(1)));
        let revoked = ActorSession {
            revoked_at: Some(issued),
            ..session.clone()
        };
        assert!(!revoked.is_valid_at(issued));
    }

    #[test]
    fn session_expiry_may_not_exceed_lease_expiry() {
        let issued = Utc::now();
        let lease_expiry = issued + chrono::Duration::minutes(10);
        let session = ActorSession {
            actor: crate::ids::ActorId::parse("agent:claude").unwrap(),
            lease: crate::ids::new::lease_id().unwrap(),
            token_hash: SessionToken::generate().unwrap().hash(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::hours(1),
            revoked_at: None,
            sandbox: None,
        };
        assert!(session.validate(lease_expiry).is_err());
        let ok = ActorSession {
            expires_at: lease_expiry,
            ..session
        };
        assert!(ok.validate(lease_expiry).is_ok());
    }
}
