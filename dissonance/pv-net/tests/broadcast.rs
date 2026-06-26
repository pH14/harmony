// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — broadcast: a broadcast frame yields one decision + delivery per
//! non-source node, and the per-destination oracle consultations happen in
//! sorted `NodeId` order.

mod common;

use common::{RecordingOracle, bcast, node_map};
use pv_net::{NetAnswer, NodeId, Switch, VTime};

#[test]
fn broadcast_fans_out_to_every_other_node_in_sorted_order() {
    let mut s = Switch::new(node_map(5), VTime(100));
    let mut oracle = RecordingOracle::new(NetAnswer::Deliver);

    // Source is node 2; the broadcast must reach 0,1,3,4 (not 2).
    let out = s.on_tx(VTime(1000), bcast(2, 0xAA, 8), &mut oracle);

    let drawn: Vec<NodeId> = oracle.seen.iter().map(|(_, snd)| snd.dst).collect();
    assert_eq!(
        drawn,
        vec![NodeId(0), NodeId(1), NodeId(3), NodeId(4)],
        "oracle is consulted once per non-source node, in sorted NodeId order"
    );

    // One delivery per consulted node, same destinations, same order.
    let delivered: Vec<NodeId> = out.iter().map(|d| d.dst).collect();
    assert_eq!(delivered, vec![NodeId(0), NodeId(1), NodeId(3), NodeId(4)]);
    for d in &out {
        assert_eq!(d.at, VTime(1100));
    }
}

#[test]
fn broadcast_source_is_excluded_even_with_one_peer() {
    let mut s = Switch::new(node_map(2), VTime(100));
    let mut oracle = RecordingOracle::new(NetAnswer::Deliver);
    let out = s.on_tx(VTime(1000), bcast(0, 0xAA, 4), &mut oracle);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dst, NodeId(1));
    assert_eq!(
        oracle.seen.len(),
        1,
        "exactly one decision, for the one peer"
    );
}
