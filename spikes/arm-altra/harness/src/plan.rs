// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run-set planning: what samples to take, in what order, under what conditions.
//!
//! Deterministic by construction, and it has to be — a spike whose own experiment
//! order depended on wall-clock time or an unseeded RNG could not reproduce its own
//! evidence. Targets come from a seeded xorshift (the same generator the payloads
//! use), so a run-set is a pure function of its plan.

use oracle_model::{Payload, Scale, XorShift64Star};
use serde::{Deserialize, Serialize};

/// One sample the harness intends to take.
///
/// Every planned sample must appear in the evidence, passed or failed — that is
/// what the floor checker's totality rule enforces (`docs/ARM-ALTRA.md`
/// §Evidence integrity #6: *a missing sample is a failure to account, not a pass*).
/// Planning them up front, densely numbered, is what makes an omission detectable
/// at all.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PlannedSample {
    /// Dense index, `0..attempted`.
    pub sample_id: u64,
    /// The payload to run.
    pub payload: Payload,
    /// The scale.
    pub scale: Scale,
    /// The seed to publish in the params page.
    pub seed: u64,
    /// The work delta to arm a deadline at, for stages that land on a target.
    /// `None` for pure counting runs.
    pub target_delta: Option<u64>,
    /// The experimental condition (`pinned-solo`, `co-tenant-load`, …).
    pub condition: String,
}

/// How to build a run-set.
#[derive(Clone, Debug)]
pub struct PlanSpec {
    /// The payload classes to sweep.
    pub payloads: Vec<Payload>,
    /// The scales to sweep.
    pub scales: Vec<Scale>,
    /// The conditions to sweep.
    pub conditions: Vec<String>,
    /// Repetitions of the whole matrix.
    pub reps: u64,
    /// The plan's master seed. Everything else is derived from it.
    pub seed: u64,
    /// Inclusive range of work deltas to draw deadlines from. `None` for counting
    /// runs (AA-1(a)/(b)); AA-3 drives seeded-random targets over `1..=100_000`.
    pub target_delta_range: Option<(u64, u64)>,
}

/// Build the sample list.
///
/// The iteration order is fixed (reps, then condition, then scale, then payload)
/// and the draws come from one seeded stream, so the same [`PlanSpec`] always
/// yields the same plan — including the same targets, in the same order.
///
/// # A repetition repeats the *input*
///
/// The matrix is drawn **once**, and `reps` replays it. That is not a detail: a
/// repetition whose seed and target were re-drawn is not a repetition at all, it is
/// a different experiment — and the determinism gates are built on comparing
/// same-input runs. Advancing the RNG per rep (which this used to do) gave every
/// repetition a fresh seed, so no two records ever shared a
/// `(payload, scale, seed, condition, target)` key: the replay-identity check found
/// no group to compare and passed, and `--min-reps` counted rows. AA-6's
/// ≥1,000-same-seed-repetitions gate could then go green without two identical
/// executions ever being compared. The draw happens here, above the rep loop, so
/// that cannot recur.
#[must_use]
pub fn plan(spec: &PlanSpec) -> Vec<PlannedSample> {
    let mut rng = XorShift64Star::new(spec.seed);

    // The matrix: one draw per (condition, scale, payload) cell, before any rep.
    let mut matrix: Vec<(Payload, Scale, String, u64, Option<u64>)> = Vec::new();
    for condition in &spec.conditions {
        for &scale in &spec.scales {
            for &payload in &spec.payloads {
                // Draw for every cell whether or not the stage uses a target, so
                // that adding a target range does not shift the seed stream and
                // silently change which samples got which seeds.
                let draw = rng.next_u64();
                let target_delta = spec.target_delta_range.map(|(lo, hi)| {
                    if hi <= lo {
                        lo
                    } else {
                        lo + draw % (hi - lo + 1)
                    }
                });
                let seed = rng.next_u64();
                matrix.push((payload, scale, condition.clone(), seed, target_delta));
            }
        }
    }

    let mut out = Vec::with_capacity(matrix.len().saturating_mul(spec.reps as usize));
    let mut sample_id = 0u64;
    for _ in 0..spec.reps {
        for (payload, scale, condition, seed, target_delta) in &matrix {
            out.push(PlannedSample {
                sample_id,
                payload: *payload,
                scale: *scale,
                seed: *seed,
                target_delta: *target_delta,
                condition: condition.clone(),
            });
            sample_id += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PlanSpec {
        PlanSpec {
            payloads: vec![Payload::StraightLine, Payload::Svc],
            scales: vec![Scale::Smoke, Scale::S1e6],
            conditions: vec!["pinned-solo".into(), "co-tenant-load".into()],
            reps: 2,
            seed: 0xABCD,
            target_delta_range: Some((1, 100_000)),
        }
    }

    #[test]
    fn sample_ids_are_dense_and_ordered() {
        // The totality check depends on this: ids are exactly 0..attempted, so a
        // gap in the records is unambiguously a lost sample.
        let p = plan(&spec());
        assert_eq!(p.len(), 2 * 2 * 2 * 2);
        for (i, s) in p.iter().enumerate() {
            assert_eq!(s.sample_id, i as u64);
        }
    }

    #[test]
    fn the_plan_is_a_pure_function_of_its_spec() {
        assert_eq!(plan(&spec()), plan(&spec()));
    }

    #[test]
    fn a_repetition_repeats_the_input_it_does_not_redraw_it() {
        // The bug this pins: `reps` used to advance the RNG, so every repetition got
        // a fresh seed and target. No two samples then shared a
        // (payload, scale, seed, condition, target) key — the replay-identity check
        // found nothing to compare and passed, and --min-reps counted rows. AA-6's
        // "≥1,000 same-seed repetitions bit-identical" gate could go green without a
        // single pair of identical executions being compared.
        let mut s = spec();
        s.reps = 3;
        let p = plan(&s);
        let cells = s.payloads.len() * s.scales.len() * s.conditions.len();
        assert_eq!(p.len(), cells * 3);

        for (i, sample) in p.iter().enumerate() {
            let first = &p[i % cells];
            assert_eq!(sample.payload, first.payload);
            assert_eq!(sample.scale, first.scale);
            assert_eq!(sample.condition, first.condition);
            assert_eq!(
                sample.seed,
                first.seed,
                "repetition {} of cell {} must carry the SAME seed",
                i / cells,
                i % cells
            );
            assert_eq!(
                sample.target_delta, first.target_delta,
                "and the same target: a re-drawn deadline is a different experiment"
            );
        }

        // And the matrix itself is still varied — the fix must not collapse every
        // cell onto one seed.
        let distinct: std::collections::BTreeSet<u64> =
            p.iter().take(cells).map(|s| s.seed).collect();
        assert_eq!(distinct.len(), cells, "each cell draws its own seed");
    }

    #[test]
    fn reps_extend_the_sample_ids_densely() {
        let mut s = spec();
        s.reps = 3;
        let p = plan(&s);
        for (i, sample) in p.iter().enumerate() {
            assert_eq!(sample.sample_id, i as u64);
        }
    }

    #[test]
    fn a_different_master_seed_gives_different_targets() {
        let mut other = spec();
        other.seed = 0x1234;
        assert_ne!(plan(&spec()), plan(&other));
    }

    #[test]
    fn targets_stay_inside_the_requested_range() {
        let p = plan(&spec());
        for s in &p {
            let t = s.target_delta.expect("range given");
            assert!((1..=100_000).contains(&t), "target {t} out of range");
        }
    }

    #[test]
    fn adding_a_target_range_does_not_shift_the_seed_stream() {
        // A subtle trap worth a test: if the target draw were conditional, turning
        // targets on would change every sample's *seed* too, and two stages that
        // meant to run the same payloads on the same seeds would silently not.
        let with = plan(&spec());
        let mut without_spec = spec();
        without_spec.target_delta_range = None;
        let without = plan(&without_spec);
        for (a, b) in with.iter().zip(without.iter()) {
            assert_eq!(a.seed, b.seed);
        }
    }

    #[test]
    fn a_degenerate_range_does_not_divide_by_zero() {
        let mut s = spec();
        s.target_delta_range = Some((7, 7));
        for sample in plan(&s) {
            assert_eq!(sample.target_delta, Some(7));
        }
    }
}
