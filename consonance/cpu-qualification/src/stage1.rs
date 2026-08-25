// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 1 — counter measurement. The portable half.
//!
//! Stage 1 measures the counter itself: count exactness, overflow delivery, and
//! skid. The measurements are Linux-only and live in [`crate::stage1_sys`];
//! everything here — the plan, the analytical oracle, the interrupt-delta
//! cleanliness rule, the skid histogram, the margin derivation, and the
//! projection discipline — is portable and unit-tested everywhere.
//!
//! The oracle is always the analysis. A count is judged against what the
//! payload's branch structure says it must be, never against a second counter.

use crate::payload::PayloadSpec;
use crate::report::{Record, project_millis};

/// A refusal from stage 1.
#[derive(Debug, thiserror::Error)]
pub enum Stage1Error {
    /// The measurement cannot run from here.
    #[error("stage 1 measures on Linux and this build is for {target}")]
    WrongPlatform {
        /// The platform this build targets.
        target: &'static str,
    },
    /// A counter operation failed.
    #[error("{what} failed: {detail}")]
    Counter {
        /// What was being done.
        what: String,
        /// Why it failed.
        detail: String,
    },
    /// A source the measurement needs could not be read.
    #[error("cannot read {what}: {detail}")]
    Read {
        /// What was being read.
        what: String,
        /// Why the read failed.
        detail: String,
    },
    /// The plan asks for something this host or this build cannot provide.
    #[error("{what} is unavailable: {detail}")]
    Unavailable {
        /// What was asked for.
        what: String,
        /// Why it is unavailable.
        detail: String,
    },
    /// A payload's iteration counts do not admit a differential.
    #[error("payload {payload}: n1 {n1} must be below n2 {n2}")]
    BadScales {
        /// The payload class.
        payload: String,
        /// The smaller scale.
        n1: u64,
        /// The larger scale.
        n2: u64,
    },
}

/// What the measurement core was competing with while a repetition ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Interference {
    /// Nothing else was asked to run.
    Quiet,
    /// A busy thread on the measurement core's simultaneous-multithreading
    /// sibling.
    SmtSibling,
    /// A busy thread on a different physical core.
    CoTenant,
    /// A thread churning memory large enough to evict the payload's working set.
    MemoryPressure,
}

impl Interference {
    /// The token the report spells this condition with.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Interference::Quiet => "quiet",
            Interference::SmtSibling => "smt-sibling",
            Interference::CoTenant => "co-tenant",
            Interference::MemoryPressure => "memory-pressure",
        }
    }

    /// Every condition, quiet first.
    #[must_use]
    pub fn all() -> [Interference; 4] {
        [
            Interference::Quiet,
            Interference::SmtSibling,
            Interference::CoTenant,
            Interference::MemoryPressure,
        ]
    }
}

/// What a stage-1 run intends to do, fixed before any measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasurementPlan {
    /// The core the measurement runs on.
    pub core: usize,
    /// The smaller scale of the exactness differential.
    pub n1: u64,
    /// The larger scale of the exactness differential.
    pub n2: u64,
    /// Repetitions per payload per interference condition. A repetition that
    /// catches an interrupt cannot be judged, so this is how many windows are
    /// attempted, not how many the plan commits to judging.
    pub reps: u64,
    /// Interrupt-free windows the plan commits to judging, per payload per
    /// condition. Never larger than `reps`, and the two are separate because a
    /// host that takes an interrupt in one window has not failed exactness.
    pub min_clean_reps: u64,
    /// The overflow periods to arm at.
    pub periods: Vec<u64>,
    /// Arms per period.
    pub arms_per_period: u64,
    /// The conditions the exactness slice is repeated under.
    pub conditions: Vec<Interference>,
}

/// The differential's smaller scale. Large enough that the fixed work around
/// the loop is a rounding error beside the counted iterations, and short enough
/// that a window can complete between two timer interrupts on a busy host.
const STANDARD_N1: u64 = 100_000;
/// The differential's larger scale.
const STANDARD_N2: u64 = 200_000;
/// Exactness windows attempted per payload per condition. Most of the surplus
/// over the floor is spent under the interference conditions, where a window
/// that finishes without an interrupt is the exception.
const STANDARD_REPS: u64 = 512;
/// Interrupt-free windows required per payload per condition.
const STANDARD_MIN_CLEAN_REPS: u64 = 32;
/// Two periods apart by an order of magnitude, so the skid distribution is
/// measured across the range the machinery arms at rather than at one point.
const STANDARD_PERIODS: [u64; 2] = [10_000, 100_000];
/// Arms per period per payload. With two periods and the four payload classes
/// that run on every architecture, this reaches the specified floor of a
/// million delivered overflows.
const STANDARD_ARMS_PER_PERIOD: u64 = 125_000;

impl MeasurementPlan {
    /// The plan the suite runs when nothing narrows it.
    #[must_use]
    pub fn standard(core: usize) -> MeasurementPlan {
        MeasurementPlan {
            core,
            n1: STANDARD_N1,
            n2: STANDARD_N2,
            reps: STANDARD_REPS,
            min_clean_reps: STANDARD_MIN_CLEAN_REPS,
            periods: STANDARD_PERIODS.to_vec(),
            arms_per_period: STANDARD_ARMS_PER_PERIOD,
            conditions: Interference::all().to_vec(),
        }
    }

    /// How many interrupt-free exactness windows the plan commits to judging,
    /// per payload per condition.
    #[must_use]
    pub fn clean_reps_floor(&self) -> u64 {
        self.min_clean_reps.min(self.reps)
    }

    /// How many overflows the plan commits to delivering, across `payloads`
    /// payload classes.
    #[must_use]
    pub fn overflow_floor(&self, payloads: u64) -> u64 {
        self.arms_per_period
            .saturating_mul(self.periods.len() as u64)
            .saturating_mul(payloads)
    }

    /// A short slice of this plan, for the projection the discipline requires
    /// before a long campaign.
    #[must_use]
    pub fn slice(&self, reps: u64, arms: u64) -> MeasurementPlan {
        let reps = reps.min(self.reps);
        MeasurementPlan {
            reps,
            min_clean_reps: self.min_clean_reps.min(reps),
            arms_per_period: arms.min(self.arms_per_period),
            ..self.clone()
        }
    }
}

/// What a stage-1 run produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stage1Outcome {
    /// Every retained record, in the order it was produced.
    pub records: Vec<Record>,
    /// The skid distribution across every delivered overflow. The report
    /// recomputes the maximum from the per-arm records; this is the shape of
    /// the distribution the pack's margin is derived from.
    pub skid: SkidBuckets,
    /// Measurements stage 1 requires that this run did not make, each naming
    /// what it would need. A run with any of these is incomplete.
    pub unmeasured: Vec<String>,
}

/// The guest window's payload is real-mode x86 and its state components are the
/// x86 set, so the guest half of stage 1 runs on x86-64 and reports itself
/// unmeasured elsewhere.
pub const GUEST_WINDOW_NOT_BUILT: &str = "the guest half of stage 1 (guest-side count exactness and the save/restore \
     fixpoint): the guest window is built for x86-64 and this host is not";

/// The analytical count for the difference between two scales, from the
/// per-iteration event count alone.
///
/// The differential cancels whatever fixed work brackets the loop, so the
/// oracle is exact without knowing the prologue's branch count. This is the
/// only oracle the suite uses; a second counter is never one.
#[must_use]
pub fn oracle_events(events_per_iteration: u64, n1: u64, n2: u64) -> u64 {
    events_per_iteration.saturating_mul(n2.saturating_sub(n1))
}

/// The analytical count for the difference between two scales of a payload.
#[must_use]
pub fn oracle_delta(spec: &PayloadSpec, n1: u64, n2: u64) -> u64 {
    oracle_events(spec.events_per_iteration, n1, n2)
}

/// The fixed work the smaller window measured beyond its own iterations. Stable
/// across repetitions on a counter that counts what it claims to.
#[must_use]
pub fn offset_from_counts(events_per_iteration: u64, n1: u64, count_n1: u64) -> i128 {
    i128::from(count_n1) - i128::from(events_per_iteration) * i128::from(n1)
}

/// The fixed work the smaller window of a payload measured beyond its own
/// iterations.
#[must_use]
pub fn count_offset(spec: &PayloadSpec, n1: u64, count_n1: u64) -> i128 {
    offset_from_counts(spec.events_per_iteration, n1, count_n1)
}

/// How many iterations of `spec` an arm runs to cross `period`.
///
/// Twice the analytical minimum, so an arm whose counter starts a little into
/// the payload still crosses the period rather than reporting a lost overflow.
#[must_use]
pub fn iterations_for(spec: &PayloadSpec, period: u64) -> u64 {
    let per_iteration = spec.events_per_iteration.max(1);
    period.div_ceil(per_iteration).saturating_mul(2).max(1)
}

/// A window is clean when no interrupt landed in either of its two counted
/// halves. Only clean windows are held to the oracle; a contaminated window's
/// excess is accounted interrupts, not a counting defect.
#[must_use]
pub fn is_clean(irqs_n1: u64, irqs_n2: u64) -> bool {
    irqs_n1 == 0 && irqs_n2 == 0
}

/// The skid histogram's bucket edges, in work units. A bucket holds skids from
/// its edge up to the next.
pub const SKID_BUCKET_EDGES: [u64; 6] = [0, 1, 10, 100, 1_000, 10_000];

/// The skid distribution across a set of delivered overflows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkidBuckets {
    /// Counts, one per edge in [`SKID_BUCKET_EDGES`].
    pub counts: [u64; 6],
    /// The largest skid seen.
    pub max: u64,
    /// The smallest skid seen, when anything was seen.
    pub min: Option<u64>,
    /// How many skids went into the distribution.
    pub total: u64,
}

impl SkidBuckets {
    /// Add one delivered overflow's skid.
    pub fn observe(&mut self, skid: u64) {
        let bucket = SKID_BUCKET_EDGES
            .iter()
            .rposition(|edge| skid >= *edge)
            .unwrap_or(0);
        self.counts[bucket] += 1;
        self.max = self.max.max(skid);
        self.min = Some(self.min.map_or(skid, |m| m.min(skid)));
        self.total += 1;
    }

    /// The distribution as a report line.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "n={} min={} max={} buckets={:?} edges={:?}",
            self.total,
            self.min
                .map_or_else(|| "none".to_string(), |m| m.to_string()),
            self.max,
            self.counts,
            SKID_BUCKET_EDGES
        )
    }
}

/// The skid margin the pack should record, and the sentence stating how it was
/// derived.
///
/// The rule is the one already in the backend: arm at twice the bound the
/// distribution establishes, so a skid at the full observed maximum still lands
/// with as much headroom again.
#[must_use]
pub fn derive_margin(observed_max: u64) -> (u64, String) {
    let margin = observed_max.saturating_mul(2);
    (
        margin,
        format!(
            "twice the observed maximum skid of {observed_max}, so a skid at the full \
             observed maximum still leaves {observed_max} work units of headroom"
        ),
    )
}

/// The projection a long campaign must record before it starts: how long the
/// whole campaign will take, extrapolated from a measured short slice.
#[must_use]
pub fn projection(campaign: &str, slice_units: u64, slice_millis: u64, total_units: u64) -> Record {
    Record::Projection {
        campaign: campaign.to_string(),
        slice_units,
        slice_millis,
        total_units,
        projected_millis: project_millis(slice_units, slice_millis, total_units),
    }
}

/// One core's column of `/proc/interrupts`, summed over every row that has one.
///
/// Read before and after a counted window, the difference is how many
/// interrupts landed during it. Rows with fewer numeric columns than the core
/// needs (`ERR`, `MIS`) contribute nothing.
#[must_use]
pub fn interrupts_for_core(text: &str, core: usize) -> u64 {
    let mut total = 0u64;
    // The first line is the CPU header; every later line is `name: n n n ...`.
    for line in text.lines().skip(1) {
        let Some((_, columns)) = line.split_once(':') else {
            continue;
        };
        let value = columns
            .split_whitespace()
            .take_while(|word| word.chars().all(|c| c.is_ascii_digit()))
            .nth(core)
            .and_then(|word| word.parse::<u64>().ok());
        if let Some(value) = value {
            total += value;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{BRANCH_DENSE, LOOP_BACKEDGE};

    #[test]
    fn the_oracle_is_the_payload_analysis_scaled_by_the_iteration_difference() {
        assert_eq!(oracle_delta(&LOOP_BACKEDGE, 1_000, 2_000), 1_000);
        assert_eq!(oracle_delta(&BRANCH_DENSE, 1_000, 2_000), 9_000);
        assert_eq!(oracle_delta(&BRANCH_DENSE, 7, 7), 0);
    }

    #[test]
    fn the_offset_is_what_the_smaller_window_counted_beyond_its_own_iterations() {
        assert_eq!(count_offset(&LOOP_BACKEDGE, 1_000, 1_004), 4);
        assert_eq!(count_offset(&BRANCH_DENSE, 1_000, 9_000), 0);
        // An undercount is a negative offset, not a wrapped huge one.
        assert_eq!(count_offset(&LOOP_BACKEDGE, 1_000, 990), -10);
    }

    #[test]
    fn an_arm_runs_enough_iterations_to_cross_its_period() {
        assert_eq!(iterations_for(&LOOP_BACKEDGE, 1_000), 2_000);
        assert_eq!(iterations_for(&BRANCH_DENSE, 900), 200);
        // A period that does not divide evenly rounds up before doubling.
        assert_eq!(iterations_for(&BRANCH_DENSE, 901), 202);
        // A zero period still runs one iteration, rather than arming a payload
        // that never executes.
        assert_eq!(iterations_for(&LOOP_BACKEDGE, 0), 1);
    }

    #[test]
    fn a_window_with_an_interrupt_in_either_half_is_not_clean() {
        assert!(is_clean(0, 0));
        assert!(!is_clean(1, 0));
        assert!(!is_clean(0, 1));
    }

    #[test]
    fn every_skid_lands_in_the_bucket_its_magnitude_names() {
        let mut buckets = SkidBuckets::default();
        for skid in [0, 1, 9, 10, 99, 100, 999, 1_000, 9_999, 10_000, 1_000_000] {
            buckets.observe(skid);
        }
        assert_eq!(buckets.counts, [1, 2, 2, 2, 2, 2]);
        assert_eq!(buckets.max, 1_000_000);
        assert_eq!(buckets.min, Some(0));
        assert_eq!(buckets.total, 11);
    }

    #[test]
    fn an_empty_distribution_reports_no_minimum_rather_than_zero() {
        let buckets = SkidBuckets::default();
        assert_eq!(buckets.min, None);
        assert_eq!(buckets.total, 0);
        assert!(buckets.summary().contains("min=none"));
    }

    #[test]
    fn the_margin_is_twice_the_observed_maximum_and_says_so() {
        let (margin, derivation) = derive_margin(128);
        assert_eq!(margin, 256);
        assert!(derivation.contains("128"), "{derivation}");
        // A distribution with no observed skid derives no headroom, and the
        // sentence still names the number it was derived from.
        let (margin, derivation) = derive_margin(0);
        assert_eq!(margin, 0);
        assert!(derivation.contains('0'), "{derivation}");
    }

    #[test]
    fn a_projection_scales_the_slice_to_the_whole_campaign() {
        let record = projection("overflow", 1_000, 250, 1_000_000);
        match record {
            Record::Projection {
                projected_millis, ..
            } => assert_eq!(projected_millis, 250_000),
            other => panic!("expected a projection, got {other:?}"),
        }
    }

    #[test]
    fn a_cores_interrupt_column_is_summed_across_every_row_that_has_one() {
        let text = "\
           CPU0       CPU1       CPU2
  0:          7          0          0   IO-APIC    2-edge      timer
  9:         11         13         17   IO-APIC    9-fasteoi   acpi
NMI:          1          2          3   Non-maskable interrupts
ERR:          0
";
        assert_eq!(interrupts_for_core(text, 0), 7 + 11 + 1);
        assert_eq!(interrupts_for_core(text, 1), 13 + 2);
        assert_eq!(interrupts_for_core(text, 2), 17 + 3);
        // A core past every row's column count contributes nothing rather than
        // reading a label as a number.
        assert_eq!(interrupts_for_core(text, 9), 0);
    }

    #[test]
    fn a_plans_floors_are_what_it_committed_to_measuring() {
        let plan = MeasurementPlan {
            core: 3,
            n1: 1_000_000,
            n2: 2_000_000,
            reps: 128,
            min_clean_reps: 32,
            periods: vec![10_000, 100_000],
            arms_per_period: 500_000,
            conditions: Interference::all().to_vec(),
        };
        // The floor is the number of judgeable windows, not the number
        // attempted: a host that takes an interrupt in one window has produced
        // a window it cannot judge, which is not an exactness failure.
        assert_eq!(plan.clean_reps_floor(), 32);
        assert_eq!(plan.overflow_floor(1), 1_000_000);
        assert_eq!(plan.overflow_floor(4), 4_000_000);

        let slice = plan.slice(4, 1_000);
        assert_eq!(slice.reps, 4);
        assert_eq!(slice.arms_per_period, 1_000);
        assert_eq!(slice.core, plan.core);
        // A slice that attempts fewer windows than the floor asks for cannot
        // hold the campaign's floor, so it lowers with the attempts.
        assert_eq!(slice.clean_reps_floor(), 4);
        // A slice never asks for more than the campaign it is a slice of.
        let slice = plan.slice(1_000, 10_000_000);
        assert_eq!(slice.reps, 128);
        assert_eq!(slice.clean_reps_floor(), 32);
        assert_eq!(slice.arms_per_period, 500_000);
    }

    #[test]
    fn the_standard_plan_attempts_more_windows_than_it_must_judge() {
        let plan = MeasurementPlan::standard(3);
        assert!(
            plan.reps > plan.min_clean_reps,
            "reps={} floor={}",
            plan.reps,
            plan.min_clean_reps
        );
        assert_eq!(plan.clean_reps_floor(), 32);
    }

    #[test]
    fn the_standard_plan_meets_the_specified_overflow_floor() {
        let plan = MeasurementPlan::standard(3);
        let payloads = crate::payload::runnable().len() as u64;
        assert!(
            plan.overflow_floor(payloads) >= 1_000_000,
            "{}",
            plan.overflow_floor(payloads)
        );
        assert!(plan.n1 < plan.n2);
        assert_eq!(plan.conditions, Interference::all().to_vec());
        assert_eq!(plan.core, 3);
    }

    #[test]
    fn every_interference_condition_has_a_distinct_token() {
        let mut tokens: Vec<&str> = Interference::all().iter().map(|c| c.token()).collect();
        let count = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), count);
        assert_eq!(Interference::all()[0], Interference::Quiet);
    }
}
