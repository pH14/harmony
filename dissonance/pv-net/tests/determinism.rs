// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — schedule determinism: identical `(frame sequence, oracle, clock)`
//! yields an identical `NetDeliver` sequence, ties break by the monotonic `seq`
//! (never by map iteration order), and edge V-times saturate without panicking.

mod common;

use common::{FixedOracle, SeededOracle, config, frame, node_map};
use proptest::prelude::*;
use pv_net::{NetAnswer, NetDeliver, Switch, VTime};

/// Drive a scripted run and return (per-send on_tx outputs, full drain, snapshot
/// bytes) — everything that could leak nondeterminism.
fn run(seed: u64, sends: &[(u64, u8, u8, u8)]) -> (Vec<Vec<NetDeliver>>, Vec<NetDeliver>, Vec<u8>) {
    let mut s = Switch::new(node_map(4), VTime(100));
    let mut oracle = SeededOracle::new(seed);
    let mut outs = Vec::new();
    let mut now = 0u64;
    for &(delta, src, dst, fill) in sends {
        now = now.saturating_add(delta);
        let src = src % 4;
        let dst = dst % 4;
        outs.push(s.on_tx(VTime(now), frame(src, dst, fill, 6), &mut oracle));
    }
    let drained = s.due(VTime(u64::MAX));
    (outs, drained, s.save_state())
}

proptest! {
    #![proptest_config(config(256))]

    /// Two runs with the same seed and the same sends produce identical
    /// per-send schedules, identical drains, and byte-identical snapshots. A
    /// `HashMap` reaching any of these would make the property flaky.
    #[test]
    fn schedule_is_a_pure_function_of_inputs(
        seed in any::<u64>(),
        sends in prop::collection::vec((1u64..3000, any::<u8>(), any::<u8>(), any::<u8>()), 1..40),
    ) {
        let a = run(seed, &sends);
        let b = run(seed, &sends);
        prop_assert_eq!(a, b);
    }
}

#[test]
fn ties_break_by_monotonic_seq_not_map_order() {
    // Two sends at the SAME now from the same link, both delivered → two events
    // at the same `at`, distinguished only by insertion (seq) order.
    let mut s = Switch::new(node_map(2), VTime(100));
    let mut oracle = FixedOracle(NetAnswer::Deliver);
    s.on_tx(VTime(1000), frame(0, 1, 0x11, 4), &mut oracle);
    s.on_tx(VTime(1000), frame(0, 1, 0x22, 4), &mut oracle);

    let out = s.due(VTime(u64::MAX));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].at, out[1].at, "same scheduled V-time");
    assert_eq!(out[0].frame[14], 0x11, "first-scheduled delivers first");
    assert_eq!(out[1].frame[14], 0x22, "second-scheduled delivers second");
}

#[test]
fn delay_saturates_to_u64_max() {
    let mut s = Switch::new(node_map(2), VTime(100));
    let mut oracle = FixedOracle(NetAnswer::Delay(VTime(u64::MAX)));
    let out = s.on_tx(VTime(1000), frame(0, 1, 0xAA, 4), &mut oracle);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].at,
        VTime(u64::MAX),
        "T + L0 + d saturates, never wraps"
    );
}

#[test]
fn now_near_u64_max_saturates() {
    let mut s = Switch::new(node_map(2), VTime(100));
    let mut oracle = FixedOracle(NetAnswer::Deliver);
    let out = s.on_tx(VTime(u64::MAX - 1), frame(0, 1, 0xAA, 4), &mut oracle);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].at, VTime(u64::MAX), "now + L0 clamps to u64::MAX");
    // And it is drained by a max-time `due` rather than lost to a wrap.
    assert_eq!(s.due(VTime(u64::MAX)).len(), 1);
}

#[test]
fn reorder_horizon_saturates_and_still_flushes() {
    let mut s = Switch::new(node_map(2), VTime(100));
    let mut oracle = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(u64::MAX), frame(0, 1, 0xAA, 4), &mut oracle);
    // Horizon = saturating(u64::MAX + L0 + REORDER_MAX) == u64::MAX.
    let flushed = s.due(VTime(u64::MAX));
    assert_eq!(
        flushed.len(),
        1,
        "a saturated-horizon reorder still flushes"
    );
    assert_eq!(flushed[0].at, VTime(u64::MAX));
}
