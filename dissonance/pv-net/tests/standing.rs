// SPDX-License-Identifier: AGPL-3.0-or-later
//! Standing throttle ("clog") behavior: a fixed-window rate limit consulted
//! before the oracle, directional, and active only within its window.

mod common;

use common::{FixedOracle, RecordingOracle, frame, node_map};
use pv_net::{NetAnswer, NodeId, Switch, VTime};

fn switch() -> Switch {
    Switch::new(node_map(2), VTime(100))
}

#[test]
fn admits_up_to_max_then_clogs_then_resets_next_window() {
    let mut s = switch();
    // ≤2 frames per 100 V-time, active over [0, 1000).
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (2, VTime(100)),
        (VTime(0), VTime(1000)),
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);

    // Window index 0 == [0, 100): two admitted, the third clogged.
    assert_eq!(s.on_tx(VTime(0), frame(0, 1, 1, 4), &mut deliver).len(), 1);
    assert_eq!(s.on_tx(VTime(10), frame(0, 1, 2, 4), &mut deliver).len(), 1);
    assert_eq!(s.on_tx(VTime(20), frame(0, 1, 3, 4), &mut deliver).len(), 0);

    // Window index 1 == [100, 200): the budget resets.
    assert_eq!(
        s.on_tx(VTime(100), frame(0, 1, 4, 4), &mut deliver).len(),
        1
    );
}

#[test]
fn inactive_outside_its_window() {
    let mut s = switch();
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (1, VTime(100)),
        (VTime(0), VTime(1000)),
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    // now == 2000 is past the window end: no limiting, every frame delivers.
    for fill in 0..5 {
        assert_eq!(
            s.on_tx(VTime(2000), frame(0, 1, fill, 4), &mut deliver)
                .len(),
            1
        );
    }
}

#[test]
fn is_directional() {
    let mut s = switch();
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (1, VTime(1000)),
        (VTime(0), VTime(1000)),
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    // Exhaust the (0,1) budget.
    assert_eq!(s.on_tx(VTime(0), frame(0, 1, 1, 4), &mut deliver).len(), 1);
    assert_eq!(s.on_tx(VTime(0), frame(0, 1, 2, 4), &mut deliver).len(), 0);
    // The reverse link (1,0) is a different throttle key — unaffected.
    assert_eq!(s.on_tx(VTime(0), frame(1, 0, 3, 4), &mut deliver).len(), 1);
}

#[test]
fn admitted_frames_still_face_the_oracle() {
    let mut s = switch();
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (2, VTime(1000)),
        (VTime(0), VTime(1000)),
    );
    let mut oracle = RecordingOracle::new(NetAnswer::Deliver);
    // Three sends in one window: two admitted (reach the oracle), one clogged
    // (dropped before the oracle is consulted).
    s.on_tx(VTime(0), frame(0, 1, 1, 4), &mut oracle);
    s.on_tx(VTime(0), frame(0, 1, 2, 4), &mut oracle);
    s.on_tx(VTime(0), frame(0, 1, 3, 4), &mut oracle);
    assert_eq!(
        oracle.seen.len(),
        2,
        "only admitted frames reach the oracle"
    );
}

#[test]
fn re_arming_replaces_and_resets_the_counter() {
    let mut s = switch();
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (1, VTime(1000)),
        (VTime(0), VTime(1000)),
    );
    let mut deliver = FixedOracle(NetAnswer::Deliver);
    assert_eq!(s.on_tx(VTime(0), frame(0, 1, 1, 4), &mut deliver).len(), 1);
    assert_eq!(
        s.on_tx(VTime(0), frame(0, 1, 2, 4), &mut deliver).len(),
        0,
        "budget spent"
    );
    // Re-arm the same link: counter resets, so the next send is admitted again.
    s.set_throttle(
        (NodeId(0), NodeId(1)),
        (1, VTime(1000)),
        (VTime(0), VTime(1000)),
    );
    assert_eq!(s.on_tx(VTime(0), frame(0, 1, 3, 4), &mut deliver).len(), 1);
}
