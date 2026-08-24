//! `clyde-egress`: the one mechanism by which anything sandboxed reaches the
//! network.
//!
//! Every sandbox, for every profile including `none`, gets an unshared network
//! namespace with loopback only. Reachability comes from a bind-mounted Unix
//! socket and a trusted in-sandbox forwarder, never from the network namespace,
//! so the allowlist decision is made host-side in trusted code and compromising
//! the forwarder gains nothing.
//!
//! Profile `none` is implemented by not binding the socket. There is no runtime
//! flag that disables egress — the absence of the socket *is* the absence of
//! egress, which is the property worth having.

pub mod budget;
pub mod ca;
pub mod error;
pub mod forwarder;
pub mod http;
pub mod proxy;

pub use budget::{EgressBudget, EgressCounters};
pub use ca::ClydeCa;
pub use error::{EgressError, Result};
pub use proxy::{EgressContext, EgressRecorder, MemoryRecorder, ProxyHandle, bind};
