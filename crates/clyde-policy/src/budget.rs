//! Budget charging.
//!
//! Charging happens at admission, before execution, so a daemon crash cannot
//! lose the charge (schema reference: Budget invariants).

use clyde_core::budget::{Budget, BudgetCost, BudgetDimension, BudgetUsage};
use clyde_core::decision::PolicyReason;

/// Charges `cost` against `budget`, returning the new usage.
///
/// Fails closed: if any dimension would exceed its ceiling, nothing is charged
/// and the exceeded dimension is named. Partial charging is deliberately not
/// offered — a caller that charged three of four dimensions and then failed
/// would leave the lease in a state no one intended.
pub fn charge_budget(
    budget: &Budget,
    usage: &BudgetUsage,
    cost: &BudgetCost,
) -> Result<BudgetUsage, PolicyReason> {
    let next = usage.saturating_add(cost);
    if let Some(dimension) = first_exceeded(budget, &next) {
        return Err(PolicyReason::BudgetExhausted { dimension });
    }
    Ok(next)
}

/// Which dimension of `usage` exceeds `budget`, if any.
fn first_exceeded(budget: &Budget, usage: &BudgetUsage) -> Option<BudgetDimension> {
    use BudgetDimension as D;
    [
        (
            usage.duration_seconds > budget.max_duration.as_secs(),
            D::Duration,
        ),
        (usage.task_runs > budget.max_task_runs, D::TaskRuns),
        (
            usage.parallel_subagents > budget.max_parallel_subagents,
            D::ParallelSubagents,
        ),
        (usage.subagents > budget.max_subagents, D::Subagents),
        (usage.cpu_seconds > budget.max_cpu_seconds, D::CpuSeconds),
        (usage.cache_bytes > budget.max_cache_bytes, D::CacheBytes),
        (
            usage.artifact_bytes > budget.max_artifact_bytes,
            D::ArtifactBytes,
        ),
        (usage.egress_bytes > budget.max_egress_bytes, D::EgressBytes),
        (
            usage.egress_requests > budget.max_egress_requests,
            D::EgressRequests,
        ),
    ]
    .into_iter()
    .find_map(|(exceeded, dimension)| exceeded.then_some(dimension))
}

/// Whether the budget is spent in some dimension, which moves a lease to
/// `exhausted` and blocks new work without terminating in-flight work.
pub fn exhausted_dimension(budget: &Budget, usage: &BudgetUsage) -> Option<BudgetDimension> {
    budget.exhausted_dimension(usage)
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
    use clyde_core::HumanDuration;

    fn budget() -> Budget {
        Budget {
            max_duration: HumanDuration::parse("1h").unwrap(),
            max_task_runs: 3,
            max_parallel_subagents: 1,
            max_subagents: 2,
            max_cpu_seconds: 100,
            max_cache_bytes: 1024,
            max_artifact_bytes: 2048,
            max_egress_bytes: 4096,
            max_egress_requests: 5,
        }
    }

    #[test]
    fn a_charge_within_budget_succeeds_and_accumulates() {
        let budget = budget();
        let usage = charge_budget(
            &budget,
            &BudgetUsage::default(),
            &BudgetCost::one_task_run(),
        )
        .expect("first charge");
        assert_eq!(usage.task_runs, 1);
        let usage = charge_budget(&budget, &usage, &BudgetCost::one_task_run()).expect("second");
        assert_eq!(usage.task_runs, 2);
    }

    #[test]
    fn charging_up_to_the_ceiling_is_permitted_and_beyond_it_is_not() {
        let budget = budget();
        let mut usage = BudgetUsage::default();
        for _ in 0..3 {
            usage = charge_budget(&budget, &usage, &BudgetCost::one_task_run()).expect("in budget");
        }
        assert_eq!(usage.task_runs, 3);
        let denial = charge_budget(&budget, &usage, &BudgetCost::one_task_run())
            .expect_err("the fourth run must be refused");
        assert_eq!(
            denial,
            PolicyReason::BudgetExhausted {
                dimension: BudgetDimension::TaskRuns
            }
        );
    }

    #[test]
    fn nothing_is_charged_when_any_dimension_would_be_exceeded() {
        let budget = budget();
        let usage = BudgetUsage {
            task_runs: 1,
            ..BudgetUsage::default()
        };
        let over = BudgetCost {
            task_runs: 1,
            egress_bytes: 1 << 20,
            ..BudgetCost::default()
        };
        assert!(charge_budget(&budget, &usage, &over).is_err());
        // The caller still holds the original usage, so the failed charge left
        // no partial state.
        assert_eq!(usage.task_runs, 1);
        assert_eq!(usage.egress_bytes, 0);
    }

    #[test]
    fn each_dimension_is_enforced() {
        let budget = budget();
        let cases = [
            (
                BudgetCost {
                    duration_seconds: 3601,
                    ..BudgetCost::default()
                },
                BudgetDimension::Duration,
            ),
            (
                BudgetCost {
                    subagents: 3,
                    ..BudgetCost::default()
                },
                BudgetDimension::Subagents,
            ),
            (
                BudgetCost {
                    parallel_subagents: 2,
                    ..BudgetCost::default()
                },
                BudgetDimension::ParallelSubagents,
            ),
            (
                BudgetCost {
                    cpu_seconds: 101,
                    ..BudgetCost::default()
                },
                BudgetDimension::CpuSeconds,
            ),
            (
                BudgetCost {
                    cache_bytes: 2048,
                    ..BudgetCost::default()
                },
                BudgetDimension::CacheBytes,
            ),
            (
                BudgetCost {
                    artifact_bytes: 4096,
                    ..BudgetCost::default()
                },
                BudgetDimension::ArtifactBytes,
            ),
            (BudgetCost::egress(8192, 0), BudgetDimension::EgressBytes),
            (BudgetCost::egress(0, 6), BudgetDimension::EgressRequests),
        ];
        for (cost, dimension) in cases {
            assert_eq!(
                charge_budget(&budget, &BudgetUsage::default(), &cost),
                Err(PolicyReason::BudgetExhausted { dimension }),
                "cost {cost:?} must be refused on {dimension}"
            );
        }
    }

    #[test]
    fn saturation_cannot_restore_spent_budget() {
        let budget = budget();
        let usage = BudgetUsage {
            task_runs: u32::MAX,
            ..BudgetUsage::default()
        };
        assert!(charge_budget(&budget, &usage, &BudgetCost::one_task_run()).is_err());
    }

    #[test]
    fn a_zero_cost_charge_is_a_no_op_but_still_checked() {
        let budget = budget();
        let usage = BudgetUsage {
            task_runs: 3,
            ..BudgetUsage::default()
        };
        // Already at the ceiling, but adding nothing does not exceed it.
        assert_eq!(
            charge_budget(&budget, &usage, &BudgetCost::default()).unwrap(),
            usage
        );
    }

    #[test]
    fn exhaustion_is_reported_separately_from_a_refused_charge() {
        let budget = budget();
        let spent = BudgetUsage {
            task_runs: 3,
            ..BudgetUsage::default()
        };
        assert_eq!(
            exhausted_dimension(&budget, &spent),
            Some(BudgetDimension::TaskRuns)
        );
        assert_eq!(exhausted_dimension(&budget, &BudgetUsage::default()), None);
    }
}
