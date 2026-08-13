//! spc_005: hierarchical resource budget — `Σ GrantedChildBudget ≤ ParentRemainingBudget`
//! recursively at any tree depth. Additive-only in this card: defines the shape, wires nothing.
//!
//! Not to be confused with [`crate::runtime::kernel::wire::BudgetGrant`] (the RunGroup
//! cumulative-subagent-count admission grant, a flat, unrelated concept — see spc_005 §8 baseline
//! notes for why the two must not merge).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::tcb::TaskId;

/// Opt-in resource limits/usage across the nine axes spc_005 §4 defines. Every field `None` means
/// "unbounded/untracked on this axis" — mirrors [`crate::governance::quota::ResourceQuota`]'s
/// unset-means-unlimited convention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceBudget {
    pub tokens: Option<u64>,
    pub cost_microunits: Option<u64>,
    pub turns: Option<u32>,
    pub wall_ms: Option<u64>,
    pub child_tasks: Option<u32>,
    pub concurrent_children: Option<u32>,
    pub tool_calls: Option<u64>,
    pub memory_writes: Option<u64>,
    pub object_bytes: Option<u64>,
}

/// A parent→child budget allocation: `reserved` at spawn time, `consumed` as the child runs,
/// `returned` once the child terminates and unused headroom flows back to the parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetGrant {
    pub parent: TaskId,
    pub child: TaskId,
    pub reserved: ResourceBudget,
    pub consumed: ResourceBudget,
    pub returned: ResourceBudget,
    /// Durable exactly-once settlement marker. `returned == default()` is not sufficient because
    /// a legitimate all-untracked grant also returns the default shape.
    #[serde(default)]
    pub settled: bool,
}

/// Card spc_005-02 failure: which of the nine axes the request exceeded, and by how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BudgetError {
    #[error(
        "budget dimension '{dimension}' insufficient: requested {requested}, remaining {remaining}"
    )]
    Insufficient {
        dimension: &'static str,
        requested: u64,
        remaining: u64,
    },
}

/// Per-dimension `requested <= parent_remaining` check (`None` = unbounded on that axis). All
/// nine axes must clear before a [`BudgetGrant`] is issued; the first violated axis is reported —
/// pure, no I/O, no mutation of `parent_remaining`.
pub fn reserve(
    parent: TaskId,
    child: TaskId,
    parent_remaining: &ResourceBudget,
    requested: &ResourceBudget,
) -> Result<BudgetGrant, BudgetError> {
    macro_rules! check {
        ($field:ident, $name:literal) => {
            if let (Some(want), Some(have)) = (requested.$field, parent_remaining.$field)
                && want > have
            {
                return Err(BudgetError::Insufficient {
                    dimension: $name,
                    requested: want as u64,
                    remaining: have as u64,
                });
            }
        };
    }
    check!(tokens, "tokens");
    check!(cost_microunits, "cost_microunits");
    check!(turns, "turns");
    check!(wall_ms, "wall_ms");
    check!(child_tasks, "child_tasks");
    check!(concurrent_children, "concurrent_children");
    check!(tool_calls, "tool_calls");
    check!(memory_writes, "memory_writes");
    check!(object_bytes, "object_bytes");

    Ok(BudgetGrant {
        parent,
        child,
        reserved: *requested,
        consumed: ResourceBudget::default(),
        returned: ResourceBudget::default(),
        settled: false,
    })
}

/// Add measured descendant/direct usage to an existing aggregate. Unlike [`credit`], `None` here
/// means "no usage recorded yet", not an unbounded pool, so a first `Some(delta)` must become
/// visible. Saturation keeps malformed/replayed counters from wrapping.
pub fn accumulate_usage(total: &ResourceBudget, delta: &ResourceBudget) -> ResourceBudget {
    macro_rules! accumulated {
        ($field:ident) => {
            match (total.$field, delta.$field) {
                (Some(current), Some(add)) => Some(current.saturating_add(add)),
                (None, Some(add)) => Some(add),
                (current, None) => current,
            }
        };
    }
    ResourceBudget {
        tokens: accumulated!(tokens),
        cost_microunits: accumulated!(cost_microunits),
        turns: accumulated!(turns),
        wall_ms: accumulated!(wall_ms),
        child_tasks: accumulated!(child_tasks),
        concurrent_children: accumulated!(concurrent_children),
        tool_calls: accumulated!(tool_calls),
        memory_writes: accumulated!(memory_writes),
        object_bytes: accumulated!(object_bytes),
    }
}

/// Card spc_005-05: the booking half of `return_unused()` — add a just-returned amount back into
/// the parent's own remaining pool, per dimension. A dimension unset in `amount` (nothing
/// returned on that axis) leaves `remaining`'s value on that axis untouched; a dimension unset in
/// `remaining` (unbounded) stays unbounded — crediting an infinite pool is still infinite.
pub fn credit(remaining: &ResourceBudget, amount: &ResourceBudget) -> ResourceBudget {
    macro_rules! credited {
        ($field:ident) => {
            match (remaining.$field, amount.$field) {
                (Some(r), Some(a)) => Some(r.saturating_add(a)),
                (r, _) => r,
            }
        };
    }
    ResourceBudget {
        tokens: credited!(tokens),
        cost_microunits: credited!(cost_microunits),
        turns: credited!(turns),
        wall_ms: credited!(wall_ms),
        child_tasks: credited!(child_tasks),
        concurrent_children: credited!(concurrent_children),
        tool_calls: credited!(tool_calls),
        memory_writes: credited!(memory_writes),
        object_bytes: credited!(object_bytes),
    }
}

/// Card spc_005-04: the booking half of `reserve()` — subtract a just-granted amount from the
/// parent's own remaining pool, per dimension. A dimension unset in `spent` (not part of this
/// grant) leaves `remaining`'s value on that axis untouched; a dimension unset in `remaining`
/// (unbounded) stays unbounded — debiting an infinite pool is still infinite.
pub fn debit(remaining: &ResourceBudget, spent: &ResourceBudget) -> ResourceBudget {
    macro_rules! debited {
        ($field:ident) => {
            match (remaining.$field, spent.$field) {
                (Some(r), Some(s)) => Some(r.saturating_sub(s)),
                (r, _) => r,
            }
        };
    }
    ResourceBudget {
        tokens: debited!(tokens),
        cost_microunits: debited!(cost_microunits),
        turns: debited!(turns),
        wall_ms: debited!(wall_ms),
        child_tasks: debited!(child_tasks),
        concurrent_children: debited!(concurrent_children),
        tool_calls: debited!(tool_calls),
        memory_writes: debited!(memory_writes),
        object_bytes: debited!(object_bytes),
    }
}

/// Card spc_005-03: per-dimension `reserved - consumed`, saturating at 0 (never negative). A
/// dimension unset in `reserved` (nothing was ever reserved on that axis) stays unset — there is
/// nothing to return. A dimension unset in `consumed` (no metering exists for that axis, or none
/// consumed yet) is treated as zero consumed, so the full `reserved` amount is returned.
pub fn return_unused(grant: &BudgetGrant) -> ResourceBudget {
    macro_rules! unused {
        ($field:ident) => {
            grant
                .reserved
                .$field
                .map(|r| r.saturating_sub(grant.consumed.$field.unwrap_or(0)))
        };
    }
    ResourceBudget {
        tokens: unused!(tokens),
        cost_microunits: unused!(cost_microunits),
        turns: unused!(turns),
        wall_ms: unused!(wall_ms),
        child_tasks: unused!(child_tasks),
        concurrent_children: unused!(concurrent_children),
        tool_calls: unused!(tool_calls),
        memory_writes: unused!(memory_writes),
        object_bytes: unused!(object_bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spc_005_01_budget_grant_fields_are_readable_with_zero_default_consumed_and_returned() {
        let grant = BudgetGrant {
            parent: TaskId::from("root"),
            child: TaskId::from("agent-1"),
            reserved: ResourceBudget {
                tokens: Some(1_000),
                ..ResourceBudget::default()
            },
            consumed: ResourceBudget::default(),
            returned: ResourceBudget::default(),
            settled: false,
        };

        assert_eq!(grant.parent, TaskId::from("root"));
        assert_eq!(grant.child, TaskId::from("agent-1"));
        assert_eq!(grant.reserved.tokens, Some(1_000));
        assert_eq!(grant.consumed, ResourceBudget::default());
        assert_eq!(grant.returned, ResourceBudget::default());
        assert_eq!(grant.consumed.tokens, None);
        assert_eq!(grant.returned.tokens, None);
    }

    #[test]
    fn spc_005_01_resource_budget_default_is_all_none() {
        let budget = ResourceBudget::default();
        assert_eq!(budget.tokens, None);
        assert_eq!(budget.cost_microunits, None);
        assert_eq!(budget.turns, None);
        assert_eq!(budget.wall_ms, None);
        assert_eq!(budget.child_tasks, None);
        assert_eq!(budget.concurrent_children, None);
        assert_eq!(budget.tool_calls, None);
        assert_eq!(budget.memory_writes, None);
        assert_eq!(budget.object_bytes, None);
    }

    #[test]
    fn spc_005_02_reserve_allows_a_request_within_every_dimensions_remaining_headroom() {
        let remaining = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            ..ResourceBudget::default()
        };
        let requested = ResourceBudget {
            tokens: Some(500),
            turns: Some(3),
            ..ResourceBudget::default()
        };
        let grant = reserve(
            TaskId::from("root"),
            TaskId::from("child"),
            &remaining,
            &requested,
        )
        .expect("request within headroom must be granted");
        assert_eq!(grant.reserved, requested);
        assert_eq!(grant.consumed, ResourceBudget::default());
    }

    #[test]
    fn spc_005_02_reserve_allows_a_request_exactly_equal_to_remaining_boundary() {
        let remaining = ResourceBudget {
            tokens: Some(1_000),
            ..ResourceBudget::default()
        };
        let requested = ResourceBudget {
            tokens: Some(1_000),
            ..ResourceBudget::default()
        };
        let grant = reserve(
            TaskId::from("root"),
            TaskId::from("child"),
            &remaining,
            &requested,
        )
        .expect("a request exactly equal to remaining is allowed, not rejected");
        assert_eq!(grant.reserved.tokens, Some(1_000));
    }

    #[test]
    fn spc_005_02_reserve_denies_and_pinpoints_the_dimension_that_overflows() {
        let remaining = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            ..ResourceBudget::default()
        };
        let requested = ResourceBudget {
            tokens: Some(500),
            turns: Some(50), // exceeds remaining turns=10
            ..ResourceBudget::default()
        };
        let err = reserve(
            TaskId::from("root"),
            TaskId::from("child"),
            &remaining,
            &requested,
        )
        .expect_err("a request exceeding one dimension must be denied");
        assert_eq!(
            err,
            BudgetError::Insufficient {
                dimension: "turns",
                requested: 50,
                remaining: 10,
            }
        );
    }

    #[test]
    fn spc_005_02_reserve_treats_an_unset_parent_dimension_as_unlimited() {
        let remaining = ResourceBudget::default(); // every dimension unset
        let requested = ResourceBudget {
            tokens: Some(1_000_000),
            ..ResourceBudget::default()
        };
        let grant = reserve(
            TaskId::from("root"),
            TaskId::from("child"),
            &remaining,
            &requested,
        )
        .expect("an unset parent dimension must not block the request");
        assert_eq!(grant.reserved.tokens, Some(1_000_000));
    }

    fn grant_with(reserved: ResourceBudget, consumed: ResourceBudget) -> BudgetGrant {
        BudgetGrant {
            parent: TaskId::from("root"),
            child: TaskId::from("child"),
            reserved,
            consumed,
            returned: ResourceBudget::default(),
            settled: false,
        }
    }

    #[test]
    fn spc_005_03_return_unused_equals_reserved_when_nothing_was_consumed() {
        let reserved = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            ..ResourceBudget::default()
        };
        let grant = grant_with(reserved, ResourceBudget::default());
        assert_eq!(return_unused(&grant), reserved);
    }

    #[test]
    fn spc_005_03_return_unused_is_all_zero_when_fully_consumed() {
        let reserved = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            ..ResourceBudget::default()
        };
        let consumed = reserved;
        let grant = grant_with(reserved, consumed);
        assert_eq!(
            return_unused(&grant),
            ResourceBudget {
                tokens: Some(0),
                turns: Some(0),
                ..ResourceBudget::default()
            }
        );
    }

    #[test]
    fn spc_005_03_return_unused_computes_each_dimension_independently_under_partial_consumption() {
        let reserved = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            wall_ms: Some(60_000),
            ..ResourceBudget::default()
        };
        let consumed = ResourceBudget {
            tokens: Some(500), // half used
            turns: Some(10),   // fully used
            wall_ms: Some(0),  // unused
            ..ResourceBudget::default()
        };
        let grant = grant_with(reserved, consumed);
        assert_eq!(
            return_unused(&grant),
            ResourceBudget {
                tokens: Some(500),
                turns: Some(0),
                wall_ms: Some(60_000),
                ..ResourceBudget::default()
            }
        );
    }

    #[test]
    fn spc_005_03_return_unused_never_goes_negative_even_if_overconsumed() {
        let reserved = ResourceBudget {
            tokens: Some(100),
            ..ResourceBudget::default()
        };
        let consumed = ResourceBudget {
            tokens: Some(500), // more than reserved — should saturate, not underflow/panic
            ..ResourceBudget::default()
        };
        let grant = grant_with(reserved, consumed);
        assert_eq!(return_unused(&grant).tokens, Some(0));
    }

    #[test]
    fn spc_005_04_debit_subtracts_spent_from_a_bounded_remaining_dimension() {
        let remaining = ResourceBudget {
            tokens: Some(1_000),
            ..ResourceBudget::default()
        };
        let spent = ResourceBudget {
            tokens: Some(400),
            ..ResourceBudget::default()
        };
        assert_eq!(debit(&remaining, &spent).tokens, Some(600));
    }

    #[test]
    fn spc_005_04_debit_leaves_an_unbounded_dimension_unbounded() {
        let remaining = ResourceBudget::default(); // every axis unset (unbounded)
        let spent = ResourceBudget {
            tokens: Some(400),
            ..ResourceBudget::default()
        };
        assert_eq!(debit(&remaining, &spent).tokens, None);
    }

    #[test]
    fn spc_005_04_debit_leaves_an_unrequested_dimension_untouched() {
        let remaining = ResourceBudget {
            tokens: Some(1_000),
            turns: Some(10),
            ..ResourceBudget::default()
        };
        let spent = ResourceBudget {
            tokens: Some(400),
            // turns not part of this grant
            ..ResourceBudget::default()
        };
        let after = debit(&remaining, &spent);
        assert_eq!(after.tokens, Some(600));
        assert_eq!(after.turns, Some(10));
    }

    #[test]
    fn spc_005_05_credit_adds_amount_to_a_bounded_remaining_dimension() {
        let remaining = ResourceBudget {
            tokens: Some(500),
            ..ResourceBudget::default()
        };
        let amount = ResourceBudget {
            tokens: Some(700),
            ..ResourceBudget::default()
        };
        assert_eq!(credit(&remaining, &amount).tokens, Some(1_200));
    }

    #[test]
    fn spc_005_05_credit_leaves_an_unbounded_dimension_unbounded() {
        let remaining = ResourceBudget::default();
        let amount = ResourceBudget {
            tokens: Some(700),
            ..ResourceBudget::default()
        };
        assert_eq!(credit(&remaining, &amount).tokens, None);
    }

    #[test]
    fn spc_005_05_credit_leaves_a_non_returned_dimension_untouched() {
        let remaining = ResourceBudget {
            tokens: Some(500),
            turns: Some(3),
            ..ResourceBudget::default()
        };
        let amount = ResourceBudget {
            tokens: Some(700),
            ..ResourceBudget::default()
        };
        let after = credit(&remaining, &amount);
        assert_eq!(after.tokens, Some(1_200));
        assert_eq!(after.turns, Some(3));
    }
}
