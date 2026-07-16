// SPDX-License-Identifier: AGPL-3.0-or-later
//! # The campaign stopwatch (task 96): hash-neutral phase timing
//!
//! Production box runs are wall-clock-blind: nothing in the campaign-runner emits
//! how long a phase took, so an hours-long campaign's cost decomposition (how
//! much of it is `branch`, `run`, `hash`, harvesting SDK events, …) could only
//! be inferred after the fact. This module is the **sanctioned escape hatch**
//! for `Instant::now` (`clippy.toml`'s determinism ban, `tasks/00-CONVENTIONS.md`
//! rule 4): host-side observation that is read into a
//! [`CampaignReport`](crate::campaign::CampaignReport) and log lines, and
//! **nowhere else** — the same category as the task-87 film projector
//! ("hash-neutral by construction") and the live-harness watchdogs
//! (`consonance/vmm-core/tests/seal_rate_sweep.rs::watchdog_start`).
//!
//! Every `Instant::now` read anywhere in this crate lives in this one file,
//! under a single file-level `#[allow(clippy::disallowed_methods)]` (below).
//! Durations land only in [`Stopwatch::stats`]'s [`PhaseStats`] (which the
//! campaign driver copies into the report) and printed progress lines;
//! nothing in the search loop branches on a duration — the stopwatch
//! **records, it never decides**.
//!
//! All timing values are integer microseconds (`u64`); a float appears only
//! in a formatted print (e.g. branches/hour, one decimal) — never stored.
#![allow(clippy::disallowed_methods)]
// not order-observable: every `Instant::now` read in this file lands only in
// `CampaignReport.timing`/`wall_secs` (via `Stopwatch::stats`/`elapsed_secs`,
// or `boxrun.rs`'s single boot-to-ready `Mark`) and printed log lines — never
// in `state_hash`, an `Reproducer`/reproducer, or the runtrace journal — and
// nothing in the search loop branches on a duration (task 96,
// `tasks/96-campaign-stopwatch.md`).

use std::collections::BTreeMap;
use std::time::Instant;

/// One phase's accumulated observations, in microseconds. Every field is an
/// exact integer function of the recorded samples (nearest-rank percentiles,
/// no interpolation) — deterministic given the same samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseStats {
    /// How many observations were folded in.
    pub count: u64,
    /// Sum of every observation, in microseconds.
    pub total_us: u64,
    /// The 50th-percentile observation: `sorted[(count-1) * 50 / 100]`
    /// (nearest-rank, no interpolation).
    pub p50_us: u64,
    /// The 90th-percentile observation (nearest-rank).
    pub p90_us: u64,
    /// The largest observation.
    pub max_us: u64,
}

impl PhaseStats {
    /// A stats summary for exactly one observation. Used by `boxrun.rs` to
    /// seed the `Boot` phase from its single boot-to-ready measurement: the
    /// boot happens before a campaign's [`Stopwatch`] exists (it brackets the
    /// `boot_server` call, well outside `run_campaign`), so it cannot go
    /// through [`Stopwatch::time`] — this constructs the equivalent
    /// single-sample [`PhaseStats`] directly (nearest-rank of one sample is
    /// that sample, at every percentile).
    pub fn single(us: u64) -> Self {
        Self {
            count: 1,
            total_us: us,
            p50_us: us,
            p90_us: us,
            max_us: us,
        }
    }
}

/// A campaign phase the stopwatch can time.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum Phase {
    /// Boot-to-ready (`boxrun.rs`'s live-guest boot, box mode only).
    Boot,
    /// The base-seal retry loop (`snapshot()` + any non-quiescent retries).
    BaseSeal,
    /// `Machine::branch`.
    Branch,
    /// `Machine::run`.
    Run,
    /// `Machine::hash`.
    Hash,
    /// The SDK event round-trip (`sdk_events` + decode).
    Harvest,
    /// `Oracle::judge`.
    Judge,
    /// One verify-replay iteration (`branch` + `run` + `hash` together).
    Replay,
    /// The nominal-control pass.
    Nominal,
}

impl Phase {
    /// The stable snake_case name for this phase — the string a future serde
    /// derive on [`crate::campaign::CampaignReport`] would emit. This crate's
    /// task keeps `serde` off the dependency list (no new dependencies), so
    /// nothing here actually serializes; this is the name a JSON encoder
    /// would use once one is wired up, kept stable from day one.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Boot => "boot",
            Phase::BaseSeal => "base_seal",
            Phase::Branch => "branch",
            Phase::Run => "run",
            Phase::Hash => "hash",
            Phase::Harvest => "harvest",
            Phase::Judge => "judge",
            Phase::Replay => "replay",
            Phase::Nominal => "nominal",
        }
    }
}

/// A single wall-clock mark, opaque outside this module — confines every
/// literal `Instant::now()` read in the crate to `stopwatch.rs` (the
/// hash-neutrality invariant) while still letting other campaign-runner modules
/// (`boxrun.rs`'s boot-to-ready timer, which runs before a campaign's
/// [`Stopwatch`] exists) measure a wall-clock span.
#[derive(Clone, Copy, Debug)]
pub struct Mark(Instant);

/// Take a wall-clock mark now.
pub fn mark() -> Mark {
    Mark(Instant::now())
}

impl Mark {
    /// Microseconds elapsed since this mark was taken.
    pub fn elapsed_us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// Records per-phase durations during one campaign. Observation-only (see the
/// module doc): [`Stopwatch::time`] never influences its closure's control
/// flow or return value, and nothing reads a duration to make a decision.
pub struct Stopwatch {
    t0: Instant,
    samples: BTreeMap<Phase, Vec<u64>>,
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Stopwatch {
    /// Start a new stopwatch; [`Stopwatch::elapsed_secs`] is measured from
    /// this call.
    pub fn new() -> Self {
        Self {
            t0: Instant::now(),
            samples: BTreeMap::new(),
        }
    }

    /// Time one closure under `phase`, recording its wall-clock duration and
    /// returning the closure's result unchanged — a pure passthrough, so
    /// wrapping a call in `time` cannot change what it returns or whether it
    /// errors.
    pub fn time<T>(&mut self, phase: Phase, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        let us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.samples.entry(phase).or_default().push(us);
        out
    }

    /// Seconds since [`Stopwatch::new`] — cheap enough to call from a
    /// progress line every few iterations.
    pub fn elapsed_secs(&self) -> u64 {
        self.t0.elapsed().as_secs()
    }

    /// Fold every phase's samples into [`PhaseStats`] (nearest-rank
    /// percentiles, integer math — see [`PhaseStats`]). A phase with zero
    /// samples is omitted.
    pub fn stats(&self) -> BTreeMap<Phase, PhaseStats> {
        self.samples
            .iter()
            .filter_map(|(&phase, samples)| {
                if samples.is_empty() {
                    return None;
                }
                let mut sorted = samples.clone();
                sorted.sort_unstable();
                let count = sorted.len() as u64;
                let total_us: u64 = sorted.iter().sum();
                let rank = |p: u64| sorted[((count - 1) * p / 100) as usize];
                Some((
                    phase,
                    PhaseStats {
                        count,
                        total_us,
                        p50_us: rank(50),
                        p90_us: rank(90),
                        max_us: *sorted.last().expect("checked non-empty above"),
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a [`Stopwatch`] with `samples` injected directly under `phase`
    /// (bypassing `time`'s real `Instant` read — the only untestable line in
    /// this module) and fold it, returning that phase's [`PhaseStats`].
    fn stats_of(phase: Phase, samples: &[u64]) -> PhaseStats {
        let mut sw = Stopwatch::new();
        sw.samples.insert(phase, samples.to_vec());
        sw.stats().remove(&phase).expect("phase has samples")
    }

    /// Nearest-rank percentiles on a fixed 10-sample vec, checked against a
    /// hand-computed index (`sorted[(count-1)*p/100]`).
    #[test]
    fn nearest_rank_percentiles_on_a_fixed_sample_vec() {
        let s = stats_of(Phase::Run, &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(s.count, 10);
        assert_eq!(s.total_us, 550);
        // sorted[(10-1)*50/100] = sorted[4] = 50
        assert_eq!(s.p50_us, 50);
        // sorted[(10-1)*90/100] = sorted[8] = 90
        assert_eq!(s.p90_us, 90);
        assert_eq!(s.max_us, 100);
    }

    /// The `count == 1` edge: every percentile and the max collapse to the
    /// single observation.
    #[test]
    fn nearest_rank_percentiles_at_count_one() {
        let s = stats_of(Phase::Hash, &[42]);
        assert_eq!(s.count, 1);
        assert_eq!(s.total_us, 42);
        assert_eq!(s.p50_us, 42);
        assert_eq!(s.p90_us, 42);
        assert_eq!(s.max_us, 42);
    }

    /// The `count == 2` edge: nearest-rank with integer floor division picks
    /// index 0 for both p50 and p90 (`(2-1)*50/100 == (2-1)*90/100 == 0`).
    #[test]
    fn nearest_rank_percentiles_at_count_two() {
        let s = stats_of(Phase::Branch, &[30, 10]); // unsorted input on purpose
        assert_eq!(s.count, 2);
        assert_eq!(s.total_us, 40);
        assert_eq!(s.p50_us, 10);
        assert_eq!(s.p90_us, 10);
        assert_eq!(s.max_us, 30);
    }

    /// A phase with zero recorded samples never appears in `stats()`.
    #[test]
    fn a_zero_sample_phase_is_omitted_from_stats() {
        let sw = Stopwatch::new();
        assert!(sw.stats().is_empty());

        let mut sw = Stopwatch::new();
        sw.samples.insert(Phase::Judge, Vec::new());
        assert!(
            sw.stats().is_empty(),
            "an explicitly-empty sample vec is still omitted"
        );
    }

    /// `time` is a pure passthrough of the closure's return value, and
    /// records exactly one sample under the given phase.
    #[test]
    fn time_passes_through_the_closures_return_value_and_records_one_sample() {
        let mut sw = Stopwatch::new();
        let v = sw.time(Phase::Run, || 7 + 35);
        assert_eq!(v, 42);
        let stats = sw.stats();
        let s = stats.get(&Phase::Run).expect("one sample recorded");
        assert_eq!(s.count, 1);
        assert_eq!(s.p50_us, s.max_us, "a single sample: every stat collapses");
    }

    /// `PhaseStats::single` matches what `Stopwatch::stats` would produce for
    /// a real one-sample phase (the `Boot`-phase shortcut's contract).
    #[test]
    fn phase_stats_single_matches_a_real_one_sample_fold() {
        assert_eq!(PhaseStats::single(999), stats_of(Phase::Boot, &[999]));
    }

    /// Every `Phase` variant's `as_str` is a distinct, stable snake_case
    /// token — pinned so a future rename is a deliberate, reviewed diff.
    #[test]
    fn phase_as_str_is_stable_and_distinct() {
        let all = [
            Phase::Boot,
            Phase::BaseSeal,
            Phase::Branch,
            Phase::Run,
            Phase::Hash,
            Phase::Harvest,
            Phase::Judge,
            Phase::Replay,
            Phase::Nominal,
        ];
        let names: Vec<&str> = all.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "boot",
                "base_seal",
                "branch",
                "run",
                "hash",
                "harvest",
                "judge",
                "replay",
                "nominal",
            ]
        );
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "every name is distinct");
    }
}
