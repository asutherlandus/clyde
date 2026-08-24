//! Canonical digests.
//!
//! Every content identity, policy digest, request digest, and audit chain link
//! in Clyde is a BLAKE3 digest over a *canonical* encoding. Canonicalisation
//! matters: an approval is bound to a request digest, so two encodings of the
//! same request must produce one digest, and a changed request must produce a
//! different one.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;

/// A lowercase-hex BLAKE3 digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Hashes raw bytes.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    /// Hashes a domain-separated, canonically encoded value.
    ///
    /// The domain string prevents a digest computed over one kind of request
    /// from matching a digest over another kind with the same field values.
    pub fn of_canonical<T: Serialize>(domain: &str, value: &T) -> Result<Self, CanonicalError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain.as_bytes());
        hasher.update(b"\0");
        hasher.update(canonical_json(value)?.as_bytes());
        Ok(Self(hasher.finalize().to_hex().to_string()))
    }

    /// Parses a digest, rejecting anything that is not lowercase 64-char hex.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let well_formed = value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if well_formed {
            Ok(Self(value))
        } else {
            Err(ValidationError::MalformedDigest { value })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time-ish equality for digests compared against attacker-supplied
    /// values. Digests are not secrets, but approval matching compares a
    /// caller-supplied digest against a stored one, and a non-short-circuiting
    /// comparison costs nothing here.
    pub fn matches(&self, other: &Digest) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        self.0
            .bytes()
            .zip(other.0.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    #[error("value is not canonically encodable: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("value contains a non-finite number, which has no canonical encoding")]
    NonFinite,
}

/// Encodes a value as canonical JSON: object keys sorted, no insignificant
/// whitespace, and no non-finite numbers.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, CanonicalError> {
    let value = serde_json::to_value(value)?;
    let mut out = String::new();
    write_canonical(&value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &serde_json::Value, out: &mut String) -> Result<(), CanonicalError> {
    use std::fmt::Write as _;
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => {
            if n.as_f64().is_some_and(|f| !f.is_finite()) {
                return Err(CanonicalError::NonFinite);
            }
            let _ = write!(out, "{n}");
        }
        serde_json::Value::String(s) => {
            let encoded = serde_json::to_string(s)?;
            out.push_str(&encoded);
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let encoded_key = serde_json::to_string(key)?;
                out.push_str(&encoded_key);
                out.push(':');
                match map.get(key) {
                    Some(child) => write_canonical(child, out)?,
                    // Unreachable: `key` came from this map. Handled rather than
                    // indexed so no panic path exists.
                    None => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_the_digest() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
        assert_eq!(
            Digest::of_canonical("test", &a).unwrap(),
            Digest::of_canonical("test", &b).unwrap()
        );
    }

    #[test]
    fn a_changed_field_changes_the_digest() {
        let a = json!({"remote": "origin", "refspec": "refs/heads/main"});
        let b = json!({"remote": "origin", "refspec": "refs/heads/other"});
        assert_ne!(
            Digest::of_canonical("git.push", &a).unwrap(),
            Digest::of_canonical("git.push", &b).unwrap()
        );
    }

    #[test]
    fn domain_separation_prevents_cross_kind_matches() {
        let value = json!({"x": 1});
        assert_ne!(
            Digest::of_canonical("git.push", &value).unwrap(),
            Digest::of_canonical("task.escalation", &value).unwrap()
        );
    }

    #[test]
    fn nested_objects_are_canonicalised_too() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"o":{"z":1,"a":[{"y":1,"x":2}]}}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"o":{"a":[{"x":2,"y":1}],"z":1}}"#).unwrap();
        assert_eq!(canonical_json(&a).unwrap(), canonical_json(&b).unwrap());
    }

    #[test]
    fn array_order_is_significant() {
        let a = json!([1, 2]);
        let b = json!([2, 1]);
        assert_ne!(canonical_json(&a).unwrap(), canonical_json(&b).unwrap());
    }

    #[test]
    fn parse_rejects_non_hex() {
        assert!(Digest::parse("zz").is_err());
        assert!(Digest::parse("AB".repeat(32)).is_err());
        assert!(Digest::parse("ab".repeat(32)).is_ok());
    }

    #[test]
    fn matches_compares_equal_digests() {
        let a = Digest::of_bytes(b"x");
        let b = Digest::of_bytes(b"x");
        let c = Digest::of_bytes(b"y");
        assert!(a.matches(&b));
        assert!(!a.matches(&c));
    }
}
