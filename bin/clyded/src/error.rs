//! Daemon errors.
//!
//! Everything that crosses the socket boundary becomes a JSON-RPC error, and the
//! mapping lives here so that a denial always arrives as a denial rather than as
//! an internal error a caller cannot act on.

use clyde_api::jsonrpc::Error as RpcError;
use clyde_api::views::DenialView;
use clyde_core::decision::PolicyReason;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    Store(#[from] clyde_store::StoreError),

    #[error("{0}")]
    Sandbox(#[from] clyde_sandbox::SandboxError),

    #[error("{0}")]
    Snapshot(#[from] clyde_snapshot::SnapshotError),

    #[error("{0}")]
    Egress(#[from] clyde_egress::EgressError),

    #[error("{0}")]
    Git(#[from] clyde_git::GitError),

    #[error("{0}")]
    Config(#[from] clyde_policy::config::ConfigError),

    #[error("{0}")]
    Validation(#[from] clyde_core::ValidationError),

    #[error("{0}")]
    Transition(#[from] clyde_core::TransitionError),

    /// The request was refused by policy. Carries the structured reasons so the
    /// caller gets what was denied, which constraint denied it, and what to do
    /// instead.
    #[error("{}", .0.first().map(clyde_core::decision::PolicyReason::render).unwrap_or_else(|| "denied by policy".to_owned()))]
    Denied(Vec<PolicyReason>),

    /// A human must approve before this can proceed.
    #[error("{0}")]
    ApprovalRequired(String),

    #[error("{0}")]
    Invalid(String),

    #[error("{0}")]
    NotFound(String),

    #[error("input/output error: {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Internal(String),
}

impl DaemonError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// The denial's structured reasons, if it is one.
    pub fn reasons(&self) -> &[PolicyReason] {
        match self {
            Self::Denied(reasons) => reasons,
            _ => &[],
        }
    }

    /// Converts to a JSON-RPC error, preserving the structure a caller needs.
    pub fn to_rpc(&self, subject: &str) -> RpcError {
        match self {
            Self::Denied(reasons) => {
                let view = DenialView::new(subject, reasons);
                RpcError::policy_denied(self.to_string())
                    .with_data(serde_json::to_value(&view).unwrap_or(serde_json::Value::Null))
            }
            Self::ApprovalRequired(message) => RpcError::approval_required(message.clone()),
            Self::Invalid(message) => RpcError::invalid_params(message.clone()),
            Self::NotFound(message) => RpcError::invalid_params(message.clone()),
            // Everything else is a Clyde-side failure, and is reported as one
            // rather than as a problem with the caller's request.
            other => RpcError::internal(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, DaemonError>;

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;
    use clyde_core::task::TaskType;

    #[test]
    fn a_denial_carries_its_reasons_and_alternatives_across_the_wire() {
        let error = DaemonError::Denied(vec![PolicyReason::TaskNotInLease {
            task: TaskType::RustCheck,
        }]);
        let rpc = error.to_rpc("rust.check");
        assert_eq!(rpc.code, clyde_api::jsonrpc::codes::POLICY_DENIED);
        let data = rpc.data.expect("structured data");
        assert_eq!(data["denied"], "rust.check");
        assert!(!data["alternatives"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_internal_failure_is_not_reported_as_a_bad_request() {
        let error = DaemonError::internal("the snapshot store is unavailable");
        assert_eq!(
            error.to_rpc("rust.check").code,
            clyde_api::jsonrpc::codes::INTERNAL_ERROR,
            "a Clyde-side failure must not look like the caller's fault"
        );
    }

    #[test]
    fn an_approval_requirement_has_its_own_code() {
        let error = DaemonError::ApprovalRequired("a human must approve git.push".to_owned());
        assert_eq!(
            error.to_rpc("git.push").code,
            clyde_api::jsonrpc::codes::APPROVAL_REQUIRED
        );
    }

    #[test]
    fn a_denial_message_names_the_first_reason() {
        let error = DaemonError::Denied(vec![PolicyReason::LeaseExpired]);
        assert!(error.to_string().contains("expired"));
        assert_eq!(error.reasons().len(), 1);
    }
}
