//! Policy decisions.
//!
//! Recorded for every admission check, allowed or denied, so that "why did this
//! happen" is answerable after the fact (schema reference: Policy decision).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::budget::BudgetDimension;
use crate::digest::Digest;
use crate::entities::classification::{CredentialPolicy, EgressProfile, IsolationLevel};
use crate::entities::task::TaskType;
use crate::ids::{PolicyDecisionId, TaskRunId};
use crate::repo_path::RepoPath;

/// What the decision was about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", rename_all = "snake_case")]
pub enum PolicySubject {
    TaskRequest {
        task: TaskType,
        run: Option<TaskRunId>,
    },
    Escalation {
        task: TaskType,
    },
    SubagentRequest,
    Publish,
    BaselineConfirmation {
        target: RepoPath,
    },
}

/// The outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allowed,
    AllowedWithApproval,
    Denied,
}

impl PolicyOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::AllowedWithApproval => "allowed_with_approval",
            Self::Denied => "denied",
        }
    }

    pub fn permits_immediately(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Structured denial reasons.
///
/// Structured rather than prose so denials can be rendered as actionable
/// messages and asserted in tests. An agent that receives "denied" with no path
/// forward will either loop or try to work around the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PolicyReason {
    OutOfLeaseScope {
        path: RepoPath,
    },
    NotWritableUnderLease {
        path: RepoPath,
    },
    TaskNotInLease {
        task: TaskType,
    },
    TaskNotInMission {
        task: TaskType,
    },
    BudgetExhausted {
        dimension: BudgetDimension,
    },
    BudgetWiderThanParent {
        dimension: BudgetDimension,
    },
    LeaseExpired,
    LeaseNotActive,
    MissionNotActive,
    EgressWiderThanLease {
        requested: String,
        ceiling: String,
    },
    EgressProfilesIncomparable {
        requested: String,
        ceiling: String,
    },
    CredentialsWiderThanLease,
    AuthorityFlagNotHeld {
        flag: String,
    },
    /// One level of derivation only in the MVP (derivation rule 6).
    SubagentDerivationDepthExceeded,
    ApprovalRequired {
        subject: String,
    },
    ApprovalMissingOrStale,
    /// A task with no confirmed baseline is refused (D18). No implicit
    /// wide-scope first run.
    AccessBaselineMissing {
        target: RepoPath,
    },
    AccessBaselineDrift {
        rendered: String,
    },
    /// The capability exists but is not enabled in this phase (Phase 1
    /// deliverable 7): the same path escalations later use.
    PhaseGated {
        capability: String,
    },
    /// Isolation the host cannot provide, so the task is refused rather than
    /// run weaker (D22, Phase 2b deliverable 2).
    IsolationUnavailable {
        required: IsolationLevel,
    },
    CgroupLimitsUnavailable,
    /// Repository configuration attempted to widen authority (D14, D20).
    RepositoryConfigWidensAuthority {
        key: String,
    },
    ProtectedBranch {
        branch: String,
    },
    RemoteNotAllowlisted {
        remote: String,
    },
    /// Private sources are out of MVP scope and denied rather than half-supported.
    UnsupportedInMvp {
        what: String,
    },
}

impl PolicyReason {
    /// A single actionable sentence, safe to show an actor.
    ///
    /// Contains no host paths and no secrets, because it crosses to the actor
    /// surface (schema reference: wire representation).
    pub fn render(&self) -> String {
        match self {
            Self::OutOfLeaseScope { path } => {
                format!("{path} is outside this lease's read scope")
            }
            Self::NotWritableUnderLease { path } => {
                format!("{path} is not writable under this lease")
            }
            Self::TaskNotInLease { task } => {
                format!("{task} is not in this lease's task scope")
            }
            Self::TaskNotInMission { task } => {
                format!("{task} is not in the mission's allowed tasks")
            }
            Self::BudgetExhausted { dimension } => {
                format!("the {dimension} budget is exhausted")
            }
            Self::BudgetWiderThanParent { dimension } => {
                format!(
                    "the requested {dimension} budget exceeds the parent lease's remaining budget"
                )
            }
            Self::LeaseExpired => "this lease has expired".to_owned(),
            Self::LeaseNotActive => "this lease is not active".to_owned(),
            Self::MissionNotActive => "the mission is not active".to_owned(),
            Self::EgressWiderThanLease { requested, ceiling } => {
                format!("egress profile {requested} is wider than this lease's ceiling {ceiling}")
            }
            Self::EgressProfilesIncomparable { requested, ceiling } => format!(
                "egress profiles {requested} and {ceiling} are incomparable; this needs an escalation evaluated against the mission, not a derivation"
            ),
            Self::CredentialsWiderThanLease => {
                "the requested credential scope is wider than this lease's".to_owned()
            }
            Self::AuthorityFlagNotHeld { flag } => {
                format!("this lease does not hold {flag}")
            }
            Self::SubagentDerivationDepthExceeded => {
                "sub-agents may not spawn further sub-agents in this version".to_owned()
            }
            Self::ApprovalRequired { subject } => {
                format!("{subject} requires human approval on the admin channel")
            }
            Self::ApprovalMissingOrStale => {
                "no matching, unexpired, unconsumed approval exists for this exact request"
                    .to_owned()
            }
            Self::AccessBaselineMissing { target } => format!(
                "no confirmed access baseline exists for {target}; a human must confirm one before this task can run"
            ),
            Self::AccessBaselineDrift { rendered } => rendered.clone(),
            Self::PhaseGated { capability } => {
                format!("{capability} is not enabled in this version")
            }
            Self::IsolationUnavailable { required } => format!(
                "this task requires {required} isolation, which this host cannot provide; it is refused rather than run at a weaker boundary"
            ),
            Self::CgroupLimitsUnavailable => {
                "cgroup v2 delegation is unavailable, and build tasks are refused without it"
                    .to_owned()
            }
            Self::RepositoryConfigWidensAuthority { key } => format!(
                "repository configuration may not set {key}, because repository content cannot widen its own authority"
            ),
            Self::ProtectedBranch { branch } => {
                format!("{branch} matches a protected-branch pattern and cannot be pushed")
            }
            Self::RemoteNotAllowlisted { remote } => {
                format!("remote {remote} is not in the configured allowlist")
            }
            Self::UnsupportedInMvp { what } => {
                format!(
                    "{what} is not supported in this version and is refused rather than half-supported"
                )
            }
        }
    }

    /// A narrower or escalated alternative, where one exists.
    ///
    /// A denial with no path forward is worse than a clear next step.
    pub fn suggested_alternative(&self) -> Option<String> {
        match self {
            Self::TaskNotInLease { task } | Self::TaskNotInMission { task } => Some(format!(
                "call request_escalation for {task} with a reason, or ask for a mission whose scope includes it"
            )),
            Self::OutOfLeaseScope { path } | Self::NotWritableUnderLease { path } => Some(format!(
                "work within the lease's scope, or request a scope expansion naming {path}"
            )),
            Self::LeaseExpired => {
                Some("ask the operator to renew the lease, which issues a replacement".to_owned())
            }
            Self::BudgetExhausted { .. } => {
                Some("ask the operator to renew the mission with additional budget".to_owned())
            }
            Self::AccessBaselineMissing { .. } => Some(
                "ask the operator to run `clyde access propose` and confirm the baseline".to_owned(),
            ),
            Self::AccessBaselineDrift { .. } => Some(
                "raise an escalation; a human must confirm the baseline change, and you cannot confirm it yourself"
                    .to_owned(),
            ),
            Self::EgressProfilesIncomparable { .. } | Self::EgressWiderThanLease { .. } => Some(
                "request an escalation naming the exact hosts required".to_owned(),
            ),
            Self::CgroupLimitsUnavailable => {
                Some("run `clyde doctor` for the delegation remedy".to_owned())
            }
            _ => None,
        }
    }
}

/// A recorded policy decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub id: PolicyDecisionId,
    pub subject: PolicySubject,
    pub outcome: PolicyOutcome,
    pub reasons: Vec<PolicyReason>,
    pub resolved_policy_digest: Option<Digest>,
    pub suggested_alternative: Option<String>,
    pub decided_at: DateTime<Utc>,
}

/// What a resolved policy grants, carried alongside a decision for rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContext {
    pub egress: EgressProfile,
    pub credentials: CredentialPolicy,
    pub isolation: IsolationLevel,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn every_reason_renders_without_leaking_host_paths() {
        let reasons = vec![
            PolicyReason::OutOfLeaseScope {
                path: RepoPath::parse("src/x").unwrap(),
            },
            PolicyReason::TaskNotInLease {
                task: TaskType::RustCheck,
            },
            PolicyReason::BudgetExhausted {
                dimension: BudgetDimension::TaskRuns,
            },
            PolicyReason::LeaseExpired,
            PolicyReason::EgressProfilesIncomparable {
                requested: "rust-registry".to_owned(),
                ceiling: "model-api".to_owned(),
            },
            PolicyReason::AccessBaselineMissing {
                target: RepoPath::parse("crates/core").unwrap(),
            },
            PolicyReason::PhaseGated {
                capability: "rust.check".to_owned(),
            },
            PolicyReason::IsolationUnavailable {
                required: IsolationLevel::MicroVm,
            },
            PolicyReason::UnsupportedInMvp {
                what: "private registry credentials".to_owned(),
            },
        ];
        for reason in reasons {
            let rendered = reason.render();
            assert!(!rendered.is_empty());
            assert!(
                !rendered.contains("/home/") && !rendered.contains("/var/lib/clyde"),
                "reason leaked a host path: {rendered}"
            );
        }
    }

    #[test]
    fn denials_that_have_a_next_step_offer_one() {
        let with_alternative = [
            PolicyReason::TaskNotInLease {
                task: TaskType::RustCheck,
            },
            PolicyReason::LeaseExpired,
            PolicyReason::AccessBaselineDrift {
                rendered: "path outside baseline".to_owned(),
            },
        ];
        for reason in with_alternative {
            assert!(
                reason.suggested_alternative().is_some(),
                "{reason:?} must offer a next step"
            );
        }
    }

    #[test]
    fn drift_alternative_never_suggests_self_approval() {
        let reason = PolicyReason::AccessBaselineDrift {
            rendered: "x".to_owned(),
        };
        let alternative = reason.suggested_alternative().unwrap();
        assert!(alternative.contains("cannot confirm it yourself"));
    }

    #[test]
    fn outcome_permits_only_when_plain_allowed() {
        assert!(PolicyOutcome::Allowed.permits_immediately());
        assert!(!PolicyOutcome::AllowedWithApproval.permits_immediately());
        assert!(!PolicyOutcome::Denied.permits_immediately());
    }
}
