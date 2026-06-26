// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — per-verb golden: a hand-written expectation on the schedule each
//! [`NetAnswer`] (and a standing partition) produces.

mod common;

use common::{FixedOracle, frame, node_map};
use pv_net::{NetAnswer, NetDeliver, NodeId, REORDER_MAX, Switch, VTime};

const L0: u64 = 100;
const NOW: u64 = 1000;

/// A 2-node switch and one A→B frame (payload byte `0xAA`, total 18 bytes).
fn setup() -> (Switch, Vec<u8>) {
    let s = Switch::new(node_map(2), VTime(L0));
    (s, frame(0, 1, 0xAA, 4))
}

fn drive(answer: NetAnswer) -> (Vec<u8>, Vec<NetDeliver>) {
    let (mut s, f) = setup();
    let mut oracle = FixedOracle(answer);
    let out = s.on_tx(VTime(NOW), f.clone(), &mut oracle);
    (f, out)
}

#[test]
fn deliver_one_event_at_t_plus_l0() {
    let (f, out) = drive(NetAnswer::Deliver);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dst, NodeId(1));
    assert_eq!(out[0].at, VTime(NOW + L0));
    assert_eq!(
        out[0].frame, f,
        "nominal delivery is byte-for-byte the input"
    );
}

#[test]
fn drop_no_event() {
    let (_, out) = drive(NetAnswer::Drop);
    assert!(out.is_empty());
}

#[test]
fn delay_one_event_at_t_plus_l0_plus_d() {
    let d = 250;
    let (_, out) = drive(NetAnswer::Delay(VTime(d)));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].at, VTime(NOW + L0 + d));
}

#[test]
fn dup_two_events() {
    let (f, out) = drive(NetAnswer::Dup);
    assert_eq!(out.len(), 2);
    for d in &out {
        assert_eq!(d.at, VTime(NOW + L0));
        assert_eq!(d.dst, NodeId(1));
        assert_eq!(d.frame, f);
    }
}

#[test]
fn corrupt_differs_at_exactly_offset_mod_len() {
    let (f, out) = drive(NetAnswer::Corrupt {
        offset: 3,
        xor: 0xFF,
    });
    assert_eq!(out.len(), 1);
    let got = &out[0].frame;
    assert_eq!(got.len(), f.len());
    for (i, (&a, &b)) in f.iter().zip(got.iter()).enumerate() {
        if i == 3 {
            assert_eq!(b, a ^ 0xFF, "byte {i} is flipped");
        } else {
            assert_eq!(a, b, "byte {i} is untouched");
        }
    }
}

#[test]
fn corrupt_offset_is_reduced_modulo_len() {
    let (_, f) = setup();
    let idx = 3usize;
    let oob = (f.len() + idx) as u32; // out of range; (len+3) % len == 3
    let (_, out) = drive(NetAnswer::Corrupt {
        offset: oob,
        xor: 0x5A,
    });
    assert_eq!(out.len(), 1);
    let got = &out[0].frame;
    let diffs: Vec<usize> = f
        .iter()
        .zip(got.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(diffs, vec![idx], "exactly the modulo'd index differs");
}

#[test]
fn reorder_delivered_after_the_next_frame_on_the_link() {
    let mut s = Switch::new(node_map(2), VTime(L0));
    let f1 = frame(0, 1, 0xAA, 4); // first send, held
    let f2 = frame(0, 1, 0xBB, 4); // next send, delivers normally

    let mut hold = FixedOracle(NetAnswer::Reorder);
    let held = s.on_tx(VTime(NOW), f1.clone(), &mut hold);
    assert!(held.is_empty(), "a reordered frame schedules nothing yet");

    let mut deliver = FixedOracle(NetAnswer::Deliver);
    let out = s.on_tx(VTime(NOW + 10), f2.clone(), &mut deliver);

    // The next frame (f2) comes first, the held frame (f1) after it.
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].frame, f2, "the next frame leads");
    assert_eq!(out[1].frame, f1, "the reordered frame trails");

    // And `due` drains them in the same order.
    let drained = s.due(VTime(u64::MAX));
    assert_eq!(drained, out);
}

#[test]
fn reorder_delivered_after_a_delayed_next_frame() {
    // Regression: the held frame must follow the next frame's *actual* schedule.
    // If the next frame is Delay(d) it lands at T+L0+d; the held frame must not
    // slip ahead of it (seq only tie-breaks at an equal `at`).
    let mut s = Switch::new(node_map(2), VTime(L0));
    let f1 = frame(0, 1, 0xAA, 4); // held
    let f2 = frame(0, 1, 0xBB, 4); // next, delayed

    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(NOW), f1.clone(), &mut hold);

    let d = 500;
    let mut delay = FixedOracle(NetAnswer::Delay(VTime(d)));
    let out = s.on_tx(VTime(NOW + 10), f2.clone(), &mut delay);

    let delayed_at = VTime(NOW + 10 + L0 + d);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].frame, f2, "the delayed next frame leads");
    assert_eq!(out[0].at, delayed_at);
    assert_eq!(out[1].frame, f1, "the reordered frame trails it");
    assert_eq!(
        out[1].at, delayed_at,
        "held frame anchored at the delayed time"
    );
    assert!(
        out[1].at >= out[0].at,
        "held frame never precedes the next frame"
    );
    assert_eq!(s.due(VTime(u64::MAX)), out);
}

#[test]
fn reorder_with_no_later_frame_flushes_once_at_the_horizon() {
    let mut s = Switch::new(node_map(2), VTime(L0));
    let f1 = frame(0, 1, 0xAA, 4);
    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(NOW), f1.clone(), &mut hold);

    let horizon = NOW + L0 + REORDER_MAX.0;

    // Before the horizon: nothing is due, the frame is still held.
    assert!(s.due(VTime(horizon - 1)).is_empty());

    // At the horizon: flushed exactly once, with `at == horizon`.
    let flushed = s.due(VTime(horizon));
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].frame, f1);
    assert_eq!(flushed[0].at, VTime(horizon));
    assert_eq!(flushed[0].dst, NodeId(1));

    // Never again.
    assert!(s.due(VTime(u64::MAX)).is_empty());
}

#[test]
fn expired_held_reorder_released_by_a_late_tx_delivers_at_its_horizon() {
    // A held reorder whose horizon has already passed when the *next* frame
    // finally arrives (and `due` was never called between) must still be bounded
    // by its horizon — delivered AT the horizon, not re-anchored after the late
    // next frame.
    let mut s = Switch::new(node_map(2), VTime(L0));
    let f1 = frame(0, 1, 0xAA, 4); // held
    let mut hold = FixedOracle(NetAnswer::Reorder);
    s.on_tx(VTime(NOW), f1.clone(), &mut hold);

    let horizon = NOW + L0 + REORDER_MAX.0;
    let late = horizon + 1000; // next frame arrives well past the horizon
    let f2 = frame(0, 1, 0xBB, 4);
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    let out = s.on_tx(VTime(late), f2.clone(), &mut deliver);

    assert_eq!(out.len(), 2);
    assert_eq!(
        out[0].frame, f1,
        "the expired held frame delivers at its horizon"
    );
    assert_eq!(out[0].at, VTime(horizon));
    assert_eq!(
        out[1].frame, f2,
        "the late next frame delivers at its own time"
    );
    assert_eq!(out[1].at, VTime(late + L0));
    assert!(
        out[0].at < out[1].at,
        "the held frame is bounded by its horizon, ahead of the late next frame"
    );
    assert_eq!(s.due(VTime(u64::MAX)), out);
}

#[test]
fn partition_drops_within_window_delivers_outside() {
    let window = (VTime(500), VTime(1500)); // half-open [500, 1500)
    let build = || {
        let mut s = Switch::new(node_map(2), VTime(L0));
        s.set_partition(NodeId(0), NodeId(1), window);
        s
    };

    let send_at = |now: u64| {
        let mut s = build();
        let mut oracle = FixedOracle(NetAnswer::Deliver);
        s.on_tx(VTime(now), frame(0, 1, 0xAA, 4), &mut oracle)
    };

    assert!(send_at(499).len() == 1, "before the window: delivered");
    assert!(send_at(500).is_empty(), "window start: dropped");
    assert!(send_at(1499).is_empty(), "inside the window: dropped");
    assert!(
        send_at(1500).len() == 1,
        "window end is exclusive: delivered"
    );
}

#[test]
fn partition_is_undirected() {
    let mut s = Switch::new(node_map(2), VTime(L0));
    s.set_partition(NodeId(1), NodeId(0), (VTime(0), VTime(2000)));
    // Armed as (1,0); a 0→1 send must still be dropped.
    let mut oracle = FixedOracle(NetAnswer::Deliver);
    assert!(
        s.on_tx(VTime(NOW), frame(0, 1, 0xAA, 4), &mut oracle)
            .is_empty()
    );
}
