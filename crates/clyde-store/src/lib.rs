//! `clyde-store`: persistence for Clyde.
//!
//! The crate exposes one trait, [`Store`], with two implementations:
//! [`MemoryStore`] for tests of logic above the store, and [`SqliteStore`] for
//! the daemon.
//!
//! Two design choices are worth stating, because they are load-bearing:
//!
//! - **Cross-entity transitions are single methods.** There is no general
//!   transaction handle. Mission closeout, lease revocation fan-out, and budget
//!   charging are each one call, so a caller cannot perform half of one and a
//!   crash cannot leave an inconsistent authority state.
//! - **The audit log is append-only and hash-chained, with a recorded head.**
//!   There is no update or delete method. The head is stored alongside the log,
//!   because truncating the tail of a bare hash chain is otherwise undetectable.

pub mod error;
pub mod memory;
pub mod sqlite;
pub mod store;
pub mod types;
pub mod types_bundle;

pub use error::{Result, StoreError};
pub use memory::MemoryStore;
pub use sqlite::SqliteStore;
pub use store::Store;
pub use types::{
    ApprovalRecord, AuditFilter, ConfigLoad, LeaseRenewal, MissionCloseout, ResolvedSession,
    TimelineDetail,
};
pub use types_bundle::BundleRecord;

#[cfg(test)]
mod tests;
