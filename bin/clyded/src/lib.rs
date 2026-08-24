//! `clyded`: the Clyde control plane.
//!
//! The daemon owns the authority model: it issues leases, binds sessions, admits
//! tasks, hosts the agent, and is the only component that talks to the broker.
//!
//! It is a library as well as a binary so the integration tests can drive it
//! in-process with an injected sandbox backend, rather than only through a
//! socket.

pub mod access;
pub mod actor_api;
pub mod admin_api;
pub mod agent;
pub mod approvals;
pub mod artifacts;
pub mod audit;
pub mod broker_gateway;
pub mod config_load;
pub mod daemon;
pub mod error;
pub mod missions;
pub mod paths;
pub mod publish;
pub mod review;
pub mod sandboxes;
pub mod server;
pub mod subagents;
pub mod tasks;

pub use daemon::{Daemon, DaemonOptions};
pub use error::{DaemonError, Result};
