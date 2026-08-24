//! `clyde-broker-api`: the broker's wire contract.
//!
//! The interface is **capability-oriented, never secret-oriented**:
//! [`BrokerRequest::GitPush`] exists; there is no request that returns a key,
//! a token, or an agent socket, and adding one would be a change to this file
//! that a reviewer would see.
//!
//! The broker validates every request independently. It does not trust clyded's
//! word that an approval exists — it verifies the approval record itself — so
//! this contract carries the identifiers needed for that verification rather
//! than a boolean saying it happened.

use std::path::PathBuf;

use clyde_core::Digest;
use clyde_core::ids::{ApprovalId, BrokerOpId, LeaseId, MissionId};
use serde::{Deserialize, Serialize};

pub mod transport;

/// What the broker can be asked to do.
///
/// Closed, and deliberately small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum BrokerRequest {
    /// What this broker supports and what it holds. Answers with capability
    /// names, never with credential material.
    Capabilities,
    /// Push an approved commit.
    ///
    /// Boxed so the two variants are close in size: the enum is passed by
    /// value across the transport and a large inline variant would make the
    /// capabilities query as expensive to move as a push request.
    GitPush(Box<GitPushRequest>),
}

/// A brokered push.
///
/// The remote *name* and the resolved *URL* are both carried: the name is what
/// the human approved, the URL is what the broker will actually contact, and
/// carrying both lets the broker check they still agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitPushRequest {
    pub operation: BrokerOpId,
    pub mission: MissionId,
    pub lease: LeaseId,
    /// The approval this executes under. The broker re-reads it.
    pub approval: ApprovalId,
    /// Digest over the normalised request. Must equal the approval's.
    pub request_digest: Digest,
    /// Host path of the workspace repository the commit is fetched from.
    pub workspace: PathBuf,
    pub remote: String,
    pub remote_url: String,
    pub refspec: String,
    pub commit: String,
    /// The tree the approval covered, checked after the fetch.
    pub expected_tree: String,
}

impl GitPushRequest {
    /// Recomputes the digest from the fields that identify the operation.
    ///
    /// The broker compares this against both the carried digest and the stored
    /// approval, so a caller cannot supply a digest that does not describe the
    /// request it is making.
    pub fn compute_digest(&self) -> Result<Digest, clyde_core::digest::CanonicalError> {
        let normalised = serde_json::json!({
            "mission": self.mission.as_str(),
            "lease": self.lease.as_str(),
            "remote": self.remote,
            "refspec": self.refspec,
            "commit": self.commit,
            "tree": self.expected_tree,
        });
        Digest::of_canonical("clyde.broker.git-push.v1", &normalised)
    }

    /// Whether the carried digest describes this request.
    pub fn digest_matches(&self) -> bool {
        self.compute_digest()
            .map(|computed| computed.matches(&self.request_digest))
            .unwrap_or(false)
    }
}

/// What the broker answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum BrokerResponse {
    Capabilities(Capabilities),
    Pushed(PushOutcome),
    /// The broker declined. `reason` is safe to show a human and contains no
    /// credential material.
    Refused {
        reason: RefusalReason,
    },
}

/// What the broker supports.
///
/// `holds_credential` is a boolean, not a description: whether a credential is
/// present is operationally useful, and what it is never is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub version: String,
    /// Capability names, such as `git_push`.
    pub operations: Vec<String>,
    pub holds_credential: bool,
    /// How the credential is supplied, by kind only: `ssh-agent` or `ssh-key`.
    pub credential_kind: Option<String>,
    pub remote_allowlist: Vec<String>,
    pub branch_allowlist: Vec<String>,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOutcome {
    pub operation: BrokerOpId,
    pub commit: String,
    pub refspec: String,
    /// Human-readable result. Never includes the remote's authentication detail.
    pub summary: String,
}

/// Why the broker refused.
///
/// Structured so clyded can record the refusal without re-deriving it from
/// prose, and so a refusal can be rendered to a human as a policy statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RefusalReason {
    /// The caller was not clyded.
    UntrustedCaller {
        detail: String,
    },
    /// No approval record matched, or it was expired or already consumed.
    ApprovalInvalid {
        detail: String,
    },
    /// The carried digest does not describe the request.
    DigestMismatch,
    RemoteNotAllowlisted {
        remote: String,
    },
    BranchNotAllowlisted {
        branch: String,
    },
    ProtectedBranch {
        branch: String,
    },
    /// The fetched commit or tree did not match the approval.
    ContentMismatch {
        detail: String,
    },
    /// The broker holds no usable credential.
    NoCredential,
    MalformedRequest {
        detail: String,
    },
    /// Git itself failed. Carries git's stderr, which does not contain the
    /// credential: the broker never places one on a command line.
    GitFailed {
        detail: String,
    },
    /// The operation was frozen by mission revocation while in flight.
    Frozen,
}

impl RefusalReason {
    pub fn render(&self) -> String {
        match self {
            Self::UntrustedCaller { detail } => {
                format!("the broker accepts requests only from clyded: {detail}")
            }
            Self::ApprovalInvalid { detail } => format!(
                "no matching, unexpired, unconsumed approval exists for this exact request: {detail}"
            ),
            Self::DigestMismatch => {
                "the request digest does not describe the request that was made".to_owned()
            }
            Self::RemoteNotAllowlisted { remote } => {
                format!("remote {remote} is not in the configured allowlist")
            }
            Self::BranchNotAllowlisted { branch } => {
                format!("branch {branch} does not match any allowlisted pattern")
            }
            Self::ProtectedBranch { branch } => {
                format!("{branch} matches a protected-branch pattern and is refused outright")
            }
            Self::ContentMismatch { detail } => format!(
                "what was fetched does not match what was approved, so nothing was pushed: {detail}"
            ),
            Self::NoCredential => {
                "the broker holds no usable credential for this operation".to_owned()
            }
            Self::MalformedRequest { detail } => format!("the request was malformed: {detail}"),
            Self::GitFailed { detail } => format!("git failed: {detail}"),
            Self::Frozen => {
                "the mission was revoked while this operation was in flight, so it was frozen"
                    .to_owned()
            }
        }
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

    fn request() -> GitPushRequest {
        let mut request = GitPushRequest {
            operation: ids::new::broker_op_id().unwrap(),
            mission: ids::new::mission_id().unwrap(),
            lease: ids::new::lease_id().unwrap(),
            approval: ids::new::approval_id().unwrap(),
            request_digest: Digest::of_bytes(b"placeholder"),
            workspace: PathBuf::from("/srv/project"),
            remote: "origin".to_owned(),
            remote_url: "git@github.test:org/repo.git".to_owned(),
            refspec: "refs/heads/feature".to_owned(),
            commit: "a".repeat(40),
            expected_tree: "b".repeat(40),
        };
        request.request_digest = request.compute_digest().unwrap();
        request
    }

    #[test]
    fn the_digest_covers_the_fields_that_identify_the_operation() {
        let request = request();
        assert!(request.digest_matches());

        for altered in [
            GitPushRequest {
                refspec: "refs/heads/main".to_owned(),
                ..request.clone()
            },
            GitPushRequest {
                commit: "c".repeat(40),
                ..request.clone()
            },
            GitPushRequest {
                remote: "fork".to_owned(),
                ..request.clone()
            },
            GitPushRequest {
                expected_tree: "d".repeat(40),
                ..request.clone()
            },
        ] {
            assert!(
                !altered.digest_matches(),
                "an altered request must not carry the original digest"
            );
        }
    }

    #[test]
    fn the_resolved_url_is_not_part_of_the_digest_but_the_remote_name_is() {
        // The human approves a remote *name*; the URL is resolved from
        // configuration, so a configuration change is not an approval mismatch,
        // while a different remote is.
        let request = request();
        let other_url = GitPushRequest {
            remote_url: "https://github.test/org/repo.git".to_owned(),
            ..request.clone()
        };
        assert!(other_url.digest_matches());
    }

    #[test]
    fn there_is_no_request_that_returns_credential_material() {
        // Structural: the request enum has two variants, and neither asks for a
        // secret. A `get_ssh_key` would have to be added here.
        let encoded = serde_json::to_string(&BrokerRequest::Capabilities).unwrap();
        assert_eq!(encoded, r#"{"request":"capabilities"}"#);
        for forbidden in [
            "get_ssh_key",
            "get_token",
            "read_credential",
            "agent_socket",
        ] {
            let attempt =
                serde_json::from_str::<BrokerRequest>(&format!(r#"{{"request":"{forbidden}"}}"#));
            assert!(attempt.is_err(), "{forbidden} must not be representable");
        }
    }

    #[test]
    fn capabilities_report_presence_not_content() {
        let capabilities = Capabilities {
            version: "0.1.0".to_owned(),
            operations: vec!["git_push".to_owned()],
            holds_credential: true,
            credential_kind: Some("ssh-agent".to_owned()),
            remote_allowlist: vec!["origin".to_owned()],
            branch_allowlist: vec!["feature/*".to_owned()],
            protected_branches: vec!["main".to_owned()],
        };
        let encoded = serde_json::to_string(&capabilities).unwrap();
        assert!(encoded.contains("ssh-agent"));
        assert!(
            !encoded.contains("BEGIN") && !encoded.contains("ssh-rsa"),
            "capabilities must never carry key material: {encoded}"
        );
    }

    #[test]
    fn refusals_render_as_policy_statements() {
        for reason in [
            RefusalReason::UntrustedCaller {
                detail: "peer uid 1001".to_owned(),
            },
            RefusalReason::ApprovalInvalid {
                detail: "already consumed".to_owned(),
            },
            RefusalReason::DigestMismatch,
            RefusalReason::RemoteNotAllowlisted {
                remote: "evil".to_owned(),
            },
            RefusalReason::BranchNotAllowlisted {
                branch: "x".to_owned(),
            },
            RefusalReason::ProtectedBranch {
                branch: "main".to_owned(),
            },
            RefusalReason::ContentMismatch {
                detail: "tree differs".to_owned(),
            },
            RefusalReason::NoCredential,
            RefusalReason::MalformedRequest {
                detail: "bad refspec".to_owned(),
            },
            RefusalReason::GitFailed {
                detail: "rejected".to_owned(),
            },
            RefusalReason::Frozen,
        ] {
            let rendered = reason.render();
            assert!(!rendered.is_empty());
            assert!(!rendered.contains("PRIVATE KEY"));
        }
    }

    #[test]
    fn requests_reject_unknown_fields() {
        let attempt = serde_json::from_value::<GitPushRequest>(serde_json::json!({
            "operation": "bo-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "mission": "m-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "lease": "l-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "approval": "ap-01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "request_digest": "ab".repeat(32),
            "workspace": "/srv/project",
            "remote": "origin",
            "remote_url": "git@x:y.git",
            "refspec": "refs/heads/f",
            "commit": "a".repeat(40),
            "expected_tree": "b".repeat(40),
            "force": true
        }));
        assert!(attempt.is_err(), "an unexpected field must not be ignored");
    }
}
