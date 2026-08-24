//! Leases: where authority actually lives.
//!
//! An actor holds no authority of its own; every check resolves to an active
//! lease. Derivation is a pure function over two leases and is the highest-value
//! unit-test target in the project (schema reference: derivation rules).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::budget::{Budget, BudgetUsage};
use crate::entities::classification::{CredentialPolicy, EgressProfile};
use crate::entities::mission::MissionScope;
use crate::entities::task::TaskType;
use crate::error::{TransitionError, ValidationError};
use crate::ids::{ActorId, LeaseId, MissionId};

/// What an actor holding this lease may do.
///
/// Flags are monotone under derivation: a child may drop a flag, never raise
/// one (derivation rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityFlags {
    pub may_edit: bool,
    pub may_request_tasks: bool,
    pub may_spawn_subagents: bool,
    pub may_request_publish: bool,
}

impl AuthorityFlags {
    pub const NONE: Self = Self {
        may_edit: false,
        may_request_tasks: false,
        may_spawn_subagents: false,
        may_request_publish: false,
    };

    /// Whether no flag is set here that is unset in `parent`.
    pub fn is_no_wider_than(self, parent: Self) -> Option<&'static str> {
        [
            (self.may_edit && !parent.may_edit, "may_edit"),
            (
                self.may_request_tasks && !parent.may_request_tasks,
                "may_request_tasks",
            ),
            (
                self.may_spawn_subagents && !parent.may_spawn_subagents,
                "may_spawn_subagents",
            ),
            (
                self.may_request_publish && !parent.may_request_publish,
                "may_request_publish",
            ),
        ]
        .into_iter()
        .find_map(|(widened, name)| widened.then_some(name))
    }
}

/// Lease state (schema reference: lease states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Issued,
    Active,
    /// Budget spent in some dimension. Blocks new work; in-flight work is not
    /// terminated (schema reference: Budget invariants).
    Exhausted,
    Expired,
    Revoked,
    /// A renewal issued a replacement lease (Phase 1 deliverable 3).
    Superseded,
}

impl std::fmt::Display for LeaseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Issued => "issued",
            Self::Active => "active",
            Self::Exhausted => "exhausted",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::Superseded => "superseded",
        };
        f.write_str(text)
    }
}

impl LeaseState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Expired | Self::Revoked | Self::Superseded)
    }

    /// Whether new work may be admitted under a lease in this state.
    pub fn permits_new_work(self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn transition(self, to: LeaseState) -> Result<LeaseState, TransitionError> {
        if self.is_terminal() {
            return Err(TransitionError::terminal("lease", self, to));
        }
        let permitted = match (self, to) {
            (Self::Issued, Self::Active | Self::Revoked | Self::Expired | Self::Superseded) => true,
            (Self::Active, Self::Exhausted | Self::Expired | Self::Revoked | Self::Superseded) => {
                true
            }
            // An exhausted lease can still expire or be revoked; those are
            // stronger states and revocation must always be reachable.
            (Self::Exhausted, Self::Expired | Self::Revoked | Self::Superseded) => true,
            _ => false,
        };
        if permitted {
            Ok(to)
        } else {
            Err(TransitionError::not_permitted("lease", self, to))
        }
    }
}

const MAX_PURPOSE: usize = 1024;

/// A lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: LeaseId,
    pub mission: MissionId,
    pub parent: Option<LeaseId>,
    pub actor: ActorId,
    /// Always Clyde: leases are issued by the control plane, never by an actor.
    pub issued_by: ActorId,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub repo_scope: MissionScope,
    pub task_scope: BTreeSet<TaskType>,
    pub network_scope: EgressProfile,
    pub credential_scope: CredentialPolicy,
    pub authority: AuthorityFlags,
    pub budget: Budget,
    pub usage: BudgetUsage,
    pub state: LeaseState,
    pub purpose: String,
}

impl Lease {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.purpose.trim().is_empty() {
            return Err(ValidationError::EmptyField { field: "purpose" });
        }
        if self.purpose.len() > MAX_PURPOSE {
            return Err(ValidationError::FieldTooLong {
                field: "purpose",
                max: MAX_PURPOSE,
            });
        }
        if self.expires_at <= self.issued_at {
            return Err(ValidationError::ExpiryNotAfterIssue {
                issued_at: self.issued_at.to_rfc3339(),
                expires_at: self.expires_at.to_rfc3339(),
            });
        }
        self.repo_scope.validate()?;
        self.budget.validate()?;
        Ok(())
    }

    /// Whether the lease may admit new work as of `now`.
    ///
    /// Expiry is evaluated against the clock rather than trusting the stored
    /// state, because a lease that expired while the daemon was not looking must
    /// not admit work on the strength of a stale `Active`.
    pub fn permits_new_work_at(&self, now: DateTime<Utc>) -> bool {
        self.state.permits_new_work() && now < self.expires_at
    }

    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    pub fn allows_task(&self, task: TaskType) -> bool {
        self.task_scope.contains(&task)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::duration::HumanDuration;
    use crate::ids;
    use crate::repo_path::RepoPath;

    pub(crate) fn lease() -> Lease {
        let issued = Utc::now();
        Lease {
            id: ids::new::lease_id().unwrap(),
            mission: ids::new::mission_id().unwrap(),
            parent: None,
            actor: ActorId::parse("agent:claude").unwrap(),
            issued_by: ActorId::parse("human:clyde").unwrap(),
            issued_at: issued,
            expires_at: issued + chrono::Duration::hours(1),
            repo_scope: MissionScope {
                edit_paths: [RepoPath::parse("src").unwrap()].into_iter().collect(),
                read_paths: BTreeSet::new(),
            },
            task_scope: [TaskType::RustCheck].into_iter().collect(),
            network_scope: EgressProfile::None,
            credential_scope: CredentialPolicy::None,
            authority: AuthorityFlags {
                may_edit: true,
                may_request_tasks: true,
                may_spawn_subagents: true,
                may_request_publish: false,
            },
            budget: Budget {
                max_duration: HumanDuration::parse("1h").unwrap(),
                max_task_runs: 10,
                max_parallel_subagents: 1,
                max_subagents: 1,
                max_cpu_seconds: 600,
                max_cache_bytes: 1 << 30,
                max_artifact_bytes: 1 << 28,
                max_egress_bytes: 0,
                max_egress_requests: 0,
            },
            usage: BudgetUsage::default(),
            state: LeaseState::Active,
            purpose: "primary lease".to_owned(),
        }
    }

    #[test]
    fn authority_flags_may_only_narrow() {
        let parent = AuthorityFlags {
            may_edit: true,
            may_request_tasks: true,
            may_spawn_subagents: false,
            may_request_publish: false,
        };
        assert_eq!(AuthorityFlags::NONE.is_no_wider_than(parent), None);
        assert_eq!(parent.is_no_wider_than(parent), None);
        let widened = AuthorityFlags {
            may_spawn_subagents: true,
            ..parent
        };
        assert_eq!(
            widened.is_no_wider_than(parent),
            Some("may_spawn_subagents")
        );
        let publish = AuthorityFlags {
            may_request_publish: true,
            ..parent
        };
        assert_eq!(
            publish.is_no_wider_than(parent),
            Some("may_request_publish")
        );
    }

    #[test]
    fn lease_state_machine() {
        use LeaseState::*;
        assert_eq!(Issued.transition(Active).unwrap(), Active);
        assert_eq!(Active.transition(Exhausted).unwrap(), Exhausted);
        assert_eq!(Exhausted.transition(Revoked).unwrap(), Revoked);
        assert_eq!(Active.transition(Superseded).unwrap(), Superseded);
        for terminal in [Expired, Revoked, Superseded] {
            assert!(terminal.transition(Active).is_err());
        }
        assert!(
            Exhausted.transition(Active).is_err(),
            "budget does not refill"
        );
    }

    #[test]
    fn only_active_admits_new_work() {
        use LeaseState::*;
        assert!(Active.permits_new_work());
        for state in [Issued, Exhausted, Expired, Revoked, Superseded] {
            assert!(!state.permits_new_work(), "{state} must not admit work");
        }
    }

    #[test]
    fn expiry_is_evaluated_against_the_clock_not_the_stored_state() {
        let mut lease = lease();
        let now = lease.issued_at + chrono::Duration::hours(2);
        assert_eq!(lease.state, LeaseState::Active);
        assert!(
            !lease.permits_new_work_at(now),
            "a stale Active must not admit work past expiry"
        );
        assert!(lease.is_expired_at(now));
        lease.expires_at = now + chrono::Duration::minutes(1);
        assert!(lease.permits_new_work_at(now));
    }

    #[test]
    fn validation_rejects_bad_expiry_and_empty_purpose() {
        let mut l = lease();
        assert!(l.validate().is_ok());
        l.expires_at = l.issued_at;
        assert!(l.validate().is_err());
        let mut l = lease();
        l.purpose = "  ".to_owned();
        assert!(l.validate().is_err());
    }

    #[test]
    fn task_scope_membership_is_exact() {
        let l = lease();
        assert!(l.allows_task(TaskType::RustCheck));
        assert!(!l.allows_task(TaskType::GitPush));
    }
}
