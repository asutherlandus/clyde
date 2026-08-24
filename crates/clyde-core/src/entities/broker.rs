//! Brokered operations (Phase 4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{TransitionError, ValidationError};
use crate::ids::{ApprovalId, BrokerOpId, LeaseId, MissionId};

/// The brokered operations the MVP supports.
///
/// The interface is capability-oriented, never secret-oriented: `GitPush`
/// exists, `GetSshKey` does not and must not (technology choices: broker API
/// style).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrokeredKind {
    GitPush {
        remote: String,
        refspec: String,
        commit: String,
    },
}

impl BrokeredKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GitPush { .. } => "git_push",
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::GitPush {
                remote,
                refspec,
                commit,
            } => {
                if remote.trim().is_empty() {
                    return Err(ValidationError::EmptyField { field: "remote" });
                }
                if refspec.trim().is_empty() {
                    return Err(ValidationError::EmptyField { field: "refspec" });
                }
                crate::entities::task::validate_git_object_id(commit)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOpState {
    Requested,
    Approved,
    Executing,
    Succeeded,
    Failed,
    /// Mission revocation freezes in-flight operations rather than cancelling
    /// them silently, so the record shows what was in progress.
    Frozen,
}

impl std::fmt::Display for BrokerOpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Requested => "requested",
            Self::Approved => "approved",
            Self::Executing => "executing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Frozen => "frozen",
        };
        f.write_str(text)
    }
}

impl BrokerOpState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Frozen)
    }

    pub fn transition(self, to: BrokerOpState) -> Result<BrokerOpState, TransitionError> {
        if self.is_terminal() {
            return Err(TransitionError::terminal("brokered operation", self, to));
        }
        let permitted = match (self, to) {
            (Self::Requested, Self::Approved | Self::Failed | Self::Frozen) => true,
            (Self::Approved, Self::Executing | Self::Failed | Self::Frozen) => true,
            (Self::Executing, Self::Succeeded | Self::Failed | Self::Frozen) => true,
            _ => false,
        };
        if permitted {
            Ok(to)
        } else {
            Err(TransitionError::not_permitted(
                "brokered operation",
                self,
                to,
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredOperation {
    pub id: BrokerOpId,
    pub mission: MissionId,
    pub lease: LeaseId,
    /// The approval this operation executes under. Not optional: no brokered
    /// operation exists without one.
    pub approval: ApprovalId,
    pub kind: BrokeredKind,
    pub state: BrokerOpState,
    pub result_summary: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
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
    fn broker_op_state_machine() {
        use BrokerOpState::*;
        assert_eq!(Requested.transition(Approved).unwrap(), Approved);
        assert_eq!(Approved.transition(Executing).unwrap(), Executing);
        assert_eq!(Executing.transition(Succeeded).unwrap(), Succeeded);
        // Revocation freezes at every non-terminal point.
        assert_eq!(Requested.transition(Frozen).unwrap(), Frozen);
        assert_eq!(Approved.transition(Frozen).unwrap(), Frozen);
        assert_eq!(Executing.transition(Frozen).unwrap(), Frozen);
        // Executing without an approval step must not be reachable.
        assert!(Requested.transition(Executing).is_err());
        assert!(Succeeded.transition(Executing).is_err());
        assert!(Frozen.transition(Executing).is_err());
    }

    #[test]
    fn push_requests_validate_their_object_id() {
        let good = BrokeredKind::GitPush {
            remote: "origin".to_owned(),
            refspec: "refs/heads/feature".to_owned(),
            commit: "a".repeat(40),
        };
        assert!(good.validate().is_ok());
        let bad = BrokeredKind::GitPush {
            remote: "origin".to_owned(),
            refspec: "refs/heads/feature".to_owned(),
            commit: "HEAD".to_owned(),
        };
        assert!(bad.validate().is_err());
        let empty_remote = BrokeredKind::GitPush {
            remote: " ".to_owned(),
            refspec: "refs/heads/feature".to_owned(),
            commit: "a".repeat(40),
        };
        assert!(empty_remote.validate().is_err());
    }
}
