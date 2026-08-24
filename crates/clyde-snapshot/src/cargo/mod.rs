//! Cargo-specific knowledge: build closures, lockfiles, the code-execution
//! inventory, and failure classification.
//!
//! Kept in one place so that "what Clyde knows about cargo" is reviewable, and
//! so a change in cargo's behaviour has one blast radius.

pub mod classify;
pub mod closure;
pub mod inventory;
pub mod lockfile;
