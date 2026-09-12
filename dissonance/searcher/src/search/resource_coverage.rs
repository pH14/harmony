// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact local two-state coverage of integer resource thresholds.

use std::cmp::Reverse;

/// One current representative or the proposed candidate.
pub(super) struct Point {
    pub index: usize,
    pub resources: [u64; 2],
    pub cost: u64,
    pub stable_id: u64,
}

// An inclusive u64 rectangle can contain 2^128 thresholds. The extra byte
// also holds the sum of two rectangles before subtracting their overlap.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Coverage(u8, u128);

impl Coverage {
    fn rectangle(resources: [u64; 2]) -> Self {
        let (low, carry) =
            (u128::from(resources[0]) + 1).overflowing_mul(u128::from(resources[1]) + 1);
        Self(u8::from(carry), low)
    }

    fn union(left: [u64; 2], right: [u64; 2]) -> Self {
        let a = Self::rectangle(left);
        let b = Self::rectangle(right);
        let overlap = Self::rectangle([left[0].min(right[0]), left[1].min(right[1])]);
        let (sum, carry) = a.1.overflowing_add(b.1);
        let (low, borrow) = sum.overflowing_sub(overlap.1);
        Self(
            a.0 + b.0 + u8::from(carry) - overlap.0 - u8::from(borrow),
            low,
        )
    }
}

/// The best subset of one or two points available *now*. This is not a
/// globally optimal streaming subset: previously discarded points are absent.
/// Equal coverage prefers fewer representatives, then the sorted per-entry
/// (within-group cost, stable id) pairs. Units and inclusive zero thresholds
/// are part of the policy. No workload outcome is inferred from this proxy.
pub(super) fn best(points: &[Point]) -> Vec<usize> {
    let mut winner = None;
    for (i, first) in points.iter().enumerate() {
        for (j, second) in points.iter().enumerate().skip(i) {
            let count = if i == j { 1 } else { 2 };
            let coverage = if count == 1 {
                Coverage::rectangle(first.resources)
            } else {
                Coverage::union(first.resources, second.resources)
            };
            let mut ranks = [
                (first.cost, first.stable_id),
                (second.cost, second.stable_id),
            ];
            ranks.sort_unstable();
            let score = (coverage, Reverse(count), Reverse(ranks));
            if winner
                .as_ref()
                .is_none_or(|(previous, _, _)| score > *previous)
            {
                winner = Some((score, i, j));
            }
        }
    }
    winner.map_or_else(Vec::new, |(_, i, j)| {
        if i == j {
            vec![points[i].index]
        } else {
            vec![points[i].index, points[j].index]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn union_matches_enumerated_thresholds_and_handles_full_u64_axes() {
        for ax in 0..5 {
            for ay in 0..5 {
                for bx in 0..5 {
                    for by in 0..5 {
                        let thresholds: BTreeSet<_> = (0..=ax)
                            .flat_map(|x| (0..=ay).map(move |y| (x, y)))
                            .chain((0..=bx).flat_map(|x| (0..=by).map(move |y| (x, y))))
                            .collect();
                        assert_eq!(
                            Coverage::union([ax, ay], [bx, by]),
                            Coverage(0, thresholds.len() as u128)
                        );
                    }
                }
            }
        }
        let maximum = [u64::MAX; 2];
        assert_eq!(Coverage::rectangle(maximum), Coverage(1, 0));
        assert_eq!(Coverage::union(maximum, maximum), Coverage(1, 0));
        assert_eq!(Coverage::union(maximum, [0, 0]), Coverage(1, 0));
        assert_eq!(
            Coverage::union([u64::MAX, 0], [0, u64::MAX]),
            Coverage(0, (1_u128 << 65) - 1)
        );
    }

    #[test]
    fn subset_matches_exhaustive_small_threshold_oracle() {
        for a in 0..9 {
            for b in 0..9 {
                for c in 0..9 {
                    let points: Vec<_> = [a, b, c]
                        .iter()
                        .enumerate()
                        .map(|(i, p)| Point {
                            index: i,
                            resources: [p / 3, p % 3],
                            cost: i as u64,
                            stable_id: i as u64,
                        })
                        .collect();
                    let coverage = |indices: &[usize]| -> usize {
                        (0..3)
                            .flat_map(|x| (0..3).map(move |y| (x, y)))
                            .filter(|(x, y)| {
                                indices.iter().any(|i| {
                                    points[*i].resources[0] >= *x && points[*i].resources[1] >= *y
                                })
                            })
                            .count()
                    };
                    let actual = best(&points);
                    let optimum = (0..3)
                        .flat_map(|i| (i..3).map(move |j| (i, j)))
                        .map(|(i, j)| coverage(&[i, j]))
                        .max()
                        .unwrap();
                    assert_eq!(coverage(&actual), optimum);
                    assert!(actual.len() <= 2);
                }
            }
        }
    }

    #[test]
    fn ties_use_fewer_points_then_cost_and_stable_id() {
        let points = [
            Point {
                index: 0,
                resources: [5, 5],
                cost: 2,
                stable_id: 0,
            },
            Point {
                index: 1,
                resources: [5, 5],
                cost: 1,
                stable_id: 2,
            },
            Point {
                index: 2,
                resources: [5, 5],
                cost: 1,
                stable_id: 1,
            },
        ];
        assert_eq!(best(&points), vec![2]);
        let reversed: Vec<_> = points.into_iter().rev().collect();
        assert_eq!(best(&reversed), vec![2]);
    }
}
