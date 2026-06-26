// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — snapshot round-trip: `save_state`/`restore_state` preserve the
//! pending schedule, standing faults, the held-reorder buffer, and the next
//! monotonic `seq` exactly (including a snapshot taken while a reorder is held);
//! `save_state` is byte-identical across two equally-driven switches; a malformed
//! restore errors cleanly and leaves the switch untouched.

mod common;

use common::{FixedOracle, frame, node_map};
use pv_net::{NetAnswer, NetError, NodeId, REORDER_MAX, Switch, VTime};

const L0: u64 = 100;

/// A switch carrying every kind of state: a partition, a throttle with an
/// advanced counter, two pending deliveries, and a held-reorder frame.
fn populated() -> Switch {
    let mut s = Switch::new(node_map(4), VTime(L0));
    s.set_partition(NodeId(0), NodeId(1), (VTime(10), VTime(50)));
    s.set_throttle(
        (NodeId(2), NodeId(3)),
        (2, VTime(100)),
        (VTime(0), VTime(10_000)),
    );

    let mut deliver = FixedOracle(NetAnswer::Deliver);
    s.on_tx(VTime(200), frame(0, 2, 0x10, 4), &mut deliver); // pending
    s.on_tx(VTime(230), frame(2, 3, 0x13, 4), &mut deliver); // pending + advances throttle count

    let mut delay = FixedOracle(NetAnswer::Delay(VTime(30)));
    s.on_tx(VTime(210), frame(1, 3, 0x11, 4), &mut delay); // pending, delayed

    let mut reorder = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(220), frame(0, 3, 0x12, 4), &mut reorder); // held on link (0,3)

    s
}

#[test]
fn round_trip_is_byte_identical() {
    let s1 = populated();
    let bytes = s1.save_state();

    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&bytes).expect("valid blob restores");
    assert_eq!(s2.save_state(), bytes, "restore reproduces the exact bytes");
}

#[test]
fn save_state_is_identical_across_two_equally_driven_switches() {
    assert_eq!(populated().save_state(), populated().save_state());
}

#[test]
fn restore_preserves_future_rx_ordering_with_a_held_reorder() {
    let mut s1 = populated();
    let bytes = s1.save_state();
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&bytes).unwrap();

    // Release the held (0,3) reorder by sending the next frame on that link.
    let mut a = FixedOracle(NetAnswer::Deliver);
    let mut b = FixedOracle(NetAnswer::Deliver);
    let o1 = s1.on_tx(VTime(300), frame(0, 3, 0x99, 4), &mut a);
    let o2 = s2.on_tx(VTime(300), frame(0, 3, 0x99, 4), &mut b);
    assert_eq!(o1, o2, "the held frame releases identically after restore");

    assert_eq!(s1.due(VTime(u64::MAX)), s2.due(VTime(u64::MAX)));
    assert_eq!(s1.save_state(), s2.save_state());
}

#[test]
fn restore_preserves_the_reorder_horizon() {
    let mut s1 = populated();
    let bytes = s1.save_state();
    let mut s2 = Switch::new(node_map(4), VTime(L0));
    s2.restore_state(&bytes).unwrap();

    // The held frame was sent at 220 → horizon 220 + L0 + REORDER_MAX.
    let horizon = 220 + L0 + REORDER_MAX.0;
    assert_eq!(
        s1.due(VTime(horizon)),
        s2.due(VTime(horizon)),
        "the held frame flushes at the same horizon after restore"
    );
    assert_eq!(s1.save_state(), s2.save_state());
}

#[test]
fn malformed_blobs_error_cleanly() {
    let mut s = Switch::new(node_map(4), VTime(L0));
    assert_eq!(s.restore_state(&[]), Err(NetError::Malformed), "empty");
    assert_eq!(
        s.restore_state(&[0, 1, 2]),
        Err(NetError::Malformed),
        "short"
    );
    assert_eq!(
        s.restore_state(&[0xFF; 64]),
        Err(NetError::Malformed),
        "bad magic"
    );

    let good = populated().save_state();
    assert!(s.restore_state(&good).is_ok(), "the baseline blob is valid");

    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(
        s.restore_state(&trailing),
        Err(NetError::Malformed),
        "trailing byte"
    );

    assert_eq!(
        s.restore_state(&good[..good.len() - 1]),
        Err(NetError::Malformed),
        "truncated"
    );
}

#[test]
fn restore_rejects_a_throttle_count_above_max() {
    // `count > max` is unreachable via set_throttle/on_tx, so a blob claiming it
    // is corrupt and must be rejected (else restore would admit/clog a different
    // number of frames than the recording).
    let mut s = Switch::new(node_map(2), VTime(L0));
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (2, VTime(100)),
        (VTime(0), VTime(1000)),
    );
    let mut blob = s.save_state();

    // Layout tail: [...throttle record ending in count:u32][pending_count:u32 = 0]
    // [held_count:u32 = 0]. So the throttle's `count` is the u32 at [n-12 .. n-8].
    let n = blob.len();
    assert_eq!(
        &blob[n - 12..n - 8],
        &0u32.to_le_bytes(),
        "count field located"
    );
    assert_eq!(&blob[n - 8..], &[0u8; 8], "pending+held counts are zero");
    assert!(
        s.restore_state(&blob).is_ok(),
        "the unmodified blob is valid"
    );

    blob[n - 12..n - 8].copy_from_slice(&3u32.to_le_bytes()); // count 3 > max 2
    assert_eq!(
        s.restore_state(&blob),
        Err(NetError::Malformed),
        "count > max is rejected"
    );
}

#[test]
fn a_failed_restore_leaves_the_switch_untouched() {
    let mut s = populated();
    let before = s.save_state();
    assert_eq!(
        s.restore_state(&[0xAB; 17]),
        Err(NetError::Malformed),
        "garbage is rejected"
    );
    assert_eq!(
        s.save_state(),
        before,
        "state is unchanged after a failed restore"
    );
}
