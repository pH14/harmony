// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — decider-driven (`ToxiproxyEngine`). The decider is consulted exactly
//! **once per flow, on `Open`**, and in deterministic order across multiplexed
//! flows: the draw order is the `Open` order, which a `HashMap`-backed connection
//! set could not guarantee.

mod common;

use std::collections::BTreeSet;

use common::{RecordingDecider, arb_events};
use flow::{ConnId, Dir, FlowEngine, FlowEvent, NodeId, ToxiproxyEngine, Moment};
use proptest::prelude::*;

fn open(conn: u64, src: u32, dst: u32) -> FlowEvent {
    FlowEvent::Open {
        conn: ConnId(conn),
        src: NodeId(src),
        dst: NodeId(dst),
    }
}

fn chunk(conn: u64, at: u64) -> FlowEvent {
    FlowEvent::Chunk {
        conn: ConnId(conn),
        dir: Dir::ClientToServer,
        at: Moment(at),
        bytes: vec![0xAB],
    }
}

/// The decider is consulted once per flow, on Open, in Open order — not on
/// chunks, not on a duplicate Open, and not on a stray event for an unopened conn.
#[test]
fn consulted_once_per_flow_in_open_order() {
    let mut e = ToxiproxyEngine::new();
    let mut d = RecordingDecider::all_nominal();

    // A stray chunk/close for never-opened flows must not consult the decider.
    e.on_event(chunk(9, 0), &mut d);
    e.on_event(
        FlowEvent::Close {
            conn: ConnId(8),
            at: Moment(1),
        },
        &mut d,
    );

    // Opens in a deliberately non-sorted order: 3, 1, 2.
    e.on_event(open(3, 0, 1), &mut d);
    e.on_event(open(1, 0, 1), &mut d);
    e.on_event(open(2, 0, 1), &mut d);

    // A duplicate Open for an already-open flow must not re-consult.
    e.on_event(open(3, 7, 7), &mut d);

    // Chunks for each flow must not consult.
    for c in [1, 2, 3] {
        e.on_event(chunk(c, 5), &mut d);
    }

    assert_eq!(
        d.conn_order(),
        vec![ConnId(3), ConnId(1), ConnId(2)],
        "decider draw order is the Open order; each flow consulted exactly once"
    );
}

/// The `(conn, src, dst)` the engine passes the decider is exactly the Open's
/// triple (so a policy can depend on the link).
#[test]
fn decider_receives_open_triple() {
    let mut e = ToxiproxyEngine::new();
    let mut d = RecordingDecider::all_nominal();
    e.on_event(open(42, 5, 9), &mut d);
    assert_eq!(d.calls, vec![(ConnId(42), NodeId(5), NodeId(9))]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Over any event sequence, the decider is consulted exactly once per distinct
    /// flow that is opened — equal to the number of Opens for a not-yet-seen conn.
    #[test]
    fn consult_count_equals_distinct_opens(events in arb_events()) {
        let mut seen = BTreeSet::new();
        let mut expected = 0usize;
        for ev in &events {
            if let FlowEvent::Open { conn, .. } = ev
                && seen.insert(conn.0)
            {
                expected += 1;
            }
        }

        let mut e = ToxiproxyEngine::new();
        let mut d = RecordingDecider::all_nominal();
        for ev in events {
            e.on_event(ev, &mut d);
        }
        prop_assert_eq!(d.calls.len(), expected);
    }
}
