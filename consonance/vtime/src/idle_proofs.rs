// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::{VClock, VClockConfig};

#[kani::proof]
fn plan_matches_saturating_spec() {
    let now: u64 = kani::any();
    let deadline: u64 = kani::any();

    let a = IdlePlanner::new().plan(now, deadline);

    assert_eq!(a.advance_vns, deadline.saturating_sub(now));
    assert_eq!(a.landed_vns, now.max(deadline));
    assert_eq!(Some(a.landed_vns), now.checked_add(a.advance_vns));
    assert_eq!(a.already_due, a.advance_vns == 0);
    assert_eq!(a.already_due, deadline <= now);
    assert!(a.landed_vns >= now);
}

#[kani::proof]
fn advance_lands_clock_at_deadline_or_clamps() {
    let vns_base: u64 = kani::any();
    let deadline: u64 = kani::any();
    let now = vns_base;

    let a = IdlePlanner::new().plan(now, deadline);
    let mut clk = VClock::new(VClockConfig {
        guest_hz: 1,
        guest_base: 0,
        vns_base,
    })
    .expect("all clock configurations are valid");
    clk.advance(a.advance_vns);
    let landed = clk.vns();

    assert_eq!(landed, a.landed_vns);
    assert!(landed >= now);
    if deadline >= now {
        assert_eq!(landed, deadline);
    }
}
