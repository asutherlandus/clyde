//! `clyde-snapshot`: immutable inputs for untrusted execution.
pub mod baseline;
pub mod builder;
pub mod bundle;
pub mod cache;
pub mod cargo;
pub mod content;
pub mod error;
pub mod learn;

pub use builder::{BuiltSnapshot, SnapshotRequest, build};
pub use content::ContentStore;
pub use error::{Result, SnapshotError};

pub use baseline::{propose_from_closure, propose_from_observation};
pub use bundle::BundleStore;
pub use cache::MissionCache;
pub use cargo::classify::{Classification, ClassificationInput, ExitSummary, classify};
pub use cargo::closure::{BuildClosure, compute as compute_closure};
pub use learn::Observation;
