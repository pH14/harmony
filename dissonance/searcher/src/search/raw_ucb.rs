// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

pub(crate) const RAW_UCB_ALPHA: u128 = 3;

const FRACTION_BITS: u32 = 32;

const LN_2_Q32: u128 = 2_977_044_472;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Window {
    sum: Option<u64>,
    pending: u64,
    count: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Arm {
    pulls: u64,
    yields: u64,
    windows: Vec<Window>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Index {
    Finite(u128),
    Unpulled,
}

impl Arm {
    pub(crate) fn pulls(&self) -> u64 {
        self.pulls
    }

    pub(crate) fn yields(&self) -> u64 {
        self.yields
    }

    pub(crate) fn pull(&mut self, reward: bool) {
        let reward = u64::from(reward);
        self.pulls = self.pulls.saturating_add(1);
        self.yields = self.yields.saturating_add(reward);
        if self.windows.is_empty() {
            self.windows.push(Window::default());
        }
        let largest = self.windows.len() - 1;
        if largest < 63 && self.pulls == 1_u64 << largest {
            let Window { pending, count, .. } = self.windows[largest];
            self.windows.push(Window {
                sum: None,
                pending,
                count,
            });
        }
        self.windows[0] = Window {
            sum: Some(reward),
            pending: reward,
            count: 1,
        };
        for window in &mut self.windows[1..] {
            window.pending = window.pending.saturating_add(reward);
            window.count = window.count.saturating_add(1);
        }
        for level in (1..self.windows.len()).rev() {
            if self.windows[level].count == 1_u64 << level {
                let Window { pending, count, .. } = self.windows[level - 1];
                let window = &mut self.windows[level];
                window.sum = Some(window.pending);
                window.pending = pending;
                window.count = count;
            }
        }
    }

    pub(crate) fn index(&self, width: &Width) -> (Index, u64) {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|(level, window)| {
                window.sum.map(|sum| {
                    let mean = (u128::from(sum) << FRACTION_BITS) >> level;
                    (mean.saturating_add(width.at(level)), 1_u64 << level)
                })
            })
            .min_by_key(|(index, _)| *index)
            .map_or((Index::Unpulled, 0), |(index, window)| {
                (Index::Finite(index), window)
            })
    }

    pub(crate) fn memory_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.windows.len().saturating_mul(size_of::<Window>()))
    }
}

pub(crate) struct Width {
    root: u128,
}

impl Width {
    pub(crate) fn at_round(round: u64) -> Self {
        let level = ln_q32(round).saturating_mul(RAW_UCB_ALPHA) / 2;
        Self {
            root: (level << FRACTION_BITS).isqrt(),
        }
    }

    fn at(&self, level: usize) -> u128 {
        let inverse_root = (1_u128 << (2 * FRACTION_BITS as usize - level.min(63))).isqrt();
        self.root.saturating_mul(inverse_root) >> FRACTION_BITS
    }
}

pub(crate) fn ln_q32(value: u64) -> u128 {
    if value <= 1 {
        return 0;
    }
    let whole = 63 - value.leading_zeros();
    let one = 1_u128 << FRACTION_BITS;
    let mut mantissa = (u128::from(value) << FRACTION_BITS) >> whole;
    let mut fraction = 0_u128;
    for bit in (0..FRACTION_BITS).rev() {
        mantissa = (mantissa * mantissa) >> FRACTION_BITS;
        if mantissa >= 2 * one {
            mantissa >>= 1;
            fraction |= 1 << bit;
        }
    }
    (((u128::from(whole) << FRACTION_BITS) + fraction) * LN_2_Q32) >> FRACTION_BITS
}

pub(crate) fn to_millionths(index: Index) -> Option<u64> {
    match index {
        Index::Finite(value) => {
            u64::try_from(value.saturating_mul(1_000_000) >> FRACTION_BITS).ok()
        }
        Index::Unpulled => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_window_holds_the_latest_complete_block_of_its_size() {
        let mut state = 0x9e37_79b9_u64;
        let mut history = Vec::new();
        let mut arm = Arm::default();
        for _ in 0..5_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let reward = (state >> 61) == 0;
            history.push(reward);
            arm.pull(reward);
            let n = history.len();
            assert_eq!(arm.windows.len(), n.ilog2() as usize + 2);
            for (level, window) in arm.windows.iter().enumerate() {
                let size = 1_usize << level;
                let Some(sum) = window.sum else {
                    assert!(size > n, "window {size} is empty after {n} pulls");
                    continue;
                };
                let recent = (n.saturating_sub(2 * size - 1).max(size)..=n).any(|end| {
                    history[end - size..end].iter().filter(|r| **r).count() as u64 == sum
                });
                assert!(recent, "window {size} after {n} pulls holds {sum}");
            }
        }
        assert_eq!(arm.pulls(), 5_000);
        assert_eq!(
            arm.yields(),
            history.iter().filter(|reward| **reward).count() as u64
        );
    }

    #[test]
    fn the_natural_log_matches_to_six_places() {
        for (value, expected) in [
            (2_u64, std::f64::consts::LN_2),
            (3, 1.098_612_288_668_109_8),
            (10, std::f64::consts::LN_10),
            (1_000_003, 13.815_513_557_964_23),
            (u64::MAX, 44.361_419_555_836_5),
        ] {
            let actual = ln_q32(value) as f64 / f64::from(1_u32 << 16) / f64::from(1_u32 << 16);
            assert!((actual - expected).abs() < 1e-6, "{value}: {actual}");
        }
        assert_eq!(ln_q32(1), 0);
        assert_eq!(ln_q32(0), 0);
    }

    #[test]
    fn the_width_is_the_papers_confidence_term() {
        let width = Width::at_round(1_000);
        for level in [0_usize, 1, 5, 12] {
            let h = (1_u64 << level) as f64;
            let expected = (3.0 * 1_000_f64.ln() / (2.0 * h)).sqrt();
            let actual = width.at(level) as f64 / f64::from(1_u32 << 16) / f64::from(1_u32 << 16);
            assert!(
                (actual - expected).abs() < 1e-6,
                "{level}: {actual} {expected}"
            );
        }
    }

    #[test]
    fn the_index_is_the_tightest_window_bound() {
        let mut arm = Arm::default();
        assert_eq!(arm.index(&Width::at_round(10)).0, Index::Unpulled);
        for _ in 0..64 {
            arm.pull(true);
        }
        for _ in 0..64 {
            arm.pull(false);
        }
        let width = Width::at_round(1 << 20);
        let (index, window) = arm.index(&width);
        let expected = arm
            .windows
            .iter()
            .enumerate()
            .filter_map(|(level, w)| {
                w.sum
                    .map(|sum| ((u128::from(sum) << 32) >> level) + width.at(level))
            })
            .min()
            .unwrap();
        assert_eq!(index, Index::Finite(expected));
        assert_eq!(window, 64);
    }
}
