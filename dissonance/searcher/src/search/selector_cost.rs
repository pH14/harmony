// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded, observation-only comparisons of conditional selector weights.
//! These counters never supply weights, random draws, or replay state.

use std::{cell::Cell, mem::size_of};

use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct LayerTotals {
    draws: u64,
    changed_distributions: u64,
    sum_tv_ppm_floor: u64,
    max_tv_ppm_floor: u64,
    terms: u64,
    capped_cost_terms: u64,
    equal_cost_rank_boundaries: u64,
}

#[derive(Default)]
pub(crate) struct LayerAudit(Cell<LayerTotals>);

impl LayerAudit {
    pub(crate) fn record(
        &self,
        ranked: &[usize],
        cost_free: &[usize],
        capped_cost_terms: usize,
        equal_cost_rank_boundaries: usize,
    ) {
        let (changed, tv) = conditional_distance(ranked, cost_free);
        let mut totals = self.0.get();
        totals.draws = totals.draws.saturating_add(1);
        totals.changed_distributions = totals
            .changed_distributions
            .saturating_add(u64::from(changed));
        totals.sum_tv_ppm_floor = totals.sum_tv_ppm_floor.saturating_add(tv);
        totals.max_tv_ppm_floor = totals.max_tv_ppm_floor.max(tv);
        totals.terms = totals
            .terms
            .saturating_add(u64::try_from(ranked.len()).unwrap_or(u64::MAX));
        totals.capped_cost_terms = totals
            .capped_cost_terms
            .saturating_add(u64::try_from(capped_cost_terms).unwrap_or(u64::MAX));
        totals.equal_cost_rank_boundaries = totals
            .equal_cost_rank_boundaries
            .saturating_add(u64::try_from(equal_cost_rank_boundaries).unwrap_or(u64::MAX));
        self.0.set(totals);
    }
}

/// Exact comparison of positive integer-normalized laws and a downward-rounded
/// total variation in millionths. Selector vectors have at most MAX_ARCHIVE_ENTRIES
/// (2^22) terms, each at most 2^24, so the products and scaled sum fit in u128.
fn conditional_distance(left: &[usize], right: &[usize]) -> (bool, u64) {
    assert_eq!(left.len(), right.len());
    let lsum: u128 = left.iter().map(|value| *value as u128).sum();
    let rsum: u128 = right.iter().map(|value| *value as u128).sum();
    assert!(lsum > 0 && rsum > 0);
    let difference: u128 = left
        .iter()
        .zip(right)
        .map(|(a, b)| ((*a as u128) * rsum).abs_diff((*b as u128) * lsum))
        .sum();
    let tv = difference * 1_000_000 / (2 * lsum * rsum);
    (
        difference != 0,
        u64::try_from(tv).expect("total variation is at most one"),
    )
}

#[derive(Default)]
pub(crate) struct SelectorCostAudit {
    pub(crate) between_cells: LayerAudit,
    pub(crate) within_cells: LayerAudit,
}

impl SelectorCostAudit {
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "format": "conditional-selector-cost-audit-v1",
            "scope": "cost-ranked versus cost-free conditional distributions at encountered selection stages; includes dispatched work not yet admitted; not a global adaptive effect bound",
            "persistent_memory_bytes": size_of::<Self>(),
            "temporary_memory": "two weight vectors at the current stage; each bounded by the existing selector group/window size; allocator overhead and sidecar formatting are additional",
            "between_cells": self.between_cells.0.get(),
            "within_cells": self.within_cells.0.get(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::conditional_distance;

    #[test]
    fn proportional_weights_are_the_same_law() {
        assert_eq!(conditional_distance(&[2, 4, 6], &[1, 2, 3]), (false, 0));
    }

    #[test]
    fn exact_distance_preserves_nonzero_changes_below_reporting_resolution() {
        assert_eq!(conditional_distance(&[2, 1], &[1, 2]), (true, 333_333));
        assert_eq!(
            conditional_distance(&[1_000_000, 1], &[1_000_000, 2]),
            (true, 0)
        );
    }
}
