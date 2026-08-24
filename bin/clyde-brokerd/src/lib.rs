//! `clyde-brokerd`: the credential broker.
//!
//! The broker exists so that code execution and authority live in different
//! processes. It holds the developer's existing credential in memory, never
//! writes it anywhere, never logs it, and exposes a capability-oriented
//! interface: `git_push` exists, `get_ssh_key` does not and must not (D8).
//!
//! It validates every request **independently**. It does not trust clyded's word
//! that an approval exists — it reads the approval record itself.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use clyde_broker_api::{
    BrokerRequest, BrokerResponse, Capabilities, GitPushRequest, PushOutcome, RefusalReason,
};
use clyde_core::broker::BrokerOpState;
use clyde_git::GitRunner;
use clyde_git::push::{Credential, PushRequest};
use clyde_policy::config::Config;
use clyde_store::Store;

pub mod service;

/// Everything the broker holds.
///
/// The credential is a field of this struct and of nothing else. It is never
/// serialised, and `Debug` shows its kind rather than its content.
pub struct Broker {
    pub config: Config,
    pub credential: Credential,
    pub git: GitRunner,
    /// Read-only view of Clyde's state, used to verify approvals.
    pub store: Arc<dyn Store>,
    pub scratch: PathBuf,
}

impl std::fmt::Debug for Broker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broker")
            .field("credential", &self.credential)
            .field("scratch", &self.scratch)
            .finish()
    }
}

impl Broker {
    /// Answers a capability query.
    ///
    /// Reports what it can do and whether it holds a credential, by kind. Never
    /// what the credential is.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            operations: vec!["git_push".to_owned()],
            holds_credential: self.credential.is_present(),
            credential_kind: self.credential.kind().map(str::to_owned),
            remote_allowlist: self.config.broker.remotes.keys().cloned().collect(),
            branch_allowlist: self.config.push.branch_patterns.iter().cloned().collect(),
            protected_branches: self
                .config
                .push
                .protected_branch_patterns
                .iter()
                .cloned()
                .collect(),
        }
    }

    /// Validates and performs a push.
    pub async fn git_push(&self, request: &GitPushRequest) -> BrokerResponse {
        match self.validate(request) {
            Err(reason) => BrokerResponse::Refused { reason },
            Ok(remote_url) => self.perform(request, remote_url).await,
        }
    }

    /// Every check the broker makes on its own account.
    ///
    /// Ordered so the most fundamental refusal is the one reported.
    fn validate(&self, request: &GitPushRequest) -> Result<String, RefusalReason> {
        if !self.credential.is_present() {
            return Err(RefusalReason::NoCredential);
        }
        // The digest must describe the request that was actually made, before
        // anything is compared against a stored approval.
        if !request.digest_matches() {
            return Err(RefusalReason::DigestMismatch);
        }
        let branch = clyde_git::validate_branch_refspec(&request.refspec).map_err(|error| {
            RefusalReason::MalformedRequest {
                detail: error.to_string(),
            }
        })?;
        clyde_core::task::validate_git_object_id(&request.commit).map_err(|error| {
            RefusalReason::MalformedRequest {
                detail: error.to_string(),
            }
        })?;

        // Protected branches are refused outright, before the allowlist is even
        // consulted: an allowlist entry must not be able to admit `main`.
        if crate::service::matches_pattern(&self.config.push.protected_branch_patterns, &branch) {
            return Err(RefusalReason::ProtectedBranch { branch });
        }
        if !crate::service::matches_pattern(&self.config.push.branch_patterns, &branch) {
            return Err(RefusalReason::BranchNotAllowlisted { branch });
        }
        let Some(remote_url) = self.config.broker.remotes.get(&request.remote) else {
            return Err(RefusalReason::RemoteNotAllowlisted {
                remote: request.remote.clone(),
            });
        };

        // The approval is read from the store, not taken from the request.
        let record = self
            .store
            .get_approval(&request.approval)
            .map_err(|error| RefusalReason::ApprovalInvalid {
                detail: error.to_string(),
            })?;
        let Some(decision) = record.decision.as_ref() else {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the approval has no decision".to_owned(),
            });
        };
        if !decision.decision.is_approval() {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the approval was denied".to_owned(),
            });
        }
        if !decision.decided_by.is_human() {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the decision was not made by a human".to_owned(),
            });
        }
        if record.request.is_expired_at(Utc::now()) {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the approval has expired".to_owned(),
            });
        }
        if !record
            .request
            .request_digest
            .matches(&request.request_digest)
        {
            return Err(RefusalReason::DigestMismatch);
        }
        if record.request.mission != request.mission || record.request.lease != request.lease {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the approval belongs to a different mission or lease".to_owned(),
            });
        }

        // Replay protection. clyded marks the operation `executing` immediately
        // before calling, and a terminal operation cannot re-enter that state,
        // so a second call for the same operation is refused. This is what makes
        // "consumed" checkable from here: single-use consumption is the control
        // plane's bookkeeping, and the operation state is the fact the broker
        // can verify for itself.
        let operations = self
            .store
            .list_broker_ops(&request.mission)
            .map_err(|error| RefusalReason::ApprovalInvalid {
                detail: error.to_string(),
            })?;
        let Some(operation) = operations
            .into_iter()
            .find(|operation| operation.id == request.operation)
        else {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "no brokered operation matches this request".to_owned(),
            });
        };
        match operation.state {
            BrokerOpState::Executing => {}
            BrokerOpState::Frozen => return Err(RefusalReason::Frozen),
            other => {
                return Err(RefusalReason::ApprovalInvalid {
                    detail: format!(
                        "the operation is {other}, so this is a replay rather than an execution"
                    ),
                });
            }
        }
        if operation.approval != request.approval {
            return Err(RefusalReason::ApprovalInvalid {
                detail: "the operation was approved under a different approval".to_owned(),
            });
        }

        Ok(remote_url.clone())
    }

    async fn perform(&self, request: &GitPushRequest, remote_url: String) -> BrokerResponse {
        let push_request = PushRequest {
            workspace: request.workspace.clone(),
            remote_url,
            refspec: request.refspec.clone(),
            commit: request.commit.clone(),
            expected_tree: request.expected_tree.clone(),
        };
        match clyde_git::push::push(&self.git, &self.scratch, &push_request, &self.credential).await
        {
            Ok(result) => BrokerResponse::Pushed(PushOutcome {
                operation: request.operation.clone(),
                commit: result.commit,
                refspec: result.refspec,
                summary: result.summary,
            }),
            Err(error) => BrokerResponse::Refused {
                reason: match error {
                    clyde_git::GitError::Invalid { kind, value } => {
                        RefusalReason::ContentMismatch {
                            detail: format!("{kind}: {value}"),
                        }
                    }
                    other => RefusalReason::GitFailed {
                        detail: other.to_string(),
                    },
                },
            },
        }
    }

    /// Dispatches one request.
    pub async fn handle(&self, request: BrokerRequest) -> BrokerResponse {
        match request {
            BrokerRequest::Capabilities => BrokerResponse::Capabilities(self.capabilities()),
            BrokerRequest::GitPush(push) => self.git_push(&push).await,
        }
    }
}

/// Resolves the credential from configuration and the environment.
///
/// The developer's existing credential, with its scope unchanged: Clyde's
/// contribution is eliminating exposure paths and adding approval and audit, not
/// reducing what the credential can do (D8).
pub fn resolve_credential(config: &Config) -> Credential {
    if let Some(path) = config.broker.ssh_key.as_ref()
        && path.is_file()
    {
        return Credential::SshKey { path: path.clone() };
    }
    if config.broker.allow_ssh_agent
        && let Some(socket) = std::env::var_os("SSH_AUTH_SOCK")
    {
        let socket = PathBuf::from(socket);
        if socket.exists() {
            return Credential::SshAgent { socket };
        }
    }
    Credential::None
}
