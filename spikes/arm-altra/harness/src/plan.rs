// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run-set planning: what samples to take, in what order, under what conditions.
//!
//! Deterministic by construction, and it has to be — a spike whose own experiment
//! order depended on wall-clock time or an unseeded RNG could not reproduce its own
//! evidence. Targets come from a seeded xorshift (the same generator the payloads
//! use), so a run-set is a pure function of its plan.

use oracle_model::{Payload, Scale, XorShift64Star};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A ceiling on the total sample count a plan may produce. Far above any real run (the
/// AA-1 cumulative floor is ~10⁶ overflows), it exists only so untrusted input — a CLI
/// `--reps 18446744073709551615` — is refused with a normal error instead of overflowing
/// `Vec::with_capacity` (a `capacity overflow` panic) or OOM-looping.
pub const MAX_PLANNED_SAMPLES: u64 = 1_000_000_000;

/// Why a plan could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    /// The matrix size times `reps` exceeds [`MAX_PLANNED_SAMPLES`] (or overflows).
    #[error(
        "the plan would produce {total} samples ({cells} matrix cells × {reps} reps), over the \
         {MAX_PLANNED_SAMPLES} ceiling — refuse rather than overflow the allocation"
    )]
    TooManySamples {
        /// Matrix cells (conditions × scales × payloads).
        cells: u64,
        /// Requested repetitions.
        reps: u64,
        /// The product (saturated), for the message.
        total: u64,
    },
}

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
///
/// # Errors
/// [`PlanError::TooManySamples`] if the matrix size times `reps` exceeds
/// [`MAX_PLANNED_SAMPLES`] or overflows — `PlanSpec` is public and takes untrusted input
/// (a hostile `--reps`), and the no-panic contract means an absurd total is a normal
/// error, never a `capacity overflow` panic.
pub fn plan(spec: &PlanSpec) -> Result<Vec<PlannedSample>, PlanError> {
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
                        // The inclusive range [lo, hi] has `hi - lo + 1` values. `hi - lo`
                        // is safe (hi > lo), but `+ 1` overflows when the span is the whole
                        // of u64 (lo == 0, hi == u64::MAX) — a debug build panics on the
                        // add, a release build wraps the divisor to 0 and panics on the
                        // modulo. When the width would overflow, every u64 is in range, so
                        // the draw itself is the value. (`lo + draw % width` cannot
                        // overflow: `draw % width <= hi - lo`, so the sum is at most `hi`.)
                        match (hi - lo).checked_add(1) {
                            Some(width) => lo + draw % width,
                            None => draw,
                        }
                    }
                });
                let seed = rng.next_u64();
                matrix.push((payload, scale, condition.clone(), seed, target_delta));
            }
        }
    }

    // Bound the total BEFORE reserving or iterating: `matrix.len() * reps` can overflow
    // usize (`Vec::with_capacity` panics) or be merely absurd (OOM loop). Both are refused
    // with a normal error — the plan never allocates a hostile capacity.
    let cells = matrix.len() as u64;
    let total = cells.saturating_mul(spec.reps);
    if total > MAX_PLANNED_SAMPLES || cells.checked_mul(spec.reps).is_none() {
        return Err(PlanError::TooManySamples {
            cells,
            reps: spec.reps,
            total,
        });
    }

    let mut out = Vec::with_capacity(total as usize);
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
    Ok(out)
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
        let p = plan(&spec()).unwrap();
        assert_eq!(p.len(), 2 * 2 * 2 * 2);
        for (i, s) in p.iter().enumerate() {
            assert_eq!(s.sample_id, i as u64);
        }
    }

    #[test]
    fn the_plan_is_a_pure_function_of_its_spec() {
        assert_eq!(plan(&spec()).unwrap(), plan(&spec()).unwrap());
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
        let p = plan(&s).unwrap();
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
        let p = plan(&s).unwrap();
        for (i, sample) in p.iter().enumerate() {
            assert_eq!(sample.sample_id, i as u64);
        }
    }

    #[test]
    fn a_different_master_seed_gives_different_targets() {
        let mut other = spec();
        other.seed = 0x1234;
        assert_ne!(plan(&spec()).unwrap(), plan(&other).unwrap());
    }

    #[test]
    fn targets_stay_inside_the_requested_range() {
        let p = plan(&spec()).unwrap();
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
        let with = plan(&spec()).unwrap();
        let mut without_spec = spec();
        without_spec.target_delta_range = None;
        let without = plan(&without_spec).unwrap();
        for (a, b) in with.iter().zip(without.iter()) {
            assert_eq!(a.seed, b.seed);
        }
    }

    #[test]
    fn a_degenerate_range_does_not_divide_by_zero() {
        let mut s = spec();
        s.target_delta_range = Some((7, 7));
        for sample in plan(&s).unwrap() {
            assert_eq!(sample.target_delta, Some(7));
        }
    }

    #[test]
    fn an_absurd_rep_count_is_a_normal_error_not_a_capacity_panic() {
        // `--reps u64::MAX` made `matrix.len() * reps` saturate to usize::MAX and
        // `Vec::with_capacity` panic with "capacity overflow". PlanSpec is public and
        // takes untrusted input, so it must return a normal error instead.
        let mut s = spec();
        s.reps = u64::MAX;
        assert!(matches!(plan(&s), Err(PlanError::TooManySamples { .. })));

        // A merely-large-but-finite total over the ceiling is refused the same way.
        s.reps = MAX_PLANNED_SAMPLES; // × the matrix cells → well over the ceiling
        assert!(matches!(plan(&s), Err(PlanError::TooManySamples { .. })));

        // A normal rep count still plans fine.
        s.reps = 3;
        assert!(plan(&s).is_ok());
    }

    #[test]
    fn a_full_width_range_does_not_overflow_or_panic() {
        // `(0, u64::MAX)` makes the inclusive width `hi - lo + 1` overflow: a debug
        // build panicked on the add, a release build wrapped the divisor to 0 and
        // panicked on the modulo. `PlanSpec` is public and takes untrusted input, so
        // it must not panic. Every drawn delta must still be a valid u64 in range.
        let mut s = spec();
        s.target_delta_range = Some((0, u64::MAX));
        let samples = plan(&s).unwrap();
        assert!(!samples.is_empty());
        // (Every u64 is in [0, u64::MAX]; the point is that this ran at all.)

        // A near-maximal window one below the overflow boundary is also fine.
        s.target_delta_range = Some((1, u64::MAX));
        for sample in plan(&s).unwrap() {
            assert!(sample.target_delta.unwrap() >= 1);
        }
    }
}
