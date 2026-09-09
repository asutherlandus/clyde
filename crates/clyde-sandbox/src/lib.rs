//! `clyde-sandbox`: the isolation boundary.
//!
//! The crate is built around one trait, [`SandboxBackend`], and one
//! backend-independent value, [`SandboxSpec`]. Anything a backend cannot honour
//! is a `preflight` failure, never a silent relaxation — that rule is what keeps
//! the trait from becoming the place where boundaries quietly weaken.
//!
//! Two backends implement it: [`BubblewrapBackend`] (D5) and
//! [`FirecrackerBackend`] (D9). Selection is by the policy's minimum isolation
//! level, with no manual override and no silent downgrade.
//!
//! Where a mechanism could be built either as a value or as an inline side
//! effect, it is built as a value: command construction, limit wrapping, seccomp
//! filters, and VM configuration are all pure functions over a spec, so what
//! Clyde actually runs is testable and auditable rather than assembled at spawn
//! time.

pub mod backend;
pub mod bubblewrap;
pub mod capability;
pub mod error;
pub mod firecracker;
pub mod guest_channel;
pub mod images;
pub mod limits;
pub mod registry;
pub mod runtime_root;
pub mod seccomp;
pub mod spec;
#[cfg(feature = "test-backend")]
pub mod test_backend;

pub use backend::{ExitStatus, SandboxBackend, SandboxHandle};
pub use bubblewrap::BubblewrapBackend;
pub use capability::{
    Capability, CgroupObservation, CpuVirtualisation, Enclosure, HostReport, KvmObservation,
    ProbePaths, probe,
};
pub use error::{Result, SandboxError};
pub use firecracker::{FirecrackerBackend, FirecrackerConfig};
pub use limits::LimitTools;
pub use registry::BackendRegistry;
pub use runtime_root::{RuntimeRoot, RuntimeRoots, assert_workspace_root};
pub use seccomp::{FilterArch, SeccompFilter};
pub use spec::{Mount, MountMode, MountPurpose, SandboxSpec, ScratchPolicy};
