# Task 05 — `consonance/vtime`: virtual time engine & precise-injection planner

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/vtime/`.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does not require: `/dev/kvm`, Intel CPU,
QEMU, root, perf_event (the real PMU backend is a later, separate task).

## Context

In the deterministic hypervisor, the guest never sees real time. Every guest-visible clock
(TSC, timer deadlines) is derived from **V-time**, which is a pure function of *work
performed* — a hardware counter of retired branches read at every VM exit. Timer interrupts
must be **injected at an exact work count**: the hardware can interrupt us *near* a target
count (PMU overflow has skid — it fires late by an unpredictable amount within a bounded
window, never early relative to the armed count... and we additionally arm it early by a
safety margin), after which we single-step the vCPU to the precise count and inject.

This crate is the **pure-logic half** of that mechanism: the work↔time arithmetic, the timer
deadline queue, and the injection-planner state machine — all driven through a backend trait.
The real perf_event/KVM backend is a later, separate task; here the backend is a simulator
that models skid adversarially, so the planner's exactness can be property-tested now.

Everything is integer/fixed-point. **No floating point anywhere** (clippy lint
`clippy::float_arithmetic = "deny"` in the crate).

## Definitions (normative)

- `work`: u64, monotonic, never overflows in practice (no wraparound handling needed; debug
  asserts welcome). **Work counts *counted events*, not instructions** — in the real backend
  it will be a PMU count of retired conditional branches. Most instructions advance work by
  0; an instruction advances it by at most 1. An injection target `T` therefore means: stop
  at the **first instruction boundary at which work reaches T** — well-defined and
  deterministic because it's a pure function of the (deterministic) instruction stream.
- Saturation: `vns()` and `tsc()` compute in u128 and **saturate to `u64::MAX`** if the
  result exceeds u64 (deterministic, documented — never implementation-defined, never a
  panic). `VClock::new` rejects configs that would saturate at trivially small work counts.
- V-time in nanoseconds: `vns(work) = vns_base + floor(work * ratio_num / ratio_den)`
  computed in u128; `ratio_num`, `ratio_den` are u64 config (`den != 0`), `vns_base` is a
  mutable u64 offset (see below). Monotonic non-decreasing by construction.
- `vns_base` exists for two integration events: **idle-skip** — when the guest HLTs, work
  stops advancing, so the host warps V-time forward to the next timer deadline by bumping
  `vns_base`; and **snapshot restore** — the hardware counter restarts (work resumes from 0),
  so the restored clock carries the snapshot's effective V-time entirely in `vns_base`.
  Because `tsc` is derived from `vns`, both events move the guest TSC forward consistently
  for free.
- Virtual TSC: `tsc(work) = tsc_base + floor(vns(work) * tsc_hz / 1_000_000_000)` in u128
  intermediates; `tsc_hz` u64 config (e.g. 2_000_000_000), `tsc_base` u64 (snapshot restore
  sets it).
- Inverse mapping: `work_for_vns(t) =` the **smallest** work `w` with `vns(w) >= t`
  (ceil division, document the exact formula and its edge cases; this is where off-by-one
  bugs live, so it gets its own property test).

## Public API

```rust
pub struct VClockConfig { pub ratio_num: u64, pub ratio_den: u64,
                          pub tsc_hz: u64, pub tsc_base: u64,
                          pub vns_base: u64 /* 0 for a fresh machine */ }
pub struct VClock { /* ... */ }
impl VClock {
    pub fn new(cfg: VClockConfig) -> Result<VClock, VtimeError>; // rejects den == 0 etc.
    pub fn vns(&self, work: u64) -> u64;
    pub fn tsc(&self, work: u64) -> u64;
    /// Smallest w with vns(w) >= target; returns 0 if target <= vns_base
    /// (a deadline already in the past, e.g. right after an idle warp).
    pub fn work_for_vns(&self, vns: u64) -> u64;
    /// Idle-skip: warp V-time forward (guest HLTed; work is frozen).
    /// Saturating; must keep all invariants (monotonicity, tsc consistency).
    pub fn advance_idle(&mut self, vns_delta: u64);
    /// Effective V-time at `work`, for storing in a snapshot. Restoring is
    /// VClock::new with vns_base = this value (work counter restarts at 0).
    pub fn snapshot_vns(&self, work: u64) -> u64;
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct TimerToken(pub u64);

/// Deadline queue in V-time. Pure data structure (BTree-based); ties broken by
/// insertion order (FIFO) — determinism requires a total order, document it.
pub struct TimerQueue { /* ... */ }
impl TimerQueue {
    pub fn new() -> Self;
    pub fn schedule_oneshot(&mut self, deadline_vns: u64, token: TimerToken);
    pub fn schedule_periodic(&mut self, first_vns: u64, period_vns: u64, token: TimerToken)
        -> Result<(), VtimeError>; // period 0 is an error
    pub fn cancel(&mut self, token: TimerToken) -> bool;
    /// Earliest pending deadline, if any.
    pub fn peek_next(&self) -> Option<(u64, TimerToken)>;
    /// Pop every deadline with deadline_vns <= now_vns (in deterministic order),
    /// re-arming periodic timers (next = fired deadline + period — fixed cadence,
    /// no drift accumulation).
    pub fn pop_due(&mut self, now_vns: u64) -> Vec<(u64, TimerToken)>;
}

/// What the planner asks of the CPU/counter backend. The real implementation
/// (perf_event + KVM single-step) comes later; tests use the simulator below.
pub trait CpuBackend {
    /// Current work count (e.g. counter read at the current VM exit).
    fn work(&self) -> u64;
    /// Arm an overflow interrupt at the given absolute work count, then run.
    /// Returns the work count at which execution actually stopped:
    /// guaranteed >= armed count, overshoot bounded by the backend's skid.
    fn run_until_overflow(&mut self, armed_at: u64) -> Result<u64, BackendError>;
    /// Execute exactly one INSTRUCTION (not one work unit). Returns the new work
    /// count, which advances by 0 or 1 — most instructions are not counted events.
    fn single_step(&mut self) -> Result<u64, BackendError>;
}

pub struct PlannerConfig {
    /// Arm the counter this many work units BEFORE the target, so that
    /// armed_at + worst-case skid still lands before the target. Must be > max skid.
    pub skid_margin: u64,
}

pub enum PlanOutcome {
    /// Stopped exactly at `target`; caller may now inject the interrupt.
    /// `single_steps_used` counts INSTRUCTIONS stepped; it is bounded only by the
    /// guest's event density, NOT by skid_margin (a long branch-free stretch may
    /// need many steps per counted event). The skid_margin bound applies to the
    /// counted-event distance covered by stepping.
    ReadyToInject { target: u64, stopped_at: u64, single_steps_used: u64 },
    /// Target already passed when planning started (caller bug or missed deadline):
    /// reported, never silently absorbed.
    TargetInPast { target: u64, now: u64 },
}

pub struct InjectionPlanner { /* owns PlannerConfig */ }
impl InjectionPlanner {
    pub fn new(cfg: PlannerConfig) -> Self;
    /// Drive `backend` so it stops exactly at work == target:
    /// - if target == now: ReadyToInject immediately, zero steps (this is the
    ///   normal case right after an idle warp made a deadline current);
    /// - if target - now > skid_margin: arm at (target - skid_margin), run, then
    ///   single-step (instruction by instruction) until work reaches target;
    /// - if 0 < target - now <= skid_margin: single-step the whole way (again:
    ///   loop until WORK reaches target, not a fixed number of steps);
    /// - if backend overshoots the target despite the margin (real-hardware skid
    ///   exceeded the configured margin): return VtimeError::SkidExceeded — this
    ///   is a determinism-destroying event and must be loud, never papered over.
    pub fn stop_at(&self, backend: &mut dyn CpuBackend, target: u64)
        -> Result<PlanOutcome, VtimeError>;
}

pub enum VtimeError { /* config errors, SkidExceeded { armed_at, target, stopped_at },
                         backend errors, via thiserror */ }
pub struct BackendError(/* opaque, thiserror */);
```

### Test simulator: `sim` module (public, others will reuse it)

`SimCpu` implements `CpuBackend` over an abstract instruction stream in which **each
instruction is a counted event or not**, per a seeded deterministic pattern with configurable
event density (from 1.0 — every instruction counts — down to sparse, e.g. one counted event
per 1 000 instructions). `single_step` executes one instruction and advances work by that
instruction's 0 or 1. `run_until_overflow(armed)` stops at an instruction boundary with work
= `armed + skid_i`, where the skid sequence is a seeded xorshift64\* PRNG drawing from
`0..=max_skid` (skid in work units). It records a full event log (arms, stops, steps) so
tests can assert *how* the planner drove it, not just where it ended. Include `max_skid`
greater than, equal to, and less than `skid_margin` in different tests.

## Acceptance gates

Beyond the standard gates:

1. **Arithmetic property tests**: monotonicity of `vns` and `tsc` over random increasing
   work sequences; round-trip law `work_for_vns(vns(w)) <= w` and
   `vns(work_for_vns(t)) >= t` with exactness at boundaries; saturation behavior verified at
   extreme configs (ratio 1/1, huge num with den 1, `work = u64::MAX`) — saturates to
   `u64::MAX`, never panics, stays monotonic.
2. **Planner exactness property test** (the core gate): arbitrary (current work, target >
   current, skid sequence with max_skid < skid_margin, event density across the full range
   including sparse streams): `stop_at` always returns `ReadyToInject { stopped_at == target }`,
   and the counted-event distance covered by single-stepping is ≤ skid_margin
   (`single_steps_used` itself may be far larger on sparse streams — assert it equals the
   number of instructions the sim actually stepped, and that the loop terminates). ≥ 256
   cases including target − now ∈ {1, skid_margin, skid_margin + 1}, skid values hitting 0
   and max, and density 1.0 as a degenerate case.
3. **SkidExceeded test**: simulator with max_skid > skid_margin eventually yields
   `VtimeError::SkidExceeded` (and the error carries the diagnostic counts).
4. **TimerQueue determinism test**: schedule a mix of one-shots and periodics, drive
   `pop_due` over a work schedule twice ⇒ identical firing sequences; FIFO tie-break
   verified for equal deadlines; periodic re-arm shows no drift (fire times are exactly
   first + k·period even when popped late).
5. **End-to-end scenario test**: VClock + TimerQueue + InjectionPlanner + SimCpu: schedule a
   100 µs-period timer, run the loop "next deadline → work_for_vns → stop_at → pop_due" for
   1 000 firings, including stretches where the simulated guest "HLTs" (no work; the loop
   uses `advance_idle` to warp to the next deadline and fires it with zero steps); assert
   every firing's stop happened at exactly the computed work count, and re-running the whole
   scenario with the same seeds reproduces the identical event log.
6. **Idle-skip & restore continuity test**: (a) `advance_idle` preserves monotonicity, and
   afterwards `tsc(work)` still equals the defining formula applied to the new total vns
   (do NOT assert a delta identity like "tsc advanced by exactly floor(delta·hz/1e9)" — the
   floor makes deltas carry-dependent; assert against the formula, plus monotonicity);
   (b) property test:
   run a scenario to an arbitrary point, `snapshot_vns`, build a fresh VClock with that
   `vns_base` (work restarting at 0) ⇒ `vns`/`tsc` continue without discontinuity, and the
   continuation's event log matches an unsnapshotted reference run.
   *Integrator ruling (PR #5): bit-exact unsnapshotted-reference equality is required for
   integer ratios (`ratio_den == 1`), which are the only snapshot-supported configs per
   `docs/INTEGRATION.md` §4. For fractional ratios — excluded from snapshot use — the u64
   API necessarily quantizes to whole ns: exactness at the restore instant plus a proven
   ≤ 1 ns lag bound is the required property.*
7. **Documentation gate**: module-level doc comment (~1 page) explaining PMU skid, why the
   margin-then-single-step design is required, and what the real backend will map each trait
   method to (perf_event overflow → SIGIO/KVM exit; single_step → KVM_GUESTDBG_SINGLESTEP).
   Cite rr's technique as prior art.

## Non-goals

perf_event, KVM, signals, or any syscall (this crate has no OS dependencies at all — keep it
`std`-only for tests but with core logic `no_std`-compatible if convenient, not required);
interrupt *vectors*/APIC emulation (the planner stops the CPU; what gets injected is the
VMM's business); multi-vCPU; wall-clock anything.
