// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 63 — the **pure-logic half** of the arbitrary-V-time seal-rate measurement
//! (the Wave-5 go/no-go). This module owns the two portable, macOS+Linux-testable
//! surfaces the task's Environment section names:
//!
//! 1. **The V-time sampling schedule** ([`SamplingSchedule`]) — choose `N ≥ 64` target
//!    V-time points spread across the post-readiness run (uniform in retired count,
//!    plus a handful landing deliberately inside known-busy windows), and its
//!    **adversarial jittered** variant ([`SamplingSchedule::jittered`]).
//! 2. **The seal-rate / `sealable`-predicate bookkeeping** — record per-target seal
//!    success/failure and the reason ([`SealAttempt`]), roll it up into rates
//!    ([`SealStats`]), addressability ([`Overshoot`]), the materialization-depth ratio
//!    ([`MaterializationDepth`]), and the Phase-A2 predicate ([`sealable`]) with its
//!    measured precision/recall ([`PredicateQuality`]) — and finally reduce the whole
//!    sweep to an explicit [`Ruling`] (GO / GO-GRID / NO-GO).
//!
//! The box harness (`tests/seal_rate_sweep.rs`, Linux + `#[ignore]`) drives a **live**
//! Postgres guest through the real snapshot path and *feeds this module the same
//! structs* — so the numbers in `SEAL-RATE-REPORT.md` are produced by exactly the code
//! the portable proptest suite exercises against a [`mock`] oracle. Nothing here reads
//! `/dev/kvm`, the wall clock, or any RNG; it is a deterministic function of its inputs
//! (project rule 4), which is what lets a Mac reproduce the bookkeeping bit-for-bit.
//!
//! ## Why these features, not "RIP class / IF / in-hypercall"
//!
//! Task 63 §5 sketches the predicate keyed on `RIP class, IF, armed-timer state,
//! in-hypercall`. The `Vmm` public seam exposes no raw RIP/RFLAGS peek — but it exposes
//! the *exact fields `save_vm_state` decides on*: whether the landing is a
//! V-time-synchronized intercept ([`Vmm::save_vtime`] succeeds), whether an RNG
//! completion is staged (the "in-hypercall / mid-exit" case), whether the CPU state is
//! representable, and the in-flight/pending interrupt state ([`Vmm::has_inflight_event_injection`]
//! et al.). [`CpuSnapshot`] carries those. Keying `sealable` on the actual decision
//! inputs is strictly better than a RIP/IF proxy and needs no production change (the
//! task's surface list forbids one) — see `IMPLEMENTATION.md`.

use std::collections::BTreeMap;

use thiserror::Error;

/// A point on the deterministic V-time axis, in whole nanoseconds. The contract
/// [`VClock`](vtime::VClock) advances V-time at **1 ns per retired conditional branch**
/// (see [`crate::vmm::contract_vclock_config`]), so a `VTime` value is equivalently the
/// retired-branch count — the same axis [`crate::vmm::Vmm::effective_vns`] reports and a
/// `run` deadline consumes. (Task 63's prose says "retired-instruction count"; on this
/// substrate the addressable V-time grid *is* retired branches — the report notes the
/// axis explicitly.)
pub type VTime = u64;

/// Parts-per-million denominator — every rate in this module is an **integer** ppm value
/// (rule 4 forbids floating point in anything that reaches an output/hash), so `1_000_000`
/// means 100.0000 %.
pub const PPM: u32 = 1_000_000;

/// `numer/denom` as an integer parts-per-million rate, saturating and `0` when `denom == 0`
/// (an empty sweep has no rate). Rounds to nearest ppm.
#[must_use]
pub fn rate_ppm(numer: usize, denom: usize) -> u32 {
    if denom == 0 {
        return 0;
    }
    // u128 so `numer * PPM` cannot overflow for any realistic sweep size.
    let scaled = (numer as u128 * PPM as u128 + (denom as u128 / 2)) / denom as u128;
    scaled.min(PPM as u128) as u32
}

/// Render a ppm rate as a human `"NN.NNNN%"` string for the report. Pure integer
/// formatting (no float).
#[must_use]
pub fn ppm_percent(ppm: u32) -> String {
    let whole = ppm / 10_000;
    let frac = ppm % 10_000;
    format!("{whole}.{frac:04}%")
}

// ---------------------------------------------------------------------------
// 1. The V-time sampling schedule
// ---------------------------------------------------------------------------

/// Why a scheduled target was chosen — so the report can split the seal rate by the
/// kind of point (uniform coverage vs. the deliberately-inconvenient busy-window
/// samples task 63 §1 asks for).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleKind {
    /// One of the evenly-spaced points covering the post-readiness span.
    Uniform,
    /// A point placed deliberately inside a known-busy window (interrupt service,
    /// WAL fsync, scheduler tick) — the "less convenient" states of §1/§3.
    Busy(BusyKind),
}

/// The three busy-window classes task 63 §1 names as deliberate targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusyKind {
    /// Inside interrupt service (the LAPIC-timer / IRQ path).
    InterruptService,
    /// Inside a WAL fsync (Postgres durability path).
    WalFsync,
    /// On a scheduler tick / preemption.
    SchedulerTick,
}

impl BusyKind {
    /// Stable short label for the report / by-kind bookkeeping (sorts deterministically).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            BusyKind::InterruptService => "interrupt-service",
            BusyKind::WalFsync => "wal-fsync",
            BusyKind::SchedulerTick => "scheduler-tick",
        }
    }
}

/// A half-open V-time window `[start, end)` the guest is known to spend inside a busy
/// path. The schedule places a target at each window's center; the box harness derives
/// these from the serial/telemetry markers of the live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyWindow {
    /// Inclusive lower V-time bound.
    pub start: VTime,
    /// Exclusive upper V-time bound.
    pub end: VTime,
    /// What kind of busy path this window covers.
    pub kind: BusyKind,
}

impl BusyWindow {
    /// The window's center V-time (where the schedule aims a busy sample). Saturating.
    #[must_use]
    pub fn center(&self) -> VTime {
        self.start + (self.end.saturating_sub(self.start)) / 2
    }
}

/// One scheduled target: the V-time to run to, and why it was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The V-time point to `run` the guest to before attempting a seal.
    pub vtime: VTime,
    /// Why this point is in the schedule.
    pub kind: SampleKind,
}

/// Something wrong with the requested sampling schedule (never a panic — rule 4:
/// library code must not panic on caller input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// `span_start >= span_end` — no room to sample.
    #[error("empty V-time span: start >= end")]
    EmptySpan,
    /// Fewer than one target requested.
    #[error("a schedule needs at least one target")]
    ZeroTargets,
    /// The span is narrower than the requested target count — can't place distinct
    /// uniform points (each uniform point needs at least 1 ns of the span).
    #[error("span too narrow for {requested} targets (only {span} ns wide)")]
    SpanTooNarrow {
        /// How many targets were requested.
        requested: usize,
        /// The width of the span in V-time ns.
        span: VTime,
    },
}

/// A deterministic set of target V-time points across the post-readiness run.
///
/// Built by [`SamplingSchedule::build`]: `n` total targets, of which a *handful*
/// (`min(busy.len(), max(1, n/8))`) land inside the supplied [`BusyWindow`]s and the
/// rest are evenly spaced across the span. Targets are returned sorted ascending
/// (the box harness runs one live guest **forward** through them — it cannot seek
/// backward), and every target lies within `[span_start, span_end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingSchedule {
    targets: Vec<Target>,
    span_start: VTime,
    span_end: VTime,
}

impl SamplingSchedule {
    /// Build a schedule of exactly `n` targets across `[span_start, span_end)`.
    ///
    /// - `n - n_busy` **uniform** targets, one at the center of each of `n_busy` equal
    ///   buckets covering the span (so they never coincide with the exact endpoints);
    /// - `n_busy = min(busy.len(), max(1, n/8))` **busy** targets at the centers of the
    ///   first `n_busy` windows (clamped into the span).
    ///
    /// Deterministic and pure. `n ≥ 64` is the task's floor but not enforced here (the
    /// harness passes it); `build` only rejects the structurally-impossible cases.
    ///
    /// # Errors
    /// [`ScheduleError`] for an empty span, zero targets, or a span too narrow to hold
    /// `n` distinct uniform points.
    pub fn build(
        span_start: VTime,
        span_end: VTime,
        n: usize,
        busy: &[BusyWindow],
    ) -> Result<Self, ScheduleError> {
        if n == 0 {
            return Err(ScheduleError::ZeroTargets);
        }
        if span_start >= span_end {
            return Err(ScheduleError::EmptySpan);
        }
        let span = span_end - span_start;
        // Each of the `n` targets needs its own ns of the span to stay distinct.
        if (span as u128) < n as u128 {
            return Err(ScheduleError::SpanTooNarrow { requested: n, span });
        }

        let n_busy = if busy.is_empty() {
            0
        } else {
            busy.len().min((n / 8).max(1)).min(n)
        };
        let n_uniform = n - n_busy;

        let mut targets = Vec::with_capacity(n);

        // Uniform: center of each of `n_uniform` equal buckets across the span.
        for i in 0..n_uniform {
            // u128 so the multiply cannot overflow; +½-bucket to center.
            let lo = span_start as u128 + (i as u128 * span as u128) / n_uniform.max(1) as u128;
            let half = (span as u128) / (2 * n_uniform.max(1) as u128);
            let vtime = (lo + half).min(span_end as u128 - 1) as VTime;
            targets.push(Target {
                vtime,
                kind: SampleKind::Uniform,
            });
        }

        // Busy: center of each of the first `n_busy` windows, clamped into the span.
        for w in busy.iter().take(n_busy) {
            let vtime = w.center().clamp(span_start, span_end - 1);
            targets.push(Target {
                vtime,
                kind: SampleKind::Busy(w.kind),
            });
        }

        targets.sort_by_key(|t| t.vtime);
        // Dedup by V-time so a busy-window center that collides with a uniform target (or another
        // window's center) does not inflate the sampled denominator with two probes at one point.
        // The result is strictly increasing; `len()` is the true distinct count (may be < `n`).
        targets.dedup_by_key(|t| t.vtime);
        debug_assert!(
            targets.windows(2).all(|w| w[0].vtime < w[1].vtime),
            "schedule targets must be strictly increasing (distinct) after dedup"
        );
        Ok(SamplingSchedule {
            targets,
            span_start,
            span_end,
        })
    }

    /// The targets, sorted ascending by V-time.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// Number of targets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether the schedule is empty (never true for a `build`-produced schedule, which
    /// always has `n ≥ 1`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// How many targets are uniform-coverage points.
    #[must_use]
    pub fn uniform_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|t| t.kind == SampleKind::Uniform)
            .count()
    }

    /// How many targets land in busy windows.
    #[must_use]
    pub fn busy_count(&self) -> usize {
        self.len() - self.uniform_count()
    }

    /// The **adversarial** schedule of task 63 §3: each target shifted by a deterministic
    /// jitter in `[-jitter, +jitter]` and re-clamped into the span, so the guest is
    /// perturbed into a less "convenient" state near each point rather than the tidy
    /// boundary the nominal schedule aims at. Jitter is a pure `splitmix64` of the target
    /// index (no RNG crate, no wall clock) so the adversarial run is itself reproducible.
    /// Targets stay sorted; kinds are preserved.
    #[must_use]
    pub fn jittered(&self, jitter: VTime) -> SamplingSchedule {
        let lo = self.span_start as i128;
        let hi = self.span_end as i128 - 1;
        let mut targets: Vec<Target> = self
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let vtime = if jitter == 0 {
                    t.vtime
                } else {
                    let r = splitmix64(i as u64 ^ t.vtime);
                    // u128 so `2*jitter+1` never overflows even at `jitter == u64::MAX`
                    // (`jittered` is pub and the harness feeds it `ADV_JITTER_VNS`).
                    let span = 2u128 * jitter as u128 + 1;
                    let delta = (r as u128 % span) as i128 - jitter as i128;
                    (t.vtime as i128 + delta).clamp(lo, hi) as VTime
                };
                Target {
                    vtime,
                    kind: t.kind,
                }
            })
            .collect();
        targets.sort_by_key(|t| t.vtime);
        // Jitter (and its clamp to the span bounds) can collide two targets onto one V-time —
        // dedup so the distinct-target invariant holds here too (`len()` may shrink below the
        // pre-jitter count for large jitter, which is correct: fewer distinct points).
        targets.dedup_by_key(|t| t.vtime);
        debug_assert!(
            targets.windows(2).all(|w| w[0].vtime < w[1].vtime),
            "jittered targets must be strictly increasing (distinct) after dedup"
        );
        SamplingSchedule {
            targets,
            span_start: self.span_start,
            span_end: self.span_end,
        }
    }
}

/// splitmix64 — a well-distributed integer mixer used for deterministic, RNG-free jitter.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// 2. Per-landing features and the seal outcome
// ---------------------------------------------------------------------------

/// The seal-relevant, snapshot-observable features of a CPU landing — the exact inputs
/// [`crate::vmm::Vmm::save_vm_state`] decides representability on, read through the
/// public `Vmm` seam (no register peek, no production change). Populated on the box from
/// the live guest; synthetic in the portable tests / the [`mock`] oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CpuSnapshot {
    /// The landing is a **V-time-synchronized** intercept: [`crate::vmm::Vmm::save_vtime`]
    /// succeeds, i.e. `last_intercept_work` is the current work and the exact V-time is
    /// known. This is the dominant sealability discriminator — a non-synchronized exit
    /// (HLT/PIO/CPUID) cannot record an exact `vns`, so `save_vm_state` fails closed.
    pub synchronized: bool,
    /// A seeded RDRAND/RDSEED completion is **staged** (drawn from the stream but its
    /// register-write/RIP-advance not yet committed) — the "mid-hypercall / mid-exit"
    /// case; `save_vm_state` fails closed here (step once more to commit).
    pub rng_mid_exit: bool,
    /// The CPU state is **unrepresentable** by the `vm_state` subset (a queued triple
    /// fault, an exception payload with the cap off, PAE `kvm_sregs2` flags/pdptrs, or
    /// `debugregs.flags`). For the post-task-41 64-bit/paging-off determinism guest this
    /// is effectively never set — it closes the contract for synthetic/relayed blobs.
    pub unrepresentable: bool,
    /// The landing carries in-flight `kvm_vcpu_events` injection state the *quiescent-only*
    /// task-39 codec fail-closed-rejected (task 40's "3112 in-flight" class). Task 41
    /// **captures** this, so it is **not** a disqualifier — recorded to prove the seal
    /// succeeds at exactly the points that used to fail.
    pub inflight_injection: bool,
    /// A *genuine* active injected/pending event (the active subset of `inflight_injection`).
    pub active_injection: bool,
    /// A guest interrupt is pending delivery but not yet accepted (the armed/firing LAPIC
    /// timer, or the COM1 ExtINT line) — the §5 "armed-timer" feature, observable via
    /// [`crate::vmm::Vmm::has_pending_guest_interrupt`].
    pub pending_guest_interrupt: bool,
}

impl CpuSnapshot {
    /// A clean, quiescent, synchronized landing (all failure bits clear) — the archetype
    /// of a sealable point. Handy for tests and for describing the common case.
    #[must_use]
    pub fn clean_synchronized() -> Self {
        CpuSnapshot {
            synchronized: true,
            rng_mid_exit: false,
            unrepresentable: false,
            inflight_injection: false,
            active_injection: false,
            pending_guest_interrupt: false,
        }
    }
}

/// Why a seal attempt failed. Mirrors the fail-closed classes of
/// [`crate::vmm::Vmm::save_vm_state`], plus the §2 determinism reclassification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureReason {
    /// Landed at a non-V-time-intercept exit — the exact `vns` is unknown, `save_vm_state`
    /// fails closed. Task 40's "5280 non-synchronized" class; **not** cleared by task 41.
    NonSynchronized,
    /// A staged RDRAND/RDSEED completion — snapshot only at a clean boundary.
    RngMidExit,
    /// The CPU state is unrepresentable by the `vm_state` subset (fail-closed).
    Unrepresentable,
    /// The seal itself succeeded, but branching from it twice with the same seed did **not**
    /// reproduce a bit-identical `state_hash` (or hit a step error / skid) — task 63 §2
    /// reclassifies such a seal as a failure, not a success.
    BranchNondeterministic,
}

impl FailureReason {
    /// Stable short label for the by-reason tally (sorts deterministically in the report).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FailureReason::NonSynchronized => "non-synchronized",
            FailureReason::RngMidExit => "rng-mid-exit",
            FailureReason::Unrepresentable => "unrepresentable",
            FailureReason::BranchNondeterministic => "branch-nondeterministic",
        }
    }

    /// All reasons, for initializing a zeroed tally in a stable order.
    #[must_use]
    pub fn all() -> [FailureReason; 4] {
        [
            FailureReason::NonSynchronized,
            FailureReason::RngMidExit,
            FailureReason::Unrepresentable,
            FailureReason::BranchNondeterministic,
        ]
    }
}

/// The outcome of one seal attempt at a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealResult {
    /// `save_vm_state` succeeded **and** (when checked) the seal branched deterministically.
    Sealed,
    /// The seal failed for the given reason.
    Failed(FailureReason),
}

impl SealResult {
    /// Whether this attempt counts as a successful, dependable seal.
    #[must_use]
    pub fn is_sealed(&self) -> bool {
        matches!(self, SealResult::Sealed)
    }
}

/// One row of the sweep: the target, where the run actually landed, the landing's
/// features, and the seal outcome. The box harness appends one of these per target; the
/// bookkeeping below is computed over a `&[SealAttempt]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealAttempt {
    /// The scheduled target this attempt was for.
    pub target: Target,
    /// Where the `run` actually stopped — the first synchronized boundary at/after the
    /// target (`landed_vtime >= target.vtime`), or the interior point an adversarial probe
    /// stepped to.
    pub landed_vtime: VTime,
    /// The landing's seal-relevant features.
    pub snapshot: CpuSnapshot,
    /// The seal outcome.
    pub result: SealResult,
}

impl SealAttempt {
    /// How far past the target the run landed (`landed - target`, saturating) — the
    /// **addressability** of that target. Small overshoot ⇒ the V-time grid is dense and
    /// sealing is effectively continuous; large overshoot ⇒ a coarse grid.
    #[must_use]
    pub fn overshoot(&self) -> VTime {
        self.landed_vtime.saturating_sub(self.target.vtime)
    }
}

// ---------------------------------------------------------------------------
// 3. Seal-rate bookkeeping
// ---------------------------------------------------------------------------

/// The rolled-up seal rate over a sweep: totals, the by-reason failure tally (a
/// `BTreeMap` for a stable, determinism-safe order — rule 4), and the integer success
/// rate in ppm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealStats {
    /// Total attempts.
    pub n: usize,
    /// Attempts that sealed dependably ([`SealResult::Sealed`]).
    pub sealed: usize,
    /// Failures grouped by reason label (sorted; every reason present, `0` if none).
    pub by_reason: BTreeMap<&'static str, usize>,
    /// Successful-seal rate in parts-per-million (`sealed / n`).
    pub success_rate_ppm: u32,
}

impl SealStats {
    /// Roll up a slice of attempts. Pure; stable ordering.
    #[must_use]
    pub fn of(attempts: &[SealAttempt]) -> Self {
        let mut by_reason: BTreeMap<&'static str, usize> = BTreeMap::new();
        for r in FailureReason::all() {
            by_reason.insert(r.label(), 0);
        }
        let mut sealed = 0usize;
        for a in attempts {
            match a.result {
                SealResult::Sealed => sealed += 1,
                SealResult::Failed(reason) => {
                    *by_reason.entry(reason.label()).or_insert(0) += 1;
                }
            }
        }
        let n = attempts.len();
        SealStats {
            n,
            sealed,
            by_reason,
            success_rate_ppm: rate_ppm(sealed, n),
        }
    }

    /// Failures total (`n - sealed`).
    #[must_use]
    pub fn failed(&self) -> usize {
        self.n - self.sealed
    }
}

/// The addressability distribution — how far past each target the run had to go to reach
/// a sealable landing. Integer order statistics (no float): min, max, mean (floor), and
/// the p50/p90 of the sorted overshoots, plus how many targets were hit exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overshoot {
    /// Smallest overshoot observed.
    pub min: VTime,
    /// Largest overshoot observed.
    pub max: VTime,
    /// Floor of the mean overshoot.
    pub mean: VTime,
    /// Median (p50) overshoot.
    pub p50: VTime,
    /// 90th-percentile overshoot.
    pub p90: VTime,
    /// How many targets were sealed at exactly the requested V-time (overshoot `0`).
    pub exact_hits: usize,
    /// How many attempts contributed.
    pub n: usize,
}

impl Overshoot {
    /// Compute the overshoot distribution over `attempts`. `None` for an empty slice.
    #[must_use]
    pub fn of(attempts: &[SealAttempt]) -> Option<Self> {
        if attempts.is_empty() {
            return None;
        }
        let mut deltas: Vec<VTime> = attempts.iter().map(SealAttempt::overshoot).collect();
        deltas.sort_unstable();
        let n = deltas.len();
        let sum: u128 = deltas.iter().map(|&d| d as u128).sum();
        let exact_hits = deltas.iter().take_while(|&&d| d == 0).count();
        Some(Overshoot {
            min: deltas[0],
            max: deltas[n - 1],
            mean: (sum / n as u128) as VTime,
            p50: deltas[percentile_index(n, 50)],
            p90: deltas[percentile_index(n, 90)],
            exact_hits,
            n,
        })
    }
}

/// The index into a length-`n` sorted slice for the `p`-th percentile (nearest-rank,
/// clamped). Integer-only.
fn percentile_index(n: usize, p: usize) -> usize {
    if n == 0 {
        return 0;
    }
    // nearest-rank: ceil(p/100 * n) - 1, clamped to [0, n-1]
    let rank = (p * n).div_ceil(100);
    rank.saturating_sub(1).min(n - 1)
}

// ---------------------------------------------------------------------------
// 4. Materialization depth (the parent-rooted premise)
// ---------------------------------------------------------------------------

/// Something wrong with a materialization-depth measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DepthError {
    /// The deep seal is not strictly after both genesis and the parent seal.
    #[error("deep seal V-time must be > parent seal V-time > genesis")]
    NonMonotonic,
}

/// The task 63 §4 measurement: to reconstruct a deep sealed state, do you replay the
/// **whole prefix from genesis**, or only the **suffix from the nearest sealed parent**?
/// The ratio of the two replay depths is the quantitative confirmation of the Phase-C
/// premise that materialization cost is the suffix, not the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterializationDepth {
    /// Replay depth (V-time ns) from genesis (boot) to the deep seal.
    pub from_genesis: VTime,
    /// Replay depth (V-time ns) from the parent seal to the deep seal (the suffix).
    pub from_parent: VTime,
}

impl MaterializationDepth {
    /// Build from the three V-time points, requiring `genesis < parent_seal < deep_seal`.
    ///
    /// # Errors
    /// [`DepthError::NonMonotonic`] if the points are not strictly increasing.
    pub fn new(genesis: VTime, parent_seal: VTime, deep_seal: VTime) -> Result<Self, DepthError> {
        if !(genesis < parent_seal && parent_seal < deep_seal) {
            return Err(DepthError::NonMonotonic);
        }
        Ok(MaterializationDepth {
            from_genesis: deep_seal - genesis,
            from_parent: deep_seal - parent_seal,
        })
    }

    /// The suffix/prefix depth ratio in ppm (`from_parent / from_genesis`). Smaller is
    /// better — it is the fraction of a genesis replay the parent-rooted path pays.
    #[must_use]
    pub fn ratio_ppm(&self) -> u32 {
        rate_ppm(self.from_parent as usize, self.from_genesis as usize)
    }

    /// The replay saved by rooting at the parent instead of genesis, in ppm
    /// (`1 - ratio`).
    #[must_use]
    pub fn savings_ppm(&self) -> u32 {
        PPM - self.ratio_ppm()
    }
}

// ---------------------------------------------------------------------------
// 5. The `sealable` predicate + its precision/recall
// ---------------------------------------------------------------------------

/// The **Phase-A2 admission predicate** (task 63 §5): `true` iff the archive should
/// admit an exemplar sealed at a landing with these features.
///
/// It fires exactly the conditions [`crate::vmm::Vmm::save_vm_state`] requires: a
/// V-time-**synchronized** landing, with **no staged RNG completion**, and
/// **representable** CPU state. In-flight interrupt/event injection is deliberately *not*
/// a disqualifier — task 41 captures it, and the whole point of this measurement is that
/// those points now seal.
///
/// This is a *static* predicate over observable features; it cannot see the *dynamic*
/// branch-determinism outcome (§2), so a synchronized/representable point that later
/// fails to branch deterministically is a genuine precision miss — measured, not hidden,
/// by [`PredicateQuality`].
#[must_use]
pub fn sealable(s: &CpuSnapshot) -> bool {
    s.synchronized && !s.rng_mid_exit && !s.unrepresentable
}

/// Precision/recall of a `sealable`-style predicate as a predictor of actual seal success
/// over a sweep. Ground truth is `result == Sealed`; predicted-positive is
/// `predicate(&snapshot)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateQuality {
    /// Predicted sealable **and** sealed.
    pub true_pos: usize,
    /// Predicted sealable but failed (a precision miss — e.g. branch-nondeterminism).
    pub false_pos: usize,
    /// Predicted not-sealable and failed.
    pub true_neg: usize,
    /// Predicted not-sealable but actually sealed (a recall miss — the predicate is too
    /// strict).
    pub false_neg: usize,
    /// `TP / (TP + FP)` in ppm — of the points the archive would admit, how many seal.
    pub precision_ppm: u32,
    /// `TP / (TP + FN)` in ppm — of the points that seal, how many the archive admits.
    pub recall_ppm: u32,
}

impl PredicateQuality {
    /// Measure a predicate over the sweep.
    #[must_use]
    pub fn measure(attempts: &[SealAttempt], predicate: impl Fn(&CpuSnapshot) -> bool) -> Self {
        let (mut tp, mut fp, mut tn, mut fn_) = (0usize, 0usize, 0usize, 0usize);
        for a in attempts {
            match (predicate(&a.snapshot), a.result.is_sealed()) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, false) => tn += 1,
                (false, true) => fn_ += 1,
            }
        }
        PredicateQuality {
            true_pos: tp,
            false_pos: fp,
            true_neg: tn,
            false_neg: fn_,
            precision_ppm: rate_ppm(tp, tp + fp),
            recall_ppm: rate_ppm(tp, tp + fn_),
        }
    }
}

// ---------------------------------------------------------------------------
// 6. The ruling
// ---------------------------------------------------------------------------

/// The task 63 gate-3 verdict the report ends with — the Phase-C gate the foreman
/// consults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruling {
    /// **GO** — arbitrary-V-time sealing holds at a rate the archive can rely on. Phase C
    /// proceeds unrestricted: any `Moment` is admissible; a materialization-time seal
    /// failure is a regression to escalate, not a design constraint.
    Go,
    /// **GO (grid-restricted)** — seals are 100 %-dependable, but only *at* the V-time
    /// grid the `run` deadline lands on (non-trivial overshoot), not at an arbitrary
    /// interior V-time. Phase C proceeds, keying exemplars to the *nearest synchronized
    /// boundary* (which `sealable` accepts) rather than an exact interior `Moment`.
    GoGridRestricted,
    /// **NO-GO / RESTRICTED** — sealing is only partial; the archive must key exemplars to
    /// `sealable(Moment)`. Phase C inherits the predicate.
    NoGoRestricted,
}

impl Ruling {
    /// Stable label for the report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Ruling::Go => "GO",
            Ruling::GoGridRestricted => "GO (grid-restricted)",
            Ruling::NoGoRestricted => "NO-GO / RESTRICTED",
        }
    }
}

/// The tunable thresholds the ruling is made against. Explicit and documented so the
/// verdict is reproducible and reviewable (the exploration roadmap leaves the exact bar
/// as the archive's tolerance — these are the defensible defaults task 63 is decided at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RulingThresholds {
    /// Minimum nominal seal-success rate (ppm) for a GO.
    pub min_nominal_ppm: u32,
    /// Minimum adversarial seal-success rate (ppm) for a GO.
    pub min_adversarial_ppm: u32,
    /// Minimum fraction (ppm) of the **scheduled** jittered targets that must actually be
    /// *probed* for the adversarial rate to be trusted. The overshoot guard legitimately skips
    /// targets a prior landing overshot, but a §3 rate computed over a *tiny* probed denominator
    /// is not representative — below this floor `rule()` refuses to trust §3 (NO-GO), rather than
    /// letting a 2-of-64 sample satisfy the bar.
    pub min_adversarial_coverage_ppm: u32,
    /// Overshoot (V-time ns) at/below which the grid is "dense" — sealing is effectively
    /// continuous, so a high rate is an *unrestricted* GO rather than a grid-restricted one.
    pub dense_grid_overshoot: VTime,
}

impl Default for RulingThresholds {
    /// The task-63 defaults: ≥ 99 % nominal, ≥ 95 % adversarial, ≥ 10 % of the jittered targets
    /// actually probed, and a grid is "dense" when the p90 overshoot is under ~one scheduler tick
    /// worth of V-time (100 000 ns ≈ 100 µs of retired-branch V-time — well under a 250 Hz tick).
    fn default() -> Self {
        RulingThresholds {
            min_nominal_ppm: 990_000,
            min_adversarial_ppm: 950_000,
            min_adversarial_coverage_ppm: 100_000,
            dense_grid_overshoot: 100_000,
        }
    }
}

/// Everything the ruling is computed from — one struct so the report and the tests reduce
/// the same inputs to the same verdict.
#[derive(Debug, Clone)]
pub struct RulingInputs {
    /// Nominal-pass seal stats.
    pub nominal: SealStats,
    /// Adversarial-pass seal stats.
    pub adversarial: SealStats,
    /// How many branch-determinism-checked sealed points came back **bit-identical** (task 63
    /// §2). A checked point that diverged is *not* counted here — and is separately reclassified
    /// a failure in the pass stats and hard-failed by the harness.
    pub det_verified: usize,
    /// How many sealed points were **subjected to** the branch-determinism check. In a run that
    /// verifies a spread subset (the §2 deviation) this is the subset size, **not** all of
    /// `nominal.sealed` — so `det_verified / det_sealed_total` reads honestly as "N/M of a
    /// subset", never overclaimed as global. `rule()` requires `det_verified == det_sealed_total
    /// > 0`.
    pub det_sealed_total: usize,
    /// How many jittered targets the adversarial pass **scheduled** (before the overshoot guard
    /// skipped any). `adversarial.n` is how many were actually probed; `adversarial_scheduled`
    /// is the denominator the §3 coverage floor is measured against. `0` when there is no
    /// adversarial pass (the coverage gate is then vacuously skipped).
    pub adversarial_scheduled: usize,
    /// The nominal-pass overshoot distribution (addressability), if any attempts ran.
    pub overshoot: Option<Overshoot>,
}

impl RulingInputs {
    /// The determinism gate: at least one seal was checked, and **every** checked seal branched
    /// bit-identically. False if any diverged or none were checked (a vacuous pass).
    #[must_use]
    pub fn determinism_ok(&self) -> bool {
        self.det_sealed_total > 0 && self.det_verified == self.det_sealed_total
    }

    /// The §3 coverage gate: enough of the scheduled jittered targets were actually probed for
    /// the adversarial rate to be representative. Vacuously true when no adversarial targets were
    /// scheduled (`adversarial_scheduled == 0`).
    #[must_use]
    pub fn adversarial_coverage_ok(&self, th: RulingThresholds) -> bool {
        self.adversarial_scheduled == 0
            || rate_ppm(self.adversarial.n, self.adversarial_scheduled)
                >= th.min_adversarial_coverage_ppm
    }

    /// Human render of the determinism evidence, honest about the subset:
    /// `"9/9 of a spread subset of 64 sealed"`.
    #[must_use]
    pub fn determinism_summary(&self) -> String {
        format!(
            "{}/{} of a spread subset of {} sealed",
            self.det_verified, self.det_sealed_total, self.nominal.sealed
        )
    }
}

/// Reduce a sweep to its [`Ruling`] under the given thresholds. Pure and total.
///
/// - Any determinism gap (a checked seal diverged, or **none were checked**), a
///   nominal/adversarial rate below the bar, or **insufficient §3 coverage** (too few of the
///   scheduled jittered targets probed to trust the adversarial rate) → **NO-GO**.
/// - At/above the bar with a **dense** grid (small p90 overshoot) → **GO** (unrestricted).
/// - At/above the bar but a **coarse** grid → **GO (grid-restricted)**: dependable at the
///   synchronized boundaries, but not at an arbitrary interior V-time.
#[must_use]
pub fn rule(inputs: &RulingInputs, th: RulingThresholds) -> Ruling {
    if !inputs.determinism_ok() {
        return Ruling::NoGoRestricted;
    }
    // A §3 rate over a tiny probed denominator is not evidence — refuse it.
    if !inputs.adversarial_coverage_ok(th) {
        return Ruling::NoGoRestricted;
    }
    let rate_ok = inputs.nominal.success_rate_ppm >= th.min_nominal_ppm
        && inputs.adversarial.success_rate_ppm >= th.min_adversarial_ppm;
    if !rate_ok {
        return Ruling::NoGoRestricted;
    }
    match inputs.overshoot {
        Some(o) if o.p90 <= th.dense_grid_overshoot => Ruling::Go,
        // A high, determinism-clean rate but a coarse grid: dependable *on the grid*.
        Some(_) => Ruling::GoGridRestricted,
        // No overshoot data at all (empty sweep) — cannot claim a dense grid.
        None => Ruling::GoGridRestricted,
    }
}

pub mod mock;

#[cfg(test)]
mod tests;
