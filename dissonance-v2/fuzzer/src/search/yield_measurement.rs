// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral post-run measurement of per-parent search yield.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

const PREDICTION_SCALE: u64 = 1_000_000;
const RATE_SCALE: u64 = 1_000_000_000;

/// One recorded draw projected onto game-neutral parent and class identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldObservation<Parent, Class> {
    /// Stable parent identity in the run's own archive.
    pub parent: Parent,
    /// Selector class containing that parent at measurement granularity.
    pub class: Class,
    /// Whether the draw discovered at least one retained descendant.
    pub productive: bool,
    /// Deterministic execution cost for the draw.
    pub cost: u64,
}

/// Registered parameters for a post-run measurement pass.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct YieldMeasurementParameters {
    /// Number of preceding class draws used by the class-only forecast.
    pub class_window: usize,
    /// Number of preceding draws of one parent used by its forecast.
    pub parent_window: usize,
    /// Minimum preceding parent draws required before scoring a forecast.
    pub minimum_parent_history: usize,
    /// Class-prior pseudo-draws mixed into the parent forecast.
    pub class_prior_strength: u64,
}

impl YieldMeasurementParameters {
    /// Check that every history and prior is non-vacuous.
    ///
    /// # Errors
    ///
    /// Returns an error if a window, minimum history, or prior is zero, or if
    /// the minimum parent history exceeds its window.
    pub fn validate(self) -> Result<(), &'static str> {
        if self.class_window == 0 || self.parent_window == 0 {
            return Err("yield measurement windows must be nonzero");
        }
        if self.minimum_parent_history == 0 {
            return Err("minimum parent history must be nonzero");
        }
        if self.minimum_parent_history > self.parent_window {
            return Err("minimum parent history exceeds the parent window");
        }
        if self.class_prior_strength == 0 {
            return Err("class prior strength must be nonzero");
        }
        Ok(())
    }
}

/// Fixed-point forecast comparison and cost-normalized outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct YieldMeasurementReport {
    /// Parameters used by the pure measurement pass.
    pub parameters: YieldMeasurementParameters,
    /// Executed draws presented to the pass.
    pub observations: u64,
    /// Distinct parents observed.
    pub parents: u64,
    /// Draws with enough prior parent history for a no-lookahead forecast.
    pub scored_forecasts: u64,
    /// Class-only mean Brier loss in parts per billion.
    pub class_brier_ppb: u64,
    /// Class-shrunk parent mean Brier loss in parts per billion.
    pub parent_brier_ppb: u64,
    /// Relative Brier improvement from the parent forecast, in signed basis points.
    pub brier_improvement_basis_points: i64,
    /// Yield per billion cost units when prior parent yield exceeded prior class yield.
    pub higher_parent_yield_per_billion_cost: u64,
    /// Yield per billion cost units otherwise.
    pub lower_parent_yield_per_billion_cost: u64,
    /// Scored draws in the higher-prior-parent-yield group.
    pub higher_group_draws: u64,
    /// Scored draws in the lower-prior-parent-yield group.
    pub lower_group_draws: u64,
    /// Distribution of mean execution cost across parents.
    pub parent_mean_cost: CostSpread,
    /// Whether the predeclared evidence rule recommends implementing budgets.
    pub recommend_budgets: bool,
    /// Plain-English result of the predeclared evidence rule.
    pub decision: String,
}

/// Integer percentile summary of per-parent mean execution cost.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CostSpread {
    /// Smallest per-parent mean cost.
    pub minimum: u64,
    /// Tenth percentile.
    pub p10: u64,
    /// Median.
    pub median: u64,
    /// Ninetieth percentile.
    pub p90: u64,
    /// Ninety-ninth percentile.
    pub p99: u64,
    /// Largest per-parent mean cost.
    pub maximum: u64,
}

#[derive(Clone, Debug, Default)]
struct History {
    records: VecDeque<(bool, u64)>,
    productive: u64,
    cost: u64,
}

impl History {
    fn push(&mut self, productive: bool, cost: u64, window: usize) {
        self.records.push_back((productive, cost));
        self.productive = self.productive.saturating_add(u64::from(productive));
        self.cost = self.cost.saturating_add(cost);
        while self.records.len() > window {
            if let Some((old_productive, old_cost)) = self.records.pop_front() {
                self.productive = self.productive.saturating_sub(u64::from(old_productive));
                self.cost = self.cost.saturating_sub(old_cost);
            }
        }
    }

    fn draws(&self) -> u64 {
        u64::try_from(self.records.len()).unwrap_or(u64::MAX)
    }
}

/// Compare a class-only recent forecast with a class-shrunk parent forecast.
///
/// Forecasts use only observations preceding the draw being scored. The Brier
/// comparison tests predictive value per draw; the separate higher/lower split
/// and cost distribution evaluate the budget objective in yield per unit cost.
/// The predeclared implementation rule requires at least 1,000 scored draws, at
/// least one percent relative Brier improvement, and positive cost-normalized
/// lift for parents whose own prior rate exceeds their class rate.
///
/// # Errors
///
/// Returns an error for invalid parameters or an observation with zero cost.
pub fn measure_parent_yield<Parent, Class>(
    observations: impl IntoIterator<Item = YieldObservation<Parent, Class>>,
    parameters: YieldMeasurementParameters,
) -> Result<YieldMeasurementReport, &'static str>
where
    Parent: Clone + Ord,
    Class: Clone + Ord,
{
    parameters.validate()?;
    let mut classes = BTreeMap::<Class, History>::new();
    let mut parents = BTreeMap::<Parent, History>::new();
    let mut parent_totals = BTreeMap::<Parent, (u64, u64)>::new();
    let mut observations_count = 0_u64;
    let mut scored = 0_u64;
    let mut class_squared_error = 0_u128;
    let mut parent_squared_error = 0_u128;
    let mut higher = (0_u64, 0_u64, 0_u64);
    let mut lower = (0_u64, 0_u64, 0_u64);

    for observation in observations {
        if observation.cost == 0 {
            return Err("yield measurement observation has zero cost");
        }
        observations_count = observations_count.saturating_add(1);
        let class_history = classes.entry(observation.class.clone()).or_default();
        let parent_history = parents.entry(observation.parent.clone()).or_default();
        if parent_history.records.len() >= parameters.minimum_parent_history
            && !class_history.records.is_empty()
        {
            let class_prediction = smoothed_prediction(class_history);
            let numerator = u128::from(parent_history.productive)
                .saturating_mul(u128::from(PREDICTION_SCALE))
                .saturating_add(
                    u128::from(class_prediction)
                        .saturating_mul(u128::from(parameters.class_prior_strength)),
                );
            let denominator = u128::from(
                parent_history
                    .draws()
                    .saturating_add(parameters.class_prior_strength),
            );
            let parent_prediction = divide_u128_to_u64(numerator, denominator);
            let actual = if observation.productive {
                PREDICTION_SCALE
            } else {
                0
            };
            class_squared_error = class_squared_error
                .saturating_add(u128::from(actual.abs_diff(class_prediction)).pow(2));
            parent_squared_error = parent_squared_error
                .saturating_add(u128::from(actual.abs_diff(parent_prediction)).pow(2));
            scored = scored.saturating_add(1);

            let parent_above_class = u128::from(parent_history.productive)
                .saturating_mul(u128::from(class_history.cost))
                > u128::from(class_history.productive)
                    .saturating_mul(u128::from(parent_history.cost));
            let group = if parent_above_class {
                &mut higher
            } else {
                &mut lower
            };
            group.0 = group.0.saturating_add(1);
            group.1 = group.1.saturating_add(u64::from(observation.productive));
            group.2 = group.2.saturating_add(observation.cost);
        }

        class_history.push(
            observation.productive,
            observation.cost,
            parameters.class_window,
        );
        parent_history.push(
            observation.productive,
            observation.cost,
            parameters.parent_window,
        );
        let total = parent_totals.entry(observation.parent).or_default();
        total.0 = total.0.saturating_add(1);
        total.1 = total.1.saturating_add(observation.cost);
    }

    let class_brier_ppb = scaled_brier(class_squared_error, scored);
    let parent_brier_ppb = scaled_brier(parent_squared_error, scored);
    let brier_improvement_basis_points =
        relative_improvement_basis_points(class_squared_error, parent_squared_error);
    let higher_rate = scaled_rate(higher.1, higher.2);
    let lower_rate = scaled_rate(lower.1, lower.2);
    let recommend_budgets =
        scored >= 1_000 && brier_improvement_basis_points >= 100 && higher_rate > lower_rate;
    let decision = if recommend_budgets {
        "yes: parent history clears the forecast and cost-normalized lift thresholds"
    } else {
        "no: parent history does not clear every predeclared evidence threshold"
    }
    .to_owned();

    let mut means = parent_totals
        .values()
        .map(|(draws, cost)| cost / draws.max(&1))
        .collect::<Vec<_>>();
    means.sort_unstable();
    Ok(YieldMeasurementReport {
        parameters,
        observations: observations_count,
        parents: u64::try_from(parent_totals.len()).unwrap_or(u64::MAX),
        scored_forecasts: scored,
        class_brier_ppb,
        parent_brier_ppb,
        brier_improvement_basis_points,
        higher_parent_yield_per_billion_cost: higher_rate,
        lower_parent_yield_per_billion_cost: lower_rate,
        higher_group_draws: higher.0,
        lower_group_draws: lower.0,
        parent_mean_cost: cost_spread(&means),
        recommend_budgets,
        decision,
    })
}

fn smoothed_prediction(history: &History) -> u64 {
    let numerator = u128::from(history.productive.saturating_add(1))
        .saturating_mul(u128::from(PREDICTION_SCALE));
    let denominator = u128::from(history.draws().saturating_add(2));
    divide_u128_to_u64(numerator, denominator)
}

fn divide_u128_to_u64(numerator: u128, denominator: u128) -> u64 {
    if denominator == 0 {
        return 0;
    }
    u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
}

fn scaled_brier(squared_error: u128, scored: u64) -> u64 {
    let denominator = u128::from(scored).saturating_mul(u128::from(PREDICTION_SCALE).pow(2));
    divide_u128_to_u64(
        squared_error.saturating_mul(u128::from(RATE_SCALE)),
        denominator,
    )
}

fn relative_improvement_basis_points(class_error: u128, parent_error: u128) -> i64 {
    if class_error == 0 {
        return 0;
    }
    let magnitude = class_error.abs_diff(parent_error).saturating_mul(10_000) / class_error;
    let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
    if parent_error <= class_error {
        magnitude
    } else {
        magnitude.saturating_neg()
    }
}

fn scaled_rate(productive: u64, cost: u64) -> u64 {
    divide_u128_to_u64(
        u128::from(productive).saturating_mul(u128::from(RATE_SCALE)),
        u128::from(cost),
    )
}

fn cost_spread(sorted: &[u64]) -> CostSpread {
    CostSpread {
        minimum: percentile(sorted, 0),
        p10: percentile(sorted, 10),
        median: percentile(sorted, 50),
        p90: percentile(sorted, 90),
        p99: percentile(sorted, 99),
        maximum: percentile(sorted, 100),
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0;
    };
    let index = last.saturating_mul(percentile).saturating_add(50) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{YieldMeasurementParameters, YieldObservation, measure_parent_yield};

    fn parameters() -> YieldMeasurementParameters {
        YieldMeasurementParameters {
            class_window: 16,
            parent_window: 8,
            minimum_parent_history: 2,
            class_prior_strength: 2,
        }
    }

    #[test]
    fn productive_parent_history_adds_signal() {
        let observations = (0..200).map(|index| {
            let parent = index % 2;
            YieldObservation {
                parent,
                class: 0,
                productive: parent == 0,
                cost: if parent == 0 { 10 } else { 20 },
            }
        });
        let report = measure_parent_yield(observations, parameters()).expect("measurement");
        assert!(report.brier_improvement_basis_points > 100);
        assert!(
            report.higher_parent_yield_per_billion_cost
                > report.lower_parent_yield_per_billion_cost
        );
    }

    #[test]
    fn no_signal_does_not_recommend_budgets() {
        let observations = (0..200).map(|index| YieldObservation {
            parent: index % 4,
            class: 0,
            productive: index % 4 < 2,
            cost: 10,
        });
        let report = measure_parent_yield(observations, parameters()).expect("measurement");
        assert!(
            !report.recommend_budgets,
            "sample is below the evidence floor"
        );
    }
}
