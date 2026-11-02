// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::search::archive::{COUNT_DECAY_SCALE, count_decay};

pub const SURVIVAL_FOLD_SHARE: u64 = 64;

pub const SURVIVAL_BUCKETS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Survival {
    pub draws: u64,
    pub deaths: u64,
}

impl Survival {
    pub fn record(&mut self, weight: u64, died: bool) {
        self.draws = self.draws.saturating_add(weight);
        if died {
            self.deaths = self.deaths.saturating_add(weight);
        }
    }

    #[must_use]
    pub fn estimate(self) -> (u64, u64) {
        (
            self.draws
                .saturating_sub(self.deaths)
                .saturating_add(COUNT_DECAY_SCALE),
            self.draws
                .saturating_add(COUNT_DECAY_SCALE.saturating_mul(2)),
        )
    }

    #[must_use]
    pub fn scale(self, weight: u64) -> u64 {
        let (survivors, draws) = self.estimate();
        u64::try_from(u128::from(weight) * u128::from(survivors) / u128::from(draws.max(1)))
            .unwrap_or(u64::MAX)
            .max(1)
    }

    #[must_use]
    pub fn bucket(self) -> usize {
        let (survivors, draws) = self.estimate();
        usize::try_from(u128::from(survivors) * SURVIVAL_BUCKETS as u128 / u128::from(draws.max(1)))
            .unwrap_or(0)
            .min(SURVIVAL_BUCKETS - 1)
    }
}

pub fn fold_weights() -> impl Iterator<Item = u64> {
    (0_u64..)
        .map(count_decay)
        .take_while(|weight| weight.saturating_mul(SURVIVAL_FOLD_SHARE) >= COUNT_DECAY_SCALE)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SurvivalHistogram {
    counts: Vec<u64>,
}

impl Default for SurvivalHistogram {
    fn default() -> Self {
        Self {
            counts: vec![0; SURVIVAL_BUCKETS],
        }
    }
}

impl SurvivalHistogram {
    pub fn add(&mut self, survival: Survival) {
        let count = &mut self.counts[survival.bucket()];
        *count = count.saturating_add(1);
    }

    pub fn remove(&mut self, survival: Survival) {
        let count = &mut self.counts[survival.bucket()];
        *count = count.saturating_sub(1);
    }

    #[must_use]
    pub fn median(&self) -> usize {
        let half = self.counts.iter().sum::<u64>().div_ceil(2);
        let mut seen = 0_u64;
        for (bucket, count) in self.counts.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= half && half > 0 {
                return bucket;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unseen_entry_sits_at_even_odds() {
        let survival = Survival::default();
        assert_eq!(survival.bucket(), SURVIVAL_BUCKETS / 2);
        assert_eq!(survival.scale(1000), 500);
    }

    #[test]
    fn deaths_lower_the_estimate_and_survivors_raise_it() {
        let mut dying = Survival::default();
        let mut living = Survival::default();
        for _ in 0..8 {
            dying.record(COUNT_DECAY_SCALE, true);
            living.record(COUNT_DECAY_SCALE, false);
        }
        assert!(dying.bucket() < SURVIVAL_BUCKETS / 2);
        assert!(living.bucket() > SURVIVAL_BUCKETS / 2);
        assert!(dying.scale(1000) < living.scale(1000));
    }

    #[test]
    fn folding_reaches_eight_generations() {
        let weights = fold_weights().collect::<Vec<_>>();
        assert_eq!(weights.len(), 8);
        assert_eq!(weights[0], COUNT_DECAY_SCALE);
    }

    #[test]
    fn the_median_follows_the_middle_entry() {
        let mut histogram = SurvivalHistogram::default();
        let mut dying = Survival::default();
        dying.record(COUNT_DECAY_SCALE.saturating_mul(16), true);
        histogram.add(dying);
        histogram.add(dying);
        histogram.add(Survival::default());
        assert_eq!(histogram.median(), dying.bucket());
        histogram.remove(dying);
        histogram.add(Survival::default());
        assert_eq!(histogram.median(), SURVIVAL_BUCKETS / 2);
    }
}
