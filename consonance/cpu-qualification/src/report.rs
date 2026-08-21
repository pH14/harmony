// SPDX-License-Identifier: AGPL-3.0-or-later
//! The qualification report: the retained raw records of one run, and the
//! recomputation that turns them into a verdict.
//!
//! Recomputation from records is the only path to a pass. A run writes one record
//! per attempt — per exactness repetition, per armed overflow — and the verdict is
//! derived here from those records alone. A run also writes summary records; they
//! are cross-checked against the recomputation and disagreement is a failure, but a
//! summary is never an input to a pass.
//!
//! The floors come from the plan record the run writes before it measures
//! anything, so what the run promised is fixed before it can be influenced by what
//! it saw.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The acceptance floors a run commits to before it measures anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floors {
    /// The fewest interrupt-free exactness repetitions each payload must produce.
    pub min_clean_reps: u64,
    /// The fewest overflows that must be delivered exactly once.
    pub min_overflow_arms: u64,
    /// The largest skid a delivered overflow may show, in work units.
    pub skid_margin: u64,
}

/// One retained record. The `kind` field is the discriminant on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// The run's plan, written before any measurement.
    Plan {
        /// The chip baseline under test.
        baseline: String,
        /// The highest stage this run executes.
        stage: u8,
        /// The floors this run must meet.
        floors: Floors,
    },
    /// What the chip reported about itself.
    ChipIdentity {
        /// The vendor string or implementer.
        vendor: String,
        /// `family_model_stepping` on x86, or the `MIDR_EL1` value on aarch64.
        identity: String,
        /// The microcode or firmware revision the kernel records.
        microcode_rev: Option<String>,
        /// The table entry this chip matched.
        table_entry: String,
    },
    /// One stage-0 expect-versus-found row.
    HostRow {
        /// The condition's token.
        condition: String,
        /// Where the condition was read.
        scope: String,
        /// What the pack says the state must be.
        expect: String,
        /// What was read.
        found: String,
        /// Whether the two agree.
        confirmed: bool,
        /// How a disagreement was dispositioned. A favorable deviation is still
        /// a deviation, so this is `None` until someone records a disposition.
        disposition: Option<String>,
    },
    /// One exactness repetition: two counted windows at scales `n1` and `n2`.
    Exactness {
        /// The payload class.
        payload: String,
        /// The interference condition this repetition ran under.
        condition: String,
        /// The repetition index within its payload and condition.
        rep: u64,
        /// The smaller scale.
        n1: u64,
        /// The larger scale.
        n2: u64,
        /// The count at scale `n1`.
        count_n1: u64,
        /// The count at scale `n2`.
        count_n2: u64,
        /// The analytical count for `n2 - n1` iterations.
        oracle_delta: u64,
        /// The payload's analytical events per iteration.
        events_per_iteration: u64,
        /// Whether the counter was multiplexed during either window.
        multiplexed: bool,
        /// Interrupts delivered to the measurement core during the `n1` window.
        irqs_n1: u64,
        /// Interrupts delivered to the measurement core during the `n2` window.
        irqs_n2: u64,
    },
    /// One armed overflow.
    OverflowArm {
        /// The payload class.
        payload: String,
        /// The arm index within its payload.
        idx: u64,
        /// The period the overflow was armed at.
        period: u64,
        /// How many samples the ring held after the arm.
        samples: u64,
        /// The counter value the sample carries, taken at the interrupt.
        value_at_interrupt: u64,
    },
    /// A run's own overflow tally. Cross-checked, never an input to a pass.
    OverflowSummary {
        /// The payload class.
        payload: String,
        /// How many overflows were armed.
        arms_total: u64,
        /// How many were delivered exactly once.
        delivered_once: u64,
        /// How many were lost.
        lost: u64,
        /// How many were delivered more than once.
        duplicated: u64,
        /// The largest skid seen.
        skid_max: u64,
    },
    /// A duration projection from a short slice, written before a long campaign.
    Projection {
        /// What the projection is for.
        campaign: String,
        /// How many units the slice ran.
        slice_units: u64,
        /// How long the slice took, in milliseconds.
        slice_millis: u64,
        /// How many units the campaign will run.
        total_units: u64,
        /// How long the campaign is projected to take, in milliseconds.
        projected_millis: u64,
    },
    /// The terminal record. Exactly one per run.
    End {
        /// The highest stage the run executed.
        stage: u8,
        /// The run's own exit code. A run that ended nonzero cannot pass here.
        rc: i32,
    },
}

/// One recomputed check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it passed.
    pub passed: bool,
    /// What the recomputation found.
    pub detail: String,
}

impl Check {
    /// A check with its outcome and what the recomputation found.
    fn new(name: impl Into<String>, passed: bool, detail: impl Into<String>) -> Check {
        Check {
            name: name.into(),
            passed,
            detail: detail.into(),
        }
    }
}

/// The recomputed verdict of one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// The baseline the plan named, when a plan was retained.
    pub baseline: Option<String>,
    /// The floors the plan committed to, when a plan was retained.
    pub floors: Option<Floors>,
    /// Every recomputed check, in a stable order.
    pub checks: Vec<Check>,
    /// Whether every check passed.
    pub passed: bool,
}

/// Recompute every floor from the retained records.
///
/// Records may arrive in any order and from any number of files; the
/// recomputation groups them itself. A missing plan, a missing terminal record, a
/// nonzero terminal code, or an empty record set all fail — an absent measurement
/// is never a pass.
#[must_use]
pub fn recompute(records: &[Record]) -> Verdict {
    let mut checks = Vec::new();

    let plan = records.iter().find_map(|r| match r {
        Record::Plan {
            baseline, floors, ..
        } => Some((baseline.clone(), *floors)),
        _ => None,
    });
    let plan_count = records
        .iter()
        .filter(|r| matches!(r, Record::Plan { .. }))
        .count();
    checks.push(Check::new(
        "plan",
        plan_count == 1,
        format!("{plan_count} plan record(s); exactly one is required"),
    ));

    check_terminal(records, &mut checks);
    check_host_rows(records, &mut checks);

    let floors = plan.as_ref().map(|(_, f)| *f);
    match floors {
        Some(floors) => {
            check_exactness(records, floors, &mut checks);
            check_interference(records, &mut checks);
            check_overflow(records, floors, &mut checks);
        }
        None => checks.push(Check::new(
            "floors",
            false,
            "no plan record, so no floors to recompute against",
        )),
    }

    let passed = !checks.is_empty() && checks.iter().all(|c| c.passed);
    Verdict {
        baseline: plan.map(|(b, _)| b),
        floors,
        checks,
        passed,
    }
}

/// Exactly one terminal record, and it must report success.
fn check_terminal(records: &[Record], checks: &mut Vec<Check>) {
    let ends: Vec<i32> = records
        .iter()
        .filter_map(|r| match r {
            Record::End { rc, .. } => Some(*rc),
            _ => None,
        })
        .collect();
    let ok = ends.len() == 1 && ends[0] == 0;
    checks.push(Check::new(
        "terminal",
        ok,
        format!(
            "{} terminal record(s), codes {ends:?}; exactly one with code 0 is required",
            ends.len()
        ),
    ));
}

/// Every stage-0 row is either confirmed or carries a disposition.
fn check_host_rows(records: &[Record], checks: &mut Vec<Check>) {
    let rows: Vec<(&String, bool, bool)> = records
        .iter()
        .filter_map(|r| match r {
            Record::HostRow {
                condition,
                confirmed,
                disposition,
                ..
            } => Some((condition, *confirmed, disposition.is_some())),
            _ => None,
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    let undispositioned: Vec<&str> = rows
        .iter()
        .filter(|(_, confirmed, has_disposition)| !confirmed && !has_disposition)
        .map(|(c, _, _)| c.as_str())
        .collect();
    checks.push(Check::new(
        "host-rows",
        undispositioned.is_empty(),
        format!(
            "{} row(s); {} deviation(s) without a disposition: {undispositioned:?}",
            rows.len(),
            undispositioned.len()
        ),
    ));
}

/// A payload's exactness repetitions, grouped for recomputation.
struct ExactnessGroup<'a> {
    reps: Vec<u64>,
    clean: Vec<&'a Record>,
    multiplexed: u64,
}

/// Whether a repetition's two windows were free of interrupts.
fn is_clean(irqs_n1: u64, irqs_n2: u64) -> bool {
    irqs_n1 == 0 && irqs_n2 == 0
}

/// Count exactness, recomputed on interrupt-free windows only.
fn check_exactness(records: &[Record], floors: Floors, checks: &mut Vec<Check>) {
    let mut groups: BTreeMap<(String, String), ExactnessGroup> = BTreeMap::new();
    for record in records {
        if let Record::Exactness {
            payload,
            condition,
            rep,
            irqs_n1,
            irqs_n2,
            multiplexed,
            ..
        } = record
        {
            let group = groups
                .entry((payload.clone(), condition.clone()))
                .or_insert_with(|| ExactnessGroup {
                    reps: Vec::new(),
                    clean: Vec::new(),
                    multiplexed: 0,
                });
            group.reps.push(*rep);
            if is_clean(*irqs_n1, *irqs_n2) {
                group.clean.push(record);
            }
            if *multiplexed {
                group.multiplexed += 1;
            }
        }
    }
    if groups.is_empty() {
        // A floor the plan committed to must be recomputed even when the run
        // retained nothing to recompute from: silence is not a pass.
        if floors.min_clean_reps > 0 {
            checks.push(Check::new(
                "exactness",
                false,
                format!(
                    "no exactness records, but the plan committed to {} clean repetition(s) \
                     per payload",
                    floors.min_clean_reps
                ),
            ));
        }
        return;
    }

    for ((payload, condition), mut group) in groups {
        group.reps.sort_unstable();
        let contiguous: Vec<u64> = (0..group.reps.len() as u64).collect();
        let all_present = group.reps == contiguous;

        let mut mismatches = 0u64;
        let mut offsets: BTreeSet<i128> = BTreeSet::new();
        for record in &group.clean {
            if let Record::Exactness {
                n1,
                count_n1,
                count_n2,
                oracle_delta,
                events_per_iteration,
                ..
            } = record
            {
                if count_n2.saturating_sub(*count_n1) != *oracle_delta {
                    mismatches += 1;
                }
                // The fixed prologue contribution: what the count carries beyond
                // the analytical per-iteration total. It must not move between
                // repetitions of one class.
                let expected = i128::from(*events_per_iteration) * i128::from(*n1);
                offsets.insert(i128::from(*count_n1) - expected);
            }
        }
        let enough = group.clean.len() as u64 >= floors.min_clean_reps;
        let stable = offsets.len() <= 1;
        let passed = all_present && enough && stable && mismatches == 0 && group.multiplexed == 0;
        checks.push(Check::new(
            format!("exactness[{payload}/{condition}]"),
            passed,
            format!(
                "reps={} contiguous={all_present} clean={} floor={} mismatches={mismatches} \
                 multiplexed={} offset_stable={stable}",
                group.reps.len(),
                group.clean.len(),
                floors.min_clean_reps,
                group.multiplexed
            ),
        ));
    }
}

/// Interference probes: a payload's clean count must not move between conditions.
fn check_interference(records: &[Record], checks: &mut Vec<Check>) {
    let mut deltas: BTreeMap<String, BTreeMap<String, BTreeSet<u64>>> = BTreeMap::new();
    for record in records {
        if let Record::Exactness {
            payload,
            condition,
            count_n1,
            count_n2,
            irqs_n1,
            irqs_n2,
            ..
        } = record
            && is_clean(*irqs_n1, *irqs_n2)
        {
            deltas
                .entry(payload.clone())
                .or_default()
                .entry(condition.clone())
                .or_default()
                .insert(count_n2.saturating_sub(*count_n1));
        }
    }
    for (payload, by_condition) in deltas {
        if by_condition.len() < 2 {
            continue;
        }
        let all: BTreeSet<u64> = by_condition.values().flatten().copied().collect();
        let conditions: Vec<&str> = by_condition.keys().map(String::as_str).collect();
        checks.push(Check::new(
            format!("interference[{payload}]"),
            all.len() == 1,
            format!(
                "conditions {conditions:?} produced clean deltas {all:?}; \
                 a count that moves under interference is a failure"
            ),
        ));
    }
}

/// Overflow delivery and skid, recomputed from the per-arm records.
fn check_overflow(records: &[Record], floors: Floors, checks: &mut Vec<Check>) {
    let mut arms: BTreeMap<String, Vec<(u64, u64, u64, u64)>> = BTreeMap::new();
    for record in records {
        if let Record::OverflowArm {
            payload,
            idx,
            period,
            samples,
            value_at_interrupt,
        } = record
        {
            arms.entry(payload.clone()).or_default().push((
                *idx,
                *period,
                *samples,
                *value_at_interrupt,
            ));
        }
    }
    let mut delivered_total = 0u64;
    let measured_payloads: BTreeSet<String> = arms.keys().cloned().collect();
    for (payload, mut rows) in arms {
        rows.sort_unstable();
        let indices: Vec<u64> = rows.iter().map(|(idx, ..)| *idx).collect();
        let contiguous: Vec<u64> = (0..rows.len() as u64).collect();
        let all_present = indices == contiguous;

        let mut lost = 0u64;
        let mut duplicated = 0u64;
        let mut premature = 0u64;
        let mut delivered = 0u64;
        let mut over_margin = 0u64;
        let mut skid_max = 0u64;
        for (_, period, samples, value) in &rows {
            match samples {
                0 => lost += 1,
                1 => {
                    if value < period {
                        premature += 1;
                    } else {
                        delivered += 1;
                        let skid = value - period;
                        skid_max = skid_max.max(skid);
                        if skid > floors.skid_margin {
                            over_margin += 1;
                        }
                    }
                }
                _ => duplicated += 1,
            }
        }
        delivered_total += delivered;
        let passed = all_present
            && lost == 0
            && duplicated == 0
            && premature == 0
            && over_margin == 0
            && delivered == rows.len() as u64;
        checks.push(Check::new(
            format!("overflow[{payload}]"),
            passed,
            format!(
                "arms={} contiguous={all_present} delivered_once={delivered} lost={lost} \
                 duplicated={duplicated} premature={premature} skid_max={skid_max} \
                 margin={} over_margin={over_margin}",
                rows.len(),
                floors.skid_margin
            ),
        ));

        cross_check_summary(
            records,
            &payload,
            rows.len() as u64,
            delivered,
            lost,
            duplicated,
            skid_max,
            checks,
        );
    }

    // A summary for a payload with no retained arms has nothing behind it. It
    // cannot be cross-checked, so it cannot contribute to a pass.
    for record in records {
        if let Record::OverflowSummary { payload, .. } = record
            && !measured_payloads.contains(payload)
        {
            checks.push(Check::new(
                format!("overflow-summary-agrees[{payload}]"),
                false,
                "a summary with no retained per-arm records cannot be recomputed".to_string(),
            ));
        }
    }

    checks.push(Check::new(
        "overflow-volume",
        delivered_total >= floors.min_overflow_arms,
        format!(
            "{delivered_total} overflow(s) delivered exactly once, recomputed from per-arm \
             records; floor is {}",
            floors.min_overflow_arms
        ),
    ));
}

/// A retained summary must agree with the recomputation. It is never an input to
/// a pass; a disagreement means the run's own accounting was wrong.
#[allow(clippy::too_many_arguments)]
fn cross_check_summary(
    records: &[Record],
    payload: &str,
    arms: u64,
    delivered: u64,
    lost: u64,
    duplicated: u64,
    skid_max: u64,
    checks: &mut Vec<Check>,
) {
    let Some(summary) = records
        .iter()
        .find(|r| matches!(r, Record::OverflowSummary { payload: p, .. } if p == payload))
    else {
        return;
    };
    let Record::OverflowSummary {
        arms_total,
        delivered_once,
        lost: s_lost,
        duplicated: s_duplicated,
        skid_max: s_skid_max,
        ..
    } = summary
    else {
        return;
    };
    let agrees = *arms_total == arms
        && *delivered_once == delivered
        && *s_lost == lost
        && *s_duplicated == duplicated
        && *s_skid_max == skid_max;
    checks.push(Check::new(
        format!("overflow-summary-agrees[{payload}]"),
        agrees,
        format!(
            "summary (arms={arms_total} delivered={delivered_once} lost={s_lost} \
             duplicated={s_duplicated} skid_max={s_skid_max}) versus recomputation \
             (arms={arms} delivered={delivered} lost={lost} duplicated={duplicated} \
             skid_max={skid_max})"
        ),
    ));
}

/// Parse a record stream: one JSON object per non-empty line.
///
/// # Errors
/// The line number and the parse error of the first line that is not a record.
pub fn parse_records(text: &str) -> Result<Vec<Record>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: Record =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        out.push(record);
    }
    Ok(out)
}

/// Serialize one record as a line of a record stream.
///
/// # Errors
/// The serialization error, which a well-formed record cannot produce.
pub fn record_line(record: &Record) -> Result<String, serde_json::Error> {
    serde_json::to_string(record)
}

/// Project a campaign's duration from a measured slice.
///
/// Returns zero when the slice measured no units, so a projection from nothing
/// reads as nothing rather than as a division failure.
#[must_use]
pub fn project_millis(slice_units: u64, slice_millis: u64, total_units: u64) -> u64 {
    if slice_units == 0 {
        return 0;
    }
    let scaled = u128::from(slice_millis) * u128::from(total_units) / u128::from(slice_units);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOORS: Floors = Floors {
        min_clean_reps: 2,
        min_overflow_arms: 3,
        skid_margin: 100,
    };

    fn plan() -> Record {
        Record::Plan {
            baseline: "sample-v1".to_string(),
            stage: 1,
            floors: FLOORS,
        }
    }

    fn end(rc: i32) -> Record {
        Record::End { stage: 1, rc }
    }

    fn exact(payload: &str, condition: &str, rep: u64, count_n2: u64, irqs: u64) -> Record {
        Record::Exactness {
            payload: payload.to_string(),
            condition: condition.to_string(),
            rep,
            n1: 1000,
            n2: 2000,
            count_n1: 1000,
            count_n2,
            oracle_delta: 1000,
            events_per_iteration: 1,
            multiplexed: false,
            irqs_n1: irqs,
            irqs_n2: 0,
        }
    }

    fn arm(idx: u64, samples: u64, value: u64) -> Record {
        Record::OverflowArm {
            payload: "loop_backedge".to_string(),
            idx,
            period: 1000,
            samples,
            value_at_interrupt: value,
        }
    }

    fn passing_run() -> Vec<Record> {
        vec![
            plan(),
            exact("loop_backedge", "pinned-solo", 0, 2000, 0),
            exact("loop_backedge", "pinned-solo", 1, 2000, 0),
            arm(0, 1, 1000),
            arm(1, 1, 1050),
            arm(2, 1, 1100),
            end(0),
        ]
    }

    #[test]
    fn a_complete_run_passes_every_recomputed_check() {
        let verdict = recompute(&passing_run());
        assert!(verdict.passed, "{:#?}", verdict.checks);
        assert_eq!(verdict.baseline.as_deref(), Some("sample-v1"));
        assert_eq!(verdict.floors, Some(FLOORS));
    }

    #[test]
    fn an_empty_record_set_never_passes() {
        let verdict = recompute(&[]);
        assert!(!verdict.passed);
        assert!(verdict.checks.iter().any(|c| c.name == "plan" && !c.passed));
        assert!(
            verdict
                .checks
                .iter()
                .any(|c| c.name == "floors" && !c.passed),
            "no plan means no floors to recompute against"
        );
    }

    #[test]
    fn a_run_that_ended_nonzero_cannot_pass() {
        let mut records = passing_run();
        records.retain(|r| !matches!(r, Record::End { .. }));
        records.push(end(1));
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        assert!(
            verdict
                .checks
                .iter()
                .any(|c| c.name == "terminal" && !c.passed)
        );
    }

    #[test]
    fn a_missing_terminal_record_cannot_pass() {
        let mut records = passing_run();
        records.retain(|r| !matches!(r, Record::End { .. }));
        assert!(!recompute(&records).passed);
    }

    #[test]
    fn two_plan_records_cannot_pass() {
        let mut records = passing_run();
        records.insert(0, plan());
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        assert!(verdict.checks.iter().any(|c| c.name == "plan" && !c.passed));
    }

    #[test]
    fn exactness_is_recomputed_from_the_counts_not_from_a_verdict_field() {
        let mut records = passing_run();
        // A clean window whose counts do not satisfy the analytical oracle.
        records.push(exact("loop_backedge", "pinned-solo", 2, 2001, 0));
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name == "exactness[loop_backedge/pinned-solo]")
            .expect("the payload is checked");
        assert!(check.detail.contains("mismatches=1"), "{}", check.detail);
    }

    #[test]
    fn a_contaminated_window_is_accounted_but_not_held_to_the_oracle() {
        let mut records = passing_run();
        // Inexact, but an interrupt landed in the window: accounted, not a failure.
        records.push(exact("loop_backedge", "pinned-solo", 2, 2500, 1));
        let verdict = recompute(&records);
        assert!(verdict.passed, "{:#?}", verdict.checks);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name == "exactness[loop_backedge/pinned-solo]")
            .expect("the payload is checked");
        assert!(check.detail.contains("reps=3"), "{}", check.detail);
        assert!(check.detail.contains("clean=2"), "{}", check.detail);
    }

    #[test]
    fn an_all_contaminated_run_fails_the_clean_repetition_floor() {
        let records = vec![
            plan(),
            exact("loop_backedge", "pinned-solo", 0, 2000, 1),
            exact("loop_backedge", "pinned-solo", 1, 2000, 1),
            end(0),
        ];
        let verdict = recompute(&records);
        assert!(
            !verdict.passed,
            "a vacuous all-contaminated run must not pass"
        );
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name.starts_with("exactness["))
            .expect("the payload is checked");
        assert!(check.detail.contains("clean=0"), "{}", check.detail);
    }

    #[test]
    fn a_missing_repetition_fails_the_totality_check() {
        let records = vec![
            plan(),
            exact("loop_backedge", "pinned-solo", 0, 2000, 0),
            exact("loop_backedge", "pinned-solo", 2, 2000, 0),
            end(0),
        ];
        let verdict = recompute(&records);
        assert!(
            !verdict.passed,
            "a gap in the repetitions is unaccounted work"
        );
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name.starts_with("exactness["))
            .expect("the payload is checked");
        assert!(
            check.detail.contains("contiguous=false"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_multiplexed_window_fails() {
        let mut records = passing_run();
        records.push(Record::Exactness {
            payload: "loop_backedge".to_string(),
            condition: "pinned-solo".to_string(),
            rep: 2,
            n1: 1000,
            n2: 2000,
            count_n1: 1000,
            count_n2: 2000,
            oracle_delta: 1000,
            events_per_iteration: 1,
            multiplexed: true,
            irqs_n1: 0,
            irqs_n2: 0,
        });
        assert!(!recompute(&records).passed);
    }

    #[test]
    fn a_moving_prologue_offset_fails() {
        let mut records = passing_run();
        records.push(Record::Exactness {
            payload: "loop_backedge".to_string(),
            condition: "pinned-solo".to_string(),
            rep: 2,
            n1: 1000,
            // Both windows shifted by the same amount: the delta is still exact,
            // but the fixed prologue contribution moved.
            count_n1: 1007,
            count_n2: 2007,
            n2: 2000,
            oracle_delta: 1000,
            events_per_iteration: 1,
            multiplexed: false,
            irqs_n1: 0,
            irqs_n2: 0,
        });
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name.starts_with("exactness["))
            .expect("the payload is checked");
        assert!(
            check.detail.contains("offset_stable=false"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_count_that_moves_under_interference_fails() {
        let mut records = passing_run();
        for rep in 0..2 {
            records.push(exact("loop_backedge", "co-tenant", rep, 2000, 0));
        }
        assert!(recompute(&records).passed, "identical counts pass");

        let mut moved = passing_run();
        moved.push(exact("loop_backedge", "co-tenant", 0, 2001, 0));
        moved.push(exact("loop_backedge", "co-tenant", 1, 2001, 0));
        let verdict = recompute(&moved);
        assert!(!verdict.passed);
        assert!(
            verdict
                .checks
                .iter()
                .any(|c| c.name == "interference[loop_backedge]" && !c.passed)
        );
    }

    #[test]
    fn a_lost_overflow_fails_and_a_duplicate_overflow_fails() {
        for (samples, marker) in [(0u64, "lost=1"), (2, "duplicated=1")] {
            let mut records = passing_run();
            records.push(arm(3, samples, 1000));
            let verdict = recompute(&records);
            assert!(!verdict.passed, "samples={samples}");
            let check = verdict
                .checks
                .iter()
                .find(|c| c.name.starts_with("overflow["))
                .expect("the payload is checked");
            assert!(check.detail.contains(marker), "{}", check.detail);
        }
    }

    #[test]
    fn a_skid_past_the_margin_fails() {
        let mut records = passing_run();
        records.push(arm(3, 1, 1000 + FLOORS.skid_margin + 1));
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name.starts_with("overflow["))
            .expect("the payload is checked");
        assert!(check.detail.contains("over_margin=1"), "{}", check.detail);
        // Exactly at the margin is within it.
        let mut at_bound = passing_run();
        at_bound.push(arm(3, 1, 1000 + FLOORS.skid_margin));
        assert!(recompute(&at_bound).passed);
    }

    #[test]
    fn an_overflow_below_its_period_is_a_premature_delivery() {
        let mut records = passing_run();
        records.push(arm(3, 1, 999));
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name.starts_with("overflow["))
            .expect("the payload is checked");
        assert!(check.detail.contains("premature=1"), "{}", check.detail);
    }

    #[test]
    fn too_few_delivered_overflows_fails_the_volume_floor() {
        let records = vec![plan(), arm(0, 1, 1000), arm(1, 1, 1000), end(0)];
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name == "overflow-volume")
            .expect("volume is checked");
        assert!(check.detail.contains("floor is 3"), "{}", check.detail);
    }

    #[test]
    fn a_summary_that_disagrees_with_the_records_fails() {
        // The per-arm records carry three deliveries with a largest skid of 100.
        let mut records = passing_run();
        records.push(Record::OverflowSummary {
            payload: "loop_backedge".to_string(),
            arms_total: 3,
            delivered_once: 3,
            lost: 0,
            duplicated: 0,
            skid_max: 4,
        });
        let verdict = recompute(&records);
        assert!(!verdict.passed);
        let check = verdict
            .checks
            .iter()
            .find(|c| c.name == "overflow-summary-agrees[loop_backedge]")
            .expect("the summary is cross-checked");
        assert!(!check.passed, "{}", check.detail);
        assert!(check.detail.contains("skid_max=100"), "{}", check.detail);
    }

    #[test]
    fn a_summary_that_matches_the_records_passes_but_is_not_what_grants_the_pass() {
        let mut records = passing_run();
        records.push(Record::OverflowSummary {
            payload: "loop_backedge".to_string(),
            arms_total: 3,
            delivered_once: 3,
            lost: 0,
            duplicated: 0,
            skid_max: 100,
        });
        // The per-arm records already carry a maximum skid of 100.
        let verdict = recompute(&records);
        assert!(verdict.passed, "{:#?}", verdict.checks);

        // With the per-arm records removed, an agreeing summary alone proves
        // nothing: the volume floor is recomputed from arms, and there are none.
        let summary_only = vec![
            plan(),
            Record::OverflowSummary {
                payload: "loop_backedge".to_string(),
                arms_total: 1_000_000,
                delivered_once: 1_000_000,
                lost: 0,
                duplicated: 0,
                skid_max: 1,
            },
            end(0),
        ];
        let verdict = recompute(&summary_only);
        assert!(
            !verdict.passed,
            "a summary line must never be the thing that grants a pass"
        );
        let volume = verdict
            .checks
            .iter()
            .find(|c| c.name == "overflow-volume")
            .expect("the volume floor is recomputed even with no arms");
        assert!(volume.detail.starts_with("0 overflow"), "{}", volume.detail);
    }

    #[test]
    fn a_run_that_retained_no_measurements_fails_every_committed_floor() {
        let verdict = recompute(&[plan(), end(0)]);
        assert!(!verdict.passed, "silence is not a pass");
        for name in ["exactness", "overflow-volume"] {
            let check = verdict
                .checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} must be recomputed"));
            assert!(!check.passed, "{}: {}", check.name, check.detail);
        }
    }

    #[test]
    fn an_undispositioned_host_deviation_fails_and_a_dispositioned_one_does_not() {
        let mut records = passing_run();
        records.push(Record::HostRow {
            condition: "nmi-watchdog-off".to_string(),
            scope: "host".to_string(),
            expect: "0".to_string(),
            found: "1".to_string(),
            confirmed: false,
            disposition: None,
        });
        assert!(!recompute(&records).passed);

        let mut dispositioned = passing_run();
        dispositioned.push(Record::HostRow {
            condition: "nmi-watchdog-off".to_string(),
            scope: "host".to_string(),
            expect: "0".to_string(),
            found: "1".to_string(),
            confirmed: false,
            disposition: Some("accepted for this run".to_string()),
        });
        assert!(recompute(&dispositioned).passed);
    }

    #[test]
    fn records_round_trip_through_the_line_format() {
        let records = passing_run();
        let text: String = records
            .iter()
            .map(|r| record_line(r).expect("serializes") + "\n")
            .collect();
        assert_eq!(parse_records(&text).expect("parses"), records);
        // Blank lines are ignored; a malformed line names its number.
        assert_eq!(parse_records("\n\n").expect("parses"), Vec::new());
        let err = parse_records("{}\n").expect_err("an empty object is not a record");
        assert!(err.starts_with("line 1:"), "{err}");
    }

    #[test]
    fn a_projection_scales_the_slice_rate_and_survives_a_zero_slice() {
        assert_eq!(project_millis(100, 50, 1000), 500);
        assert_eq!(project_millis(1, 1, 1), 1);
        assert_eq!(project_millis(0, 50, 1000), 0);
        assert_eq!(project_millis(1, u64::MAX, u64::MAX), u64::MAX);
    }
}
