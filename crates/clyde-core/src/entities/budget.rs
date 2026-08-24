//! Budgets.
//!
//! Budget consumption is recorded before a task starts, not after it completes,
//! so a crashed daemon cannot lose the charge (schema reference: Budget).

use serde::{Deserialize, Serialize};

use crate::duration::HumanDuration;
use crate::error::ValidationError;

/// The dimensions a budget bounds. Named so that exhaustion can be reported as
/// a structured reason rather than prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    Duration,
    TaskRuns,
    ParallelSubagents,
    Subagents,
    CpuSeconds,
    CacheBytes,
    ArtifactBytes,
    EgressBytes,
    EgressRequests,
}

impl BudgetDimension {
    pub fn name(self) -> &'static str {
        match self {
            Self::Duration => "duration",
            Self::TaskRuns => "task_runs",
            Self::ParallelSubagents => "parallel_subagents",
            Self::Subagents => "subagents",
            Self::CpuSeconds => "cpu_seconds",
            Self::CacheBytes => "cache_bytes",
            Self::ArtifactBytes => "artifact_bytes",
            Self::EgressBytes => "egress_bytes",
            Self::EgressRequests => "egress_requests",
        }
    }
}

impl std::fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A budget ceiling.
///
/// Egress dimensions are part of the budget because the workspace environment
/// can spend the model API credential without holding it (D11), so request and
/// byte counts are the cost control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub max_duration: HumanDuration,
    pub max_task_runs: u32,
    pub max_parallel_subagents: u8,
    pub max_subagents: u8,
    pub max_cpu_seconds: u64,
    pub max_cache_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_egress_bytes: u64,
    pub max_egress_requests: u32,
}

/// Consumption against a [`Budget`], in the same dimensions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub duration_seconds: u64,
    pub task_runs: u32,
    pub parallel_subagents: u8,
    pub subagents: u8,
    pub cpu_seconds: u64,
    pub cache_bytes: u64,
    pub artifact_bytes: u64,
    pub egress_bytes: u64,
    pub egress_requests: u32,
}

/// An increment to charge against a budget.
///
/// Kept separate from [`BudgetUsage`] so a caller cannot accidentally pass a
/// cumulative total where a delta is expected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BudgetCost {
    pub duration_seconds: u64,
    pub task_runs: u32,
    pub parallel_subagents: u8,
    pub subagents: u8,
    pub cpu_seconds: u64,
    pub cache_bytes: u64,
    pub artifact_bytes: u64,
    pub egress_bytes: u64,
    pub egress_requests: u32,
}

impl BudgetCost {
    /// One task run, the common admission charge.
    pub fn one_task_run() -> Self {
        Self {
            task_runs: 1,
            ..Self::default()
        }
    }

    /// One sub-agent session, holding a parallel slot.
    pub fn one_subagent() -> Self {
        Self {
            subagents: 1,
            parallel_subagents: 1,
            ..Self::default()
        }
    }

    pub fn egress(bytes: u64, requests: u32) -> Self {
        Self {
            egress_bytes: bytes,
            egress_requests: requests,
            ..Self::default()
        }
    }
}

impl Budget {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.max_task_runs == 0 {
            return Err(ValidationError::ZeroValue {
                field: "max_task_runs",
            });
        }
        Ok(())
    }

    /// Element-wise minimum: a derived budget is within the parent's ceiling
    /// (derivation rule 8), and configuration may only narrow (D14).
    pub fn narrowed_to(self, other: Self) -> Self {
        Self {
            max_duration: self.max_duration.min(other.max_duration),
            max_task_runs: self.max_task_runs.min(other.max_task_runs),
            max_parallel_subagents: self
                .max_parallel_subagents
                .min(other.max_parallel_subagents),
            max_subagents: self.max_subagents.min(other.max_subagents),
            max_cpu_seconds: self.max_cpu_seconds.min(other.max_cpu_seconds),
            max_cache_bytes: self.max_cache_bytes.min(other.max_cache_bytes),
            max_artifact_bytes: self.max_artifact_bytes.min(other.max_artifact_bytes),
            max_egress_bytes: self.max_egress_bytes.min(other.max_egress_bytes),
            max_egress_requests: self.max_egress_requests.min(other.max_egress_requests),
        }
    }

    /// Whether every dimension of `self` is within `other`.
    pub fn is_within(&self, other: &Budget) -> Option<BudgetDimension> {
        let checks: [(bool, BudgetDimension); 9] = [
            (
                self.max_duration <= other.max_duration,
                BudgetDimension::Duration,
            ),
            (
                self.max_task_runs <= other.max_task_runs,
                BudgetDimension::TaskRuns,
            ),
            (
                self.max_parallel_subagents <= other.max_parallel_subagents,
                BudgetDimension::ParallelSubagents,
            ),
            (
                self.max_subagents <= other.max_subagents,
                BudgetDimension::Subagents,
            ),
            (
                self.max_cpu_seconds <= other.max_cpu_seconds,
                BudgetDimension::CpuSeconds,
            ),
            (
                self.max_cache_bytes <= other.max_cache_bytes,
                BudgetDimension::CacheBytes,
            ),
            (
                self.max_artifact_bytes <= other.max_artifact_bytes,
                BudgetDimension::ArtifactBytes,
            ),
            (
                self.max_egress_bytes <= other.max_egress_bytes,
                BudgetDimension::EgressBytes,
            ),
            (
                self.max_egress_requests <= other.max_egress_requests,
                BudgetDimension::EgressRequests,
            ),
        ];
        checks
            .into_iter()
            .find_map(|(ok, dimension)| (!ok).then_some(dimension))
    }

    /// The remaining headroom given consumption so far, saturating at zero.
    pub fn remaining(&self, usage: &BudgetUsage) -> BudgetRemaining {
        BudgetRemaining {
            duration_seconds: self
                .max_duration
                .as_secs()
                .saturating_sub(usage.duration_seconds),
            task_runs: self.max_task_runs.saturating_sub(usage.task_runs),
            parallel_subagents: self
                .max_parallel_subagents
                .saturating_sub(usage.parallel_subagents),
            subagents: self.max_subagents.saturating_sub(usage.subagents),
            cpu_seconds: self.max_cpu_seconds.saturating_sub(usage.cpu_seconds),
            cache_bytes: self.max_cache_bytes.saturating_sub(usage.cache_bytes),
            artifact_bytes: self.max_artifact_bytes.saturating_sub(usage.artifact_bytes),
            egress_bytes: self.max_egress_bytes.saturating_sub(usage.egress_bytes),
            egress_requests: self
                .max_egress_requests
                .saturating_sub(usage.egress_requests),
        }
    }

    /// Whether any dimension is fully consumed, and which.
    pub fn exhausted_dimension(&self, usage: &BudgetUsage) -> Option<BudgetDimension> {
        let remaining = self.remaining(usage);
        [
            (remaining.task_runs == 0, BudgetDimension::TaskRuns),
            (remaining.duration_seconds == 0, BudgetDimension::Duration),
            (remaining.cpu_seconds == 0, BudgetDimension::CpuSeconds),
            (
                remaining.artifact_bytes == 0,
                BudgetDimension::ArtifactBytes,
            ),
            (remaining.egress_bytes == 0, BudgetDimension::EgressBytes),
            (
                remaining.egress_requests == 0,
                BudgetDimension::EgressRequests,
            ),
        ]
        .into_iter()
        .find_map(|(spent, dimension)| spent.then_some(dimension))
    }
}

/// Remaining headroom, reported to actors through `mission_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetRemaining {
    pub duration_seconds: u64,
    pub task_runs: u32,
    pub parallel_subagents: u8,
    pub subagents: u8,
    pub cpu_seconds: u64,
    pub cache_bytes: u64,
    pub artifact_bytes: u64,
    pub egress_bytes: u64,
    pub egress_requests: u32,
}

impl BudgetUsage {
    /// Adds a cost, saturating rather than overflowing.
    ///
    /// Saturation is the safe direction: an overflowed counter that wrapped to
    /// zero would silently restore budget an actor had already spent.
    pub fn saturating_add(&self, cost: &BudgetCost) -> Self {
        Self {
            duration_seconds: self.duration_seconds.saturating_add(cost.duration_seconds),
            task_runs: self.task_runs.saturating_add(cost.task_runs),
            parallel_subagents: self
                .parallel_subagents
                .saturating_add(cost.parallel_subagents),
            subagents: self.subagents.saturating_add(cost.subagents),
            cpu_seconds: self.cpu_seconds.saturating_add(cost.cpu_seconds),
            cache_bytes: self.cache_bytes.saturating_add(cost.cache_bytes),
            artifact_bytes: self.artifact_bytes.saturating_add(cost.artifact_bytes),
            egress_bytes: self.egress_bytes.saturating_add(cost.egress_bytes),
            egress_requests: self.egress_requests.saturating_add(cost.egress_requests),
        }
    }

    /// Releases a parallel sub-agent slot when a session ends. Cumulative
    /// dimensions are never released; only concurrency is.
    pub fn release_parallel_subagent(&self) -> Self {
        Self {
            parallel_subagents: self.parallel_subagents.saturating_sub(1),
            ..*self
        }
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

    fn budget() -> Budget {
        Budget {
            max_duration: HumanDuration::parse("2h").unwrap(),
            max_task_runs: 50,
            max_parallel_subagents: 2,
            max_subagents: 4,
            max_cpu_seconds: 3600,
            max_cache_bytes: 4 << 30,
            max_artifact_bytes: 1 << 30,
            max_egress_bytes: 100 << 20,
            max_egress_requests: 500,
        }
    }

    #[test]
    fn narrowing_takes_the_element_wise_minimum() {
        let wider = Budget {
            max_task_runs: 500,
            max_egress_requests: 10,
            ..budget()
        };
        let narrowed = budget().narrowed_to(wider);
        assert_eq!(narrowed.max_task_runs, 50);
        assert_eq!(narrowed.max_egress_requests, 10);
    }

    #[test]
    fn is_within_names_the_first_violated_dimension() {
        let parent = budget();
        let child = Budget {
            max_task_runs: 10,
            ..parent
        };
        assert_eq!(child.is_within(&parent), None);
        let too_big = Budget {
            max_task_runs: 51,
            ..parent
        };
        assert_eq!(too_big.is_within(&parent), Some(BudgetDimension::TaskRuns));
        let too_long = Budget {
            max_duration: HumanDuration::parse("3h").unwrap(),
            ..parent
        };
        assert_eq!(too_long.is_within(&parent), Some(BudgetDimension::Duration));
    }

    #[test]
    fn usage_saturates_rather_than_wrapping() {
        let usage = BudgetUsage {
            task_runs: u32::MAX,
            ..BudgetUsage::default()
        };
        let next = usage.saturating_add(&BudgetCost::one_task_run());
        assert_eq!(
            next.task_runs,
            u32::MAX,
            "a wrapped counter would restore budget"
        );
    }

    #[test]
    fn exhaustion_reports_the_spent_dimension() {
        let budget = budget();
        let usage = BudgetUsage {
            task_runs: 50,
            ..BudgetUsage::default()
        };
        assert_eq!(
            budget.exhausted_dimension(&usage),
            Some(BudgetDimension::TaskRuns)
        );
        assert_eq!(budget.exhausted_dimension(&BudgetUsage::default()), None);
    }

    #[test]
    fn parallel_slots_are_released_but_totals_are_not() {
        let usage = BudgetUsage::default().saturating_add(&BudgetCost::one_subagent());
        assert_eq!(usage.subagents, 1);
        assert_eq!(usage.parallel_subagents, 1);
        let after = usage.release_parallel_subagent();
        assert_eq!(after.parallel_subagents, 0);
        assert_eq!(after.subagents, 1, "the total must not be released");
    }

    #[test]
    fn remaining_saturates_at_zero() {
        let usage = BudgetUsage {
            task_runs: 500,
            ..BudgetUsage::default()
        };
        assert_eq!(budget().remaining(&usage).task_runs, 0);
    }
}
