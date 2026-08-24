//! Egress attempts.
//!
//! A refused attempt is a first-class signal, not a log line: `rust.check`
//! should never attempt egress, and if it does that is either a
//! misconfiguration or a hostile dependency probing for a way out (network
//! egress model: audit output).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::classification::HostName;
use crate::ids::TaskRunId;

/// Whether the proxy allowed a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressDecision {
    Allowed,
    Denied,
}

/// Why an attempt was denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum EgressDenialReason {
    /// The destination is not in the profile's allowlist.
    HostNotAllowlisted,
    /// The profile is `none`, so nothing should have reached the proxy at all.
    ProfileForbidsEgress,
    ByteBudgetExhausted,
    RequestBudgetExhausted,
    ConnectionBudgetExhausted,
    /// The request was not a well-formed `CONNECT`.
    MalformedRequest,
    /// The port is not one the profile permits.
    PortNotAllowed {
        port: u16,
    },
    /// The upstream could not be reached; recorded because a task result must
    /// distinguish "refused by policy" from "the network failed".
    UpstreamUnreachable,
}

impl EgressDenialReason {
    pub fn render(&self) -> String {
        match self {
            Self::HostNotAllowlisted => "destination is not in the profile's allowlist".to_owned(),
            Self::ProfileForbidsEgress => {
                "this task's egress profile is none, so no destination is reachable".to_owned()
            }
            Self::ByteBudgetExhausted => "the egress byte budget is exhausted".to_owned(),
            Self::RequestBudgetExhausted => "the egress request budget is exhausted".to_owned(),
            Self::ConnectionBudgetExhausted => {
                "the egress connection budget is exhausted".to_owned()
            }
            Self::MalformedRequest => "the request was not a well-formed CONNECT".to_owned(),
            Self::PortNotAllowed { port } => {
                format!("port {port} is not permitted by this profile")
            }
            Self::UpstreamUnreachable => "the destination could not be reached".to_owned(),
        }
    }
}

/// One recorded connection attempt.
///
/// `request_path` and `status` are populated only for terminated `model-api`
/// connections. Bodies are never recorded, and there is no configuration that
/// enables it (D7 amendment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressAttempt {
    pub task_run: Option<TaskRunId>,
    pub at: DateTime<Utc>,
    pub profile: String,
    pub host: HostName,
    pub port: u16,
    pub decision: EgressDecision,
    pub denial_reason: Option<EgressDenialReason>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Request path, for terminated connections only. Never a body.
    pub request_path: Option<String>,
    /// Upstream HTTP status, for terminated connections only.
    pub status: Option<u16>,
    pub duration_ms: u64,
}

impl EgressAttempt {
    pub fn was_allowed(&self) -> bool {
        matches!(self.decision, EgressDecision::Allowed)
    }
}

/// The aggregate view stored as a `FetchManifest` artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchManifest {
    pub profile: String,
    pub attempts: Vec<EgressAttempt>,
    pub total_bytes_in: u64,
    pub total_bytes_out: u64,
    pub refusals: u32,
}

impl FetchManifest {
    /// Summarises a set of attempts. Refusals are counted separately so they can
    /// be surfaced prominently rather than buried in a list.
    pub fn from_attempts(profile: impl Into<String>, attempts: Vec<EgressAttempt>) -> Self {
        let refusals = attempts.iter().filter(|a| !a.was_allowed()).count();
        let total_bytes_in = attempts
            .iter()
            .fold(0u64, |acc, a| acc.saturating_add(a.bytes_in));
        let total_bytes_out = attempts
            .iter()
            .fold(0u64, |acc, a| acc.saturating_add(a.bytes_out));
        Self {
            profile: profile.into(),
            attempts,
            total_bytes_in,
            total_bytes_out,
            refusals: u32::try_from(refusals).unwrap_or(u32::MAX),
        }
    }

    pub fn has_refusals(&self) -> bool {
        self.refusals > 0
    }

    /// Distinct destinations touched, for mission review.
    pub fn destinations(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .attempts
            .iter()
            .map(|attempt| attempt.host.to_string())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
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

    fn attempt(host: &str, decision: EgressDecision, bytes: u64) -> EgressAttempt {
        EgressAttempt {
            task_run: None,
            at: Utc::now(),
            profile: "rust-registry".to_owned(),
            host: HostName::parse(host).unwrap(),
            port: 443,
            decision,
            denial_reason: match decision {
                EgressDecision::Denied => Some(EgressDenialReason::HostNotAllowlisted),
                EgressDecision::Allowed => None,
            },
            bytes_in: bytes,
            bytes_out: bytes,
            request_path: None,
            status: None,
            duration_ms: 5,
        }
    }

    #[test]
    fn manifest_counts_refusals_and_aggregates_bytes() {
        let manifest = FetchManifest::from_attempts(
            "rust-registry",
            vec![
                attempt("static.crates.io", EgressDecision::Allowed, 100),
                attempt("evil.test", EgressDecision::Denied, 0),
                attempt("index.crates.io", EgressDecision::Allowed, 50),
            ],
        );
        assert_eq!(manifest.refusals, 1);
        assert!(manifest.has_refusals());
        assert_eq!(manifest.total_bytes_in, 150);
        assert_eq!(manifest.destinations().len(), 3);
    }

    #[test]
    fn an_empty_manifest_has_no_refusals() {
        let manifest = FetchManifest::from_attempts("none", vec![]);
        assert!(!manifest.has_refusals());
        assert_eq!(manifest.total_bytes_out, 0);
    }

    #[test]
    fn attempts_never_carry_a_body_field() {
        // Asserted structurally: the type has no body field, so no code path can
        // record one. This test documents the invariant against future edits.
        let json = serde_json::to_value(attempt("crates.io", EgressDecision::Allowed, 1)).unwrap();
        let object = json.as_object().unwrap();
        assert!(!object.contains_key("body"));
        assert!(!object.contains_key("payload"));
        assert!(object.contains_key("request_path"));
    }

    #[test]
    fn byte_totals_saturate() {
        let mut a = attempt("crates.io", EgressDecision::Allowed, u64::MAX);
        a.bytes_in = u64::MAX;
        let manifest =
            FetchManifest::from_attempts("x", vec![a, attempt("b.io", EgressDecision::Allowed, 5)]);
        assert_eq!(manifest.total_bytes_in, u64::MAX);
    }

    #[test]
    fn denial_reasons_render() {
        for reason in [
            EgressDenialReason::HostNotAllowlisted,
            EgressDenialReason::ProfileForbidsEgress,
            EgressDenialReason::ByteBudgetExhausted,
            EgressDenialReason::MalformedRequest,
            EgressDenialReason::PortNotAllowed { port: 22 },
            EgressDenialReason::UpstreamUnreachable,
        ] {
            assert!(!reason.render().is_empty());
        }
    }
}
