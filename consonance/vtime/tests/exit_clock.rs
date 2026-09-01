// SPDX-License-Identifier: AGPL-3.0-or-later
//! Properties of the explicit exit-count virtual clock.

use proptest::prelude::*;
use vtime::{IdlePlanner, VClock, VClockConfig};

fn clock(base: u64, hz: u64, guest_base: u64) -> VClock {
    VClock::new(VClockConfig {
        guest_hz: hz,
        guest_base,
        vns_base: base,
    })
    .expect("all clock configurations are valid")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn assigned_exit_deltas_are_deterministic(
        base in any::<u64>(),
        deltas in proptest::collection::vec(any::<u64>(), 0..128),
    ) {
        let run = || {
            let mut c = clock(base, 2_000_000_000, 0);
            let mut trace = Vec::new();
            for delta in &deltas {
                c.advance(*delta);
                trace.push(c.vns());
            }
            trace
        };
        prop_assert_eq!(run(), run());
    }

    #[test]
    fn advancement_is_monotone(base in any::<u64>(), deltas in proptest::collection::vec(any::<u64>(), 0..128)) {
        let mut c = clock(base, 1, 0);
        let mut previous = c.vns();
        for delta in deltas {
            c.advance(delta);
            prop_assert!(c.vns() >= previous);
            previous = c.vns();
        }
    }

    #[test]
    fn idle_jump_uses_the_same_accumulator(base in any::<u64>(), deadline in any::<u64>()) {
        let mut c = clock(base, 1, 0);
        let landing = IdlePlanner::new().plan(c.vns(), deadline);
        c.advance(landing.advance_vns);
        prop_assert_eq!(c.vns(), base.max(deadline));
    }
}

#[test]
fn guest_counter_conversion_is_integer_and_saturating() {
    let mut c = clock(0, 2_000_000_000, 5);
    c.advance(7);
    assert_eq!(c.guest_ticks(), 19);
    let c = clock(u64::MAX, u64::MAX, u64::MAX);
    assert_eq!(c.guest_ticks(), u64::MAX);
}
