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

/// A ceiling on the total sample count a plan may produce.
///
/// A **realistic** bound, not merely an anti-overflow one: at ~64 bytes per
/// [`PlannedSample`] (a `String` condition included), the old 10⁹ ceiling reserved ~64 GB
/// in `Vec::with_capacity` and OOM-killed the process before returning `PlanError` — so a
/// hostile `--reps`/`--cases` could terminate the harness. Ten million samples reserves
/// well under a gigabyte and is ~10× the AA-1 cumulative floor (~10⁶ overflows), so no real
/// campaign approaches it while an absurd request is refused with a normal error.
pub const MAX_PLANNED_SAMPLES: u64 = 10_000_000;

/// Why a plan could not be built.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    /// `cells × cases × reps` exceeds [`MAX_PLANNED_SAMPLES`] (or overflows).
    #[error(
        "the plan would produce {total} samples ({cells} matrix cells × {cases} cases × {reps} \
         reps), over the {MAX_PLANNED_SAMPLES} ceiling — refuse rather than reserve a hostile \
         allocation"
    )]
    TooManySamples {
        /// Matrix cells (conditions × scales × payloads).
        cells: u64,
        /// Distinct target/seed cases per cell.
        cases: u64,
        /// Requested repetitions per case.
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
    /// Distinct **cases** per matrix cell — each draws its own seed and (for armed stages)
    /// its own seeded-random target. This is the dimension that gives the armed floor a
    /// *distribution* of distinct targets; without it, a million-overflow run drew a single
    /// target per cell and cloned it, so eight cells met the floor with eight deltas.
    pub cases: u64,
    /// Repetitions of each case, for replay identity. A repetition repeats the *input*
    /// (same seed + target), so reps of one case form the same-input group the
    /// replay-identity and rep-floor checks compare; distinct cases vary the target.
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

    // The matrix cells — (condition, scale, payload) only; the per-case seed and target are
    // drawn below, inside the case loop, so each case is a distinct input.
    let mut cells: Vec<(Payload, Scale, String)> = Vec::new();
    for condition in &spec.conditions {
        for &scale in &spec.scales {
            for &payload in &spec.payloads {
                cells.push((payload, scale, condition.clone()));
            }
        }
    }

    // Bound the total BEFORE reserving or iterating: `cells × cases × reps` can overflow
    // usize (`Vec::with_capacity` panics) or be merely absurd (OOM). Both are refused with a
    // normal error — the plan never reserves a hostile capacity.
    let n_cells = cells.len() as u64;
    let total = spec
        .cases
        .checked_mul(spec.reps)
        .and_then(|per_cell| n_cells.checked_mul(per_cell))
        .filter(|&t| t <= MAX_PLANNED_SAMPLES);
    let Some(total) = total else {
        return Err(PlanError::TooManySamples {
            cells: n_cells,
            cases: spec.cases,
            reps: spec.reps,
            total: n_cells.saturating_mul(spec.cases).saturating_mul(spec.reps),
        });
    };

    let mut out = Vec::with_capacity(total as usize);
    let mut sample_id = 0u64;
    for (payload, scale, condition) in &cells {
        for _case in 0..spec.cases {
            // A DISTINCT case: its own seed and (for armed stages) its own seeded-random
            // target, drawn once here — above the rep loop, so a repetition repeats the
            // INPUT (same seed + target) and forms the same-input replay group, while the
            // NEXT case draws a fresh target. The draw happens whether or not the stage uses
            // a target, so adding a range does not shift the seed stream.
            let draw = rng.next_u64();
            let target_delta = spec.target_delta_range.map(|(lo, hi)| {
                if hi <= lo {
                    lo
                } else {
                    // The inclusive range [lo, hi] has `hi - lo + 1` values. `+ 1` overflows
                    // only when the span is the whole of u64; then every u64 is in range, so
                    // the draw itself is the value. `lo + draw % width` cannot overflow.
                    match (hi - lo).checked_add(1) {
                        Some(width) => lo + draw % width,
                        None => draw,
                    }
                }
            });
            let seed = rng.next_u64();
            for _rep in 0..spec.reps {
                out.push(PlannedSample {
                    sample_id,
                    payload: *payload,
                    scale: *scale,
                    seed,
                    target_delta,
                    condition: condition.clone(),
                });
                sample_id += 1;
            }
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
            cases: 1,
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
        s.cases = 1;
        s.reps = 3;
        let p = plan(&s).unwrap();
        let cells = s.payloads.len() * s.scales.len() * s.conditions.len();
        assert_eq!(p.len(), cells * 3);

        // Each case's `reps` samples are consecutive and must repeat the SAME input.
        for chunk in p.chunks(3) {
            let first = &chunk[0];
            for sample in chunk {
                assert_eq!(sample.payload, first.payload);
                assert_eq!(sample.scale, first.scale);
                assert_eq!(sample.condition, first.condition);
                assert_eq!(
                    sample.seed, first.seed,
                    "a repetition of a case must carry the SAME seed"
                );
                assert_eq!(
                    sample.target_delta, first.target_delta,
                    "and the same target: a re-drawn deadline is a different experiment"
                );
            }
        }

        // And the cells themselves are still varied — the fix must not collapse every
        // cell onto one seed. (One case per cell here, so the first sample of each cell.)
        let distinct: std::collections::BTreeSet<u64> =
            p.iter().step_by(3).take(cells).map(|s| s.seed).collect();
        assert_eq!(distinct.len(), cells, "each cell draws its own seed");
    }

    #[test]
    fn distinct_cases_draw_distinct_targets_but_reps_repeat_them() {
        // The armed floor's whole point: a distribution of distinct seeded-random targets,
        // not one delta cloned to the floor. `cases` distinct draws per cell, each repeated
        // `reps` times for replay identity.
        let mut s = spec();
        s.cases = 4;
        s.reps = 2;
        let p = plan(&s).unwrap();
        // The first cell owns the first cases × reps = 8 samples.
        let first_cell = &p[0..8];
        // The 4 cases → 4 distinct (seed, target) pairs; each appears exactly `reps` times.
        let cases: std::collections::BTreeSet<(u64, Option<u64>)> = first_cell
            .iter()
            .map(|s| (s.seed, s.target_delta))
            .collect();
        assert_eq!(
            cases.len(),
            4,
            "4 cases draw 4 distinct (seed, target) pairs"
        );
        for chunk in first_cell.chunks(2) {
            assert_eq!(
                chunk[0].seed, chunk[1].seed,
                "each case's reps share the seed"
            );
            assert_eq!(
                chunk[0].target_delta, chunk[1].target_delta,
                "each case's reps share the target"
            );
        }
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
