// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proofs for the exit-count virtual clock.

use super::{VClock, VClockConfig};

/// Explicit advancement is monotone for every initial value and delta.
#[kani::proof]
fn advance_is_monotone() {
    let initial: u64 = kani::any();
    let delta: u64 = kani::any();
    let mut clock = VClock::new(VClockConfig {
        guest_hz: kani::any(),
        guest_base: kani::any(),
        vns_base: initial,
    })
    .expect("all configurations are valid");
    clock.advance(delta);
    assert!(clock.vns() >= initial);
}

/// The guest counter conversion never panics for any clock configuration.
#[kani::proof]
fn guest_ticks_is_total() {
    let clock = VClock::new(VClockConfig {
        guest_hz: kani::any(),
        guest_base: kani::any(),
        vns_base: kani::any(),
    })
    .expect("all configurations are valid");
    let _ = clock.guest_ticks();
}
