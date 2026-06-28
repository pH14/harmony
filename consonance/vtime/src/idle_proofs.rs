// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for the idle planner (quality-f), split out of
//! `idle.rs` so cargo-mutants can glob-exclude them: they are `#[cfg(kani)]`
//! and verified by the dedicated `kani` CI job, not the mutation oracle.
//! Declared as `#[cfg(kani)] #[path = "idle_proofs.rs"] mod proofs;` in idle.rs,
//! so it remains a child of `idle` (`use super::*` reaches its items).
//!
//! Unlike `clock_proofs.rs`, none of these touch the `u128` divide in `vns`:
//! the idle planner is pure `u64` saturating arithmetic (`saturating_sub`,
//! `max`), so every input can stay **fully symbolic over all of `u64`** and
//! CBMC still discharges each harness cheaply.

use super::*;
use crate::{VClock, VClockConfig};

/// [`IdlePlanner::plan`] never panics and returns the saturating spec for **all**
/// `now_vns, deadline_vns ∈ u64`:
///   * `advance_vns == deadline.saturating_sub(now)`,
///   * `landed_vns == max(now, deadline) == now + advance` (no wrap; a
///     far-future deadline clamps the advance, never overflowing),
///   * `already_due == (advance == 0) == (deadline <= now)`,
///   * `landed_vns >= now` (the clock never moves backward).
#[kani::proof]
fn plan_matches_saturating_spec() {
    let now: u64 = kani::any();
    let deadline: u64 = kani::any();

    let a = IdlePlanner::new().plan(now, deadline);

    assert_eq!(a.advance_vns, deadline.saturating_sub(now));
    assert_eq!(a.landed_vns, now.max(deadline));
    // landed == now + advance, computed as a checked add: proving this never
    // overflows is the "far-future D clamps, no wrap" guarantee.
    assert_eq!(Some(a.landed_vns), now.checked_add(a.advance_vns));
    assert_eq!(a.already_due, a.advance_vns == 0);
    assert_eq!(a.already_due, deadline <= now);
    assert!(a.landed_vns >= now);
}

/// Applying the planned advance to a 1:1 [`VClock`] lands the clock **exactly at
/// the deadline** for a future deadline, and **clamps to `u64::MAX`** without
/// wrapping when the deadline is unrepresentably far. At ratio 1:1
/// `vns(0) == vns_base`, so the clock's effective V-time at the (frozen) work
/// point is read directly. `vns_base` is bounded only to keep `now := vns_base`
/// a faithful pre-jump clock; the saturation regime is still reached because
/// `deadline` ranges over all of `u64`.
#[kani::proof]
fn advance_lands_clock_at_deadline_or_clamps() {
    let vns_base: u64 = kani::any();
    let deadline: u64 = kani::any();
    // `VClock::new` rejects a 1:1 config with `vns_base == u64::MAX`
    // (`ImmediateSaturation`: `vns(1)` would exceed `u64::MAX`), so exclude that
    // single value — the saturation/no-wrap behavior of the *planner* itself is
    // already proven over all of `u64` by `plan_matches_saturating_spec`.
    kani::assume(vns_base < u64::MAX);
    // The pre-jump effective V-time at work 0 is exactly `vns_base` (1:1 clock).
    let now = vns_base;

    let a = IdlePlanner::new().plan(now, deadline);
    let mut clk = VClock::new(VClockConfig {
        ratio_num: 1,
        ratio_den: 1,
        tsc_hz: 1,
        tsc_base: 0,
        vns_base,
    })
    .expect("1:1 config is always valid");
    clk.advance_idle(a.advance_vns);
    let landed = clk.vns(0);

    // The clock lands at the planner's reported `landed_vns` (== max(now,
    // deadline)); never below `now`; equals `deadline` exactly whenever the
    // deadline is in the future and representable.
    assert_eq!(landed, a.landed_vns);
    assert!(landed >= now);
    if deadline >= now {
        assert_eq!(landed, deadline);
    }
}
