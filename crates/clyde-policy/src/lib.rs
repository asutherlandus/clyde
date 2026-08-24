//! `clyde-policy`: the security model as pure functions.
//!
//! This crate holds the decisions the rest of the system enforces: the built-in
//! task catalog, task policy resolution, lease derivation, egress profile
//! ordering, budget charging, snapshot exclusions, access-baseline drift, and
//! configuration layering.
//!
//! It deliberately depends on neither `clyde-store`, `clyde-sandbox`, nor
//! `tokio`, so every policy decision is a pure function over values and is
//! testable without I/O (Phase 0 deliverable 3). Where a decision needs
//! knowledge of the world — what the host can isolate, whether a baseline is
//! confirmed — that knowledge arrives as an argument.
//!
//! Four functions carry most of the security model, and they are the highest
//! value test targets in the project:
//!
//! - [`resolve::resolve_task_policy`]
//! - [`derive::derive_lease`]
//! - [`egress::egress_profile_order`]
//! - [`budget::charge_budget`]

pub mod access;
pub mod budget;
pub mod catalog;
pub mod config;
pub mod derive;
pub mod egress;
pub mod exclusions;
pub mod resolve;

pub use budget::charge_budget;
pub use catalog::builtin_policy;
pub use config::{Config, ConfigError, ConfigSource};
pub use derive::{LeaseDerivationRequest, derive_lease};
pub use egress::{egress_profile_order, is_no_wider_than};
pub use resolve::{
    Admission, AdmissionInput, ApprovalState, HostCapabilities, ResolvedPolicy,
    resolve_task_policy, validate_action,
};
