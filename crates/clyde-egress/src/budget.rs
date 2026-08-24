//! Per-task egress accounting.
//!
//! Byte and connection budgets are enforced per task and charged to the lease.
//! They are coarse by design: they bound bulk exfiltration and runaway
//! downloads, and they do not detect low-volume signalling. That limitation is
//! stated in the network egress model and should be stated in the approval UX
//! too, rather than implied away here.

use std::sync::atomic::{AtomicU64, Ordering};

use clyde_core::egress::EgressDenialReason;

/// Ceilings for one task's egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressBudget {
    pub max_bytes: u64,
    pub max_requests: u32,
    pub max_connections: u32,
}

impl EgressBudget {
    /// A budget that permits nothing, used for profile `none`.
    pub const DENY_ALL: Self = Self {
        max_bytes: 0,
        max_requests: 0,
        max_connections: 0,
    };
}

/// Live counters for one task's egress.
///
/// Atomics rather than a mutex: every counter is independent, connections are
/// concurrent, and no decision reads more than one counter at a time.
#[derive(Debug, Default)]
pub struct EgressCounters {
    bytes: AtomicU64,
    requests: AtomicU64,
    connections: AtomicU64,
}

impl EgressCounters {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    /// Reserves a connection slot, or explains the refusal.
    pub fn open_connection(&self, budget: &EgressBudget) -> Result<(), EgressDenialReason> {
        let taken = self.connections.fetch_add(1, Ordering::Relaxed);
        if taken >= u64::from(budget.max_connections) {
            // The counter is left incremented deliberately: a refused attempt
            // still consumed a slot, so a caller cannot retry indefinitely at no
            // cost.
            return Err(EgressDenialReason::ConnectionBudgetExhausted);
        }
        Ok(())
    }

    /// Reserves a request slot.
    pub fn open_request(&self, budget: &EgressBudget) -> Result<(), EgressDenialReason> {
        let taken = self.requests.fetch_add(1, Ordering::Relaxed);
        if taken >= u64::from(budget.max_requests) {
            return Err(EgressDenialReason::RequestBudgetExhausted);
        }
        Ok(())
    }

    /// Adds transferred bytes and reports whether the budget is now spent.
    pub fn add_bytes(&self, count: u64, budget: &EgressBudget) -> Result<(), EgressDenialReason> {
        let total = self
            .bytes
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count);
        if total > budget.max_bytes {
            return Err(EgressDenialReason::ByteBudgetExhausted);
        }
        Ok(())
    }

    /// The remaining byte headroom, for reporting.
    pub fn remaining_bytes(&self, budget: &EgressBudget) -> u64 {
        budget.max_bytes.saturating_sub(self.bytes())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    fn budget() -> EgressBudget {
        EgressBudget {
            max_bytes: 1000,
            max_requests: 2,
            max_connections: 2,
        }
    }

    #[test]
    fn connections_and_requests_are_capped() {
        let counters = EgressCounters::default();
        assert!(counters.open_connection(&budget()).is_ok());
        assert!(counters.open_connection(&budget()).is_ok());
        assert_eq!(
            counters.open_connection(&budget()),
            Err(EgressDenialReason::ConnectionBudgetExhausted)
        );
        assert!(counters.open_request(&budget()).is_ok());
        assert!(counters.open_request(&budget()).is_ok());
        assert_eq!(
            counters.open_request(&budget()),
            Err(EgressDenialReason::RequestBudgetExhausted)
        );
    }

    #[test]
    fn a_refused_attempt_still_consumes_its_slot() {
        let counters = EgressCounters::default();
        for _ in 0..3 {
            let _ = counters.open_connection(&budget());
        }
        assert_eq!(
            counters.connections(),
            3,
            "otherwise a caller could retry indefinitely at no cost"
        );
    }

    #[test]
    fn bytes_accumulate_and_the_budget_fires_once_exceeded() {
        let counters = EgressCounters::default();
        assert!(counters.add_bytes(600, &budget()).is_ok());
        assert_eq!(counters.remaining_bytes(&budget()), 400);
        assert_eq!(
            counters.add_bytes(600, &budget()),
            Err(EgressDenialReason::ByteBudgetExhausted)
        );
        assert_eq!(counters.remaining_bytes(&budget()), 0);
    }

    #[test]
    fn a_deny_all_budget_refuses_the_first_connection() {
        let counters = EgressCounters::default();
        assert!(counters.open_connection(&EgressBudget::DENY_ALL).is_err());
    }

    #[test]
    fn byte_counting_saturates() {
        let counters = EgressCounters::default();
        let generous = EgressBudget {
            max_bytes: u64::MAX,
            ..budget()
        };
        assert!(counters.add_bytes(u64::MAX, &generous).is_ok());
        assert!(counters.add_bytes(10, &generous).is_ok());
    }
}
