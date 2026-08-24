//! Entity types.
//!
//! Each module owns one entity family: its fields, its validation, and its state
//! machine as explicit transition functions.

pub mod actor;
pub mod approval;
pub mod artifact;
pub mod audit;
pub mod baseline;
pub mod broker;
pub mod budget;
pub mod classification;
pub mod decision;
pub mod egress;
pub mod lease;
pub mod mission;
pub mod session;
pub mod snapshot;
pub mod task;
pub mod workspace;
