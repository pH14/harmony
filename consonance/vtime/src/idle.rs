// SPDX-License-Identifier: AGPL-3.0-or-later
//! The idle-resume planner: [`IdlePlanner`], [`IdleAdvance`].
//!
//! The dual of [`InjectionPlanner`](crate::InjectionPlanner). Where the
//! injection planner reaches a scheduled event **by executing** (single-stepping
//! the retired-branch counter to an exact work count), the idle planner reaches
//! it **by jumping**: when the guest goes idle (`HLT`) waiting for a timer that
//! will come, the run loop warps the virtual clock forward to the armed timer's
//! deadline `D` and resumes — *without executing a single instruction*. The two
//! together make the run loop a discrete-event clock: always advance V-time to
//! the next event, by executing when there is runnable work and by jumping when
//! there is not.
//!
//! ## Mechanism: where the jump lands in the clock model
//!
//! V-time is modelled as
//! `guest_vtime = execution_vtime(real_retired_branches) + accumulated_idle`,
//! which is exactly [`VClock`](crate::VClock):
//! `vns(work) = vns_base + work·ratio`, with `work·ratio` the execution
//! component and `vns_base` the idle accumulator (idle-skip + snapshot restore;
//! see the crate docs). An idle jump adds `D − now` to **`vns_base` only**
//! ([`VClock::advance_idle`](crate::VClock::advance_idle)) — it **never** touches
//! the retired-branch `work`. That is the load-bearing invariant: a jump
//! executes no instructions, so it must fabricate **zero** retired branches, and
//! the execution-derived clock (and the `B ≡ A` counter equality the injection
//! path relies on) stays true over the execution component.
//!
//! ## Policy: land exactly at `D` (the deterministic base — and the fault seam)
//!
//! [`IdlePlanner::plan`] is the single point that *decides* how far to advance.
//! The base, deterministic clock always lands **exactly at the deadline** `D`
//! (`advance = D − now`, saturating), and **zero** when `D` is already due — the
//! overdue/at-deadline fast path (fire immediately, no jump), the HLT analogue
//! of the injection planner's [`PlanOutcome::TargetInPast`](crate::PlanOutcome).
//! Every input is a pure function of the seed (`D` from the guest's own timer
//! programming, `now` from the work-derived clock), so two same-seed runs idle
//! at the same point and jump the same amount — the idle period is a
//! deterministic constant, never a nondeterminism source.
//!
//! That "land exactly at `D`" rule is the *mechanism*; *where* to land is the
//! seam. A future dissonance fault-overlay (Antithesis-style mechanism/policy
//! split) can prescribe a deviation — land at `D + δ` as a deterministic timing
//! fault — by supplying a different decision here, **without** perturbing this
//! descriptive, deterministic base clock. No such policy is built today;
//! [`IdlePlanner`] exists so that decision has one clean, overridable home.

/// The planned idle advance produced by [`IdlePlanner::plan`].
///
/// Pure data: the caller applies it to the clock
/// ([`VClock::advance_idle`](crate::VClock::advance_idle)`(advance_vns)`), then
/// arms/injects the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleAdvance {
    /// V-time (ns) to add to the clock's idle accumulator. `D − now` for a
    /// future deadline; **`0`** when the deadline is already due (a zero jump —
    /// the clock does not move, the timer fires immediately).
    pub advance_vns: u64,
    /// The effective V-time the clock lands at after the advance:
    /// `max(now_vns, deadline_vns)` — exactly `deadline_vns` for a future
    /// deadline, or `now_vns` unchanged when it was already due. Never less than
    /// `now_vns` (the clock never moves backward).
    pub landed_vns: u64,
    /// `true` iff the deadline was already due (`deadline_vns <= now_vns`), so
    /// [`Self::advance_vns`] is `0`. The idle resume should then inject the
    /// timer immediately — the HLT analogue of
    /// [`PlanOutcome::TargetInPast`](crate::PlanOutcome::TargetInPast).
    pub already_due: bool,
}

/// Decides how far to warp the virtual clock forward on an idle (`HLT`) resume —
/// the idle-jump dual of [`InjectionPlanner`](crate::InjectionPlanner). See the
/// module docs for the mechanism (jump only the idle accumulator, never the
/// retired-branch count) and the policy/fault seam.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdlePlanner {
    // A zero-sized seam: the planner is stateless today (the only policy is
    // "land exactly at D"), but constructing it through `new()` keeps a future
    // fault-overlay policy a private, non-breaking field addition — mirroring
    // `InjectionPlanner`'s `PlannerConfig`-carrying shape.
    _seam: (),
}

impl IdlePlanner {
    /// A planner with the deterministic land-exactly-at-`D` policy.
    pub fn new() -> Self {
        IdlePlanner { _seam: () }
    }

    /// Plan the idle advance from the current effective V-time `now_vns` to the
    /// armed timer deadline `deadline_vns`.
    ///
    /// Pure, saturating, monotonic:
    /// - `advance_vns = deadline_vns.saturating_sub(now_vns)` — `0` when the
    ///   deadline is already due (overdue/at-deadline ⇒ zero jump, fire now);
    /// - `landed_vns = max(now_vns, deadline_vns)` — the clock never moves
    ///   backward, and a far-future `deadline_vns` simply yields a large
    ///   advance that [`VClock::advance_idle`](crate::VClock::advance_idle)
    ///   saturates into `vns_base` (no wrap).
    ///
    /// By construction `landed_vns == now_vns + advance_vns` and
    /// `already_due == (advance_vns == 0)`.
    pub fn plan(&self, now_vns: u64, deadline_vns: u64) -> IdleAdvance {
        let advance_vns = deadline_vns.saturating_sub(now_vns);
        IdleAdvance {
            advance_vns,
            landed_vns: now_vns.max(deadline_vns),
            already_due: advance_vns == 0,
        }
    }
}

/// Kani proof harnesses for the idle planner (quality-f). Split into a
/// `#[cfg(kani)]` `#[path]` child (like `clock_proofs.rs`) so cargo-mutants
/// glob-excludes them: they are verified by the dedicated `kani` CI job, not the
/// mutation oracle. See `IMPLEMENTATION.md` ("Formal proofs (Kani)").
#[cfg(kani)]
#[path = "idle_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VClock, VClockConfig};

    /// A 1:1 clock (so `vns(work) == vns_base + work`, exact), with `vns_base`.
    fn clock(vns_base: u64) -> VClock {
        VClock::new(VClockConfig {
            ratio_num: 1,
            ratio_den: 1,
            tsc_hz: 2_000_000_000,
            tsc_base: 0,
            vns_base,
        })
        .expect("valid 1:1 config")
    }

    #[test]
    fn future_deadline_lands_exactly_at_d() {
        let advance = IdlePlanner::new().plan(100, 250);
        assert_eq!(
            advance,
            IdleAdvance {
                advance_vns: 150,
                landed_vns: 250,
                already_due: false,
            }
        );
        // landed == now + advance, and the clock lands at D when applied.
        let mut clk = clock(100); // vns(0) == 100 == now
        clk.advance_idle(advance.advance_vns);
        assert_eq!(clk.vns(0), 250, "the jump lands the clock exactly at D");
    }

    #[test]
    fn overdue_deadline_is_zero_jump() {
        // Deadline strictly in the past: no jump, fire immediately.
        let advance = IdlePlanner::new().plan(500, 200);
        assert_eq!(
            advance,
            IdleAdvance {
                advance_vns: 0,
                landed_vns: 500, // unchanged: clock never moves backward
                already_due: true,
            }
        );
    }

    #[test]
    fn at_deadline_is_zero_jump() {
        // Deadline exactly current: still a zero jump (already due).
        let advance = IdlePlanner::new().plan(300, 300);
        assert_eq!(
            advance,
            IdleAdvance {
                advance_vns: 0,
                landed_vns: 300,
                already_due: true,
            }
        );
    }

    #[test]
    fn far_future_deadline_saturates_without_wrap() {
        // A deadline near u64::MAX from a small `now`: advance is huge but the
        // arithmetic saturates (never wraps), and applying it clamps the clock.
        let advance = IdlePlanner::new().plan(10, u64::MAX);
        assert_eq!(advance.advance_vns, u64::MAX - 10);
        assert_eq!(advance.landed_vns, u64::MAX);
        assert!(!advance.already_due);

        let mut clk = clock(10);
        clk.advance_idle(advance.advance_vns);
        assert_eq!(clk.vns(0), u64::MAX, "clock clamps at the saturation point");
        assert_eq!(clk.vns(5), u64::MAX, "still monotone after saturation");
    }

    #[test]
    fn invariants_hold_for_representative_pairs() {
        let p = IdlePlanner::new();
        for &(now, deadline) in &[
            (0u64, 0u64),
            (0, 1),
            (1, 0),
            (7, 7),
            (7, 9),
            (u64::MAX, 0),
            (0, u64::MAX),
            (u64::MAX, u64::MAX),
        ] {
            let a = p.plan(now, deadline);
            // landed == now + advance (saturating), and never below `now`.
            assert_eq!(a.landed_vns, now.saturating_add(a.advance_vns));
            assert!(a.landed_vns >= now, "clock never moves backward");
            assert_eq!(a.landed_vns, now.max(deadline));
            assert_eq!(a.already_due, a.advance_vns == 0);
            assert_eq!(a.already_due, deadline <= now);
        }
    }
}
