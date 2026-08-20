// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral search draw budgets derived from recent success per unit cost.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

/// Registered parameters for cost-normalized per-parent draw budgets.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DrawBudgetParameters {
    /// Recent outcomes retained for each parent.
    pub history_window: usize,
    /// Minimum draws available even to a parent with no recorded success.
    pub exploration_floor: u64,
    /// Upper bound on draws available before the selector's next reset.
    pub maximum_draws: u64,
    /// Cost units equivalent to one success in the saturating budget curve.
    pub success_cost_scale: u64,
}

impl DrawBudgetParameters {
    /// Validate the nonzero exploration floor and bounded budget curve.
    ///
    /// # Errors
    ///
    /// Returns an error when a parameter is zero or the floor exceeds the cap.
    pub fn validate(self) -> Result<(), &'static str> {
        if self.history_window == 0 {
            return Err("draw-budget history window must be nonzero");
        }
        if self.exploration_floor == 0 {
            return Err("draw-budget exploration floor must be nonzero");
        }
        if self.exploration_floor > self.maximum_draws {
            return Err("draw-budget exploration floor exceeds its maximum");
        }
        if self.success_cost_scale == 0 {
            return Err("draw-budget success cost scale must be nonzero");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct ParentHistory {
    outcomes: VecDeque<(bool, u64)>,
    successes: u64,
    cost: u64,
}

/// Stream-ordered recent histories for a set of parent identities.
#[derive(Clone, Debug, Default)]
pub struct DrawBudgets<Parent> {
    histories: BTreeMap<Parent, ParentHistory>,
}

impl<Parent> DrawBudgets<Parent>
where
    Parent: Clone + Ord,
{
    /// Record one completed draw in deterministic stream order.
    ///
    /// # Errors
    ///
    /// Returns an error when the cost is zero or parameters are invalid.
    pub fn record(
        &mut self,
        parent: Parent,
        productive: bool,
        cost: u64,
        parameters: DrawBudgetParameters,
    ) -> Result<(), &'static str> {
        parameters.validate()?;
        if cost == 0 {
            return Err("draw-budget outcome has zero cost");
        }
        let history = self.histories.entry(parent).or_default();
        history.outcomes.push_back((productive, cost));
        history.successes = history.successes.saturating_add(u64::from(productive));
        history.cost = history.cost.saturating_add(cost);
        while history.outcomes.len() > parameters.history_window {
            if let Some((old_productive, old_cost)) = history.outcomes.pop_front() {
                history.successes = history.successes.saturating_sub(u64::from(old_productive));
                history.cost = history.cost.saturating_sub(old_cost);
            }
        }
        Ok(())
    }

    /// Resolve the current nonzero budget for one parent.
    ///
    /// The bonus is a saturating integer function of successes per unit cost.
    /// A parent with no success receives exactly the exploration floor.
    ///
    /// # Errors
    ///
    /// Returns an error when parameters are invalid.
    pub fn budget(
        &self,
        parent: &Parent,
        parameters: DrawBudgetParameters,
    ) -> Result<u64, &'static str> {
        parameters.validate()?;
        let Some(history) = self.histories.get(parent) else {
            return Ok(parameters.exploration_floor);
        };
        let bonus_cap = parameters
            .maximum_draws
            .saturating_sub(parameters.exploration_floor);
        let success_cost =
            u128::from(history.successes).saturating_mul(u128::from(parameters.success_cost_scale));
        let denominator = u128::from(history.cost).saturating_add(success_cost);
        if denominator == 0 {
            return Ok(parameters.exploration_floor);
        }
        let bonus = u128::from(bonus_cap).saturating_mul(success_cost) / denominator;
        Ok(parameters
            .exploration_floor
            .saturating_add(u64::try_from(bonus).unwrap_or(bonus_cap))
            .min(parameters.maximum_draws))
    }
}

#[cfg(test)]
mod tests {
    use super::{DrawBudgetParameters, DrawBudgets};

    fn parameters() -> DrawBudgetParameters {
        DrawBudgetParameters {
            history_window: 2,
            exploration_floor: 4,
            maximum_draws: 64,
            success_cost_scale: 256,
        }
    }

    #[test]
    fn every_parent_has_a_nonzero_floor() {
        let budgets = DrawBudgets::<u64>::default();
        assert_eq!(budgets.budget(&7, parameters()).expect("budget"), 4);
    }

    #[test]
    fn productive_low_cost_history_gets_more_draws_and_window_expires() {
        let mut budgets = DrawBudgets::<u64>::default();
        budgets.record(1, true, 64, parameters()).expect("success");
        let productive = budgets.budget(&1, parameters()).expect("budget");
        assert!(productive > parameters().exploration_floor);
        budgets
            .record(1, false, 512, parameters())
            .expect("failure");
        budgets
            .record(1, false, 512, parameters())
            .expect("expired success");
        assert_eq!(
            budgets.budget(&1, parameters()).expect("floor"),
            parameters().exploration_floor
        );
    }
}
