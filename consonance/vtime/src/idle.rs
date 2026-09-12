// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleAdvance {
    pub advance_vns: u64,
    pub landed_vns: u64,
    pub already_due: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IdlePlanner {
    _seam: (),
}

impl IdlePlanner {
    pub fn new() -> Self {
        IdlePlanner { _seam: () }
    }

    pub fn plan(&self, now_vns: u64, deadline_vns: u64) -> IdleAdvance {
        let advance_vns = deadline_vns.saturating_sub(now_vns);
        IdleAdvance {
            advance_vns,
            landed_vns: now_vns.max(deadline_vns),
            already_due: advance_vns == 0,
        }
    }
}

#[cfg(kani)]
#[path = "idle_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VClock, VClockConfig};

    fn clock(vns_base: u64) -> VClock {
        VClock::new(VClockConfig {
            guest_hz: 2_000_000_000,
            guest_base: 0,
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
        let mut clk = clock(100);
        clk.advance(advance.advance_vns);
        assert_eq!(clk.vns(), 250, "the jump lands the clock exactly at D");
    }

    #[test]
    fn overdue_deadline_is_zero_jump() {
        let advance = IdlePlanner::new().plan(500, 200);
        assert_eq!(
            advance,
            IdleAdvance {
                advance_vns: 0,
                landed_vns: 500,
                already_due: true,
            }
        );
    }

    #[test]
    fn at_deadline_is_zero_jump() {
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
        let advance = IdlePlanner::new().plan(10, u64::MAX);
        assert_eq!(advance.advance_vns, u64::MAX - 10);
        assert_eq!(advance.landed_vns, u64::MAX);
        assert!(!advance.already_due);

        let mut clk = clock(10);
        clk.advance(advance.advance_vns);
        assert_eq!(clk.vns(), u64::MAX, "clock clamps at the saturation point");
        assert_eq!(
            clk.vns(),
            u64::MAX,
            "still saturated after another observation"
        );
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
            assert_eq!(a.landed_vns, now.saturating_add(a.advance_vns));
            assert!(a.landed_vns >= now, "clock never moves backward");
            assert_eq!(a.landed_vns, now.max(deadline));
            assert_eq!(a.already_due, a.advance_vns == 0);
            assert_eq!(a.already_due, deadline <= now);
        }
    }
}
