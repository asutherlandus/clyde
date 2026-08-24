//! Egress errors.

use clyde_core::classification::HostName;

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("input/output error in the egress layer: {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("certificate authority problem: {0}")]
    Ca(String),

    #[error("TLS problem: {0}")]
    Tls(String),

    #[error("the proxy could not parse a request: {0}")]
    Protocol(#[from] crate::http::HttpError),

    #[error("{host} could not be resolved")]
    Resolve { host: HostName },

    #[error("the proxy socket path {path:?} is already in use by another process")]
    SocketInUse { path: std::path::PathBuf },

    #[error("egress is not permitted for this task, so no proxy socket exists")]
    NoEgress,
}

impl EgressError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, EgressError>;
