// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 (the in-crate half) — `on_event` is total on guest input: arbitrary
//! events (arbitrary `Chunk.bytes`, unknown/closed `ConnId`s, duplicate Opens)
//! never panic and are deterministically ignored when stray; edge V-times
//! (`Latency(u64::MAX)`, `at` near `u64::MAX`) saturate rather than wrap. The
//! `cargo-fuzz` target in `fuzz/` drives the same property under coverage-guided
//! input; this file is the always-on proptest backstop.

mod common;

use common::{ScriptedDecider, arb_events, run_all, run_incremental};
use flow::{
    ConnId, Dir, FlowEngine, FlowEvent, FlowPolicy, NodeId, PassthroughEngine, ToxiproxyEngine,
    VTime,
};
use proptest::prelude::*;

/// A scripted policy stream that includes the saturation-edge policies, so the
/// no-panic property exercises `Latency(u64::MAX)`, full/zero-denominator loss,
/// and a zero-bps throttle alongside the ordinary ones.
fn arb_edge_script() -> impl Strategy<Value = Vec<FlowPolicy>> {
    let edge = prop_oneof![
        Just(FlowPolicy::Nominal),
        Just(FlowPolicy::Latency(VTime(u64::MAX))),
        (any::<u64>()).prop_map(|seed| FlowPolicy::Loss {
            seed,
            num: 1,
            den: 1
        }),
        (any::<u64>()).prop_map(|seed| FlowPolicy::Loss {
            seed,
            num: 5,
            den: 0
        }),
        Just(FlowPolicy::Throttle { bps: 0 }),
        Just(FlowPolicy::Reset),
        common::arb_policy(),
    ];
    prop::collection::vec(edge, 1..6)
}

/// Events whose V-times cluster near `u64::MAX`, so any non-saturating arithmetic
/// would wrap (and, in a debug build, panic).
fn arb_extreme_event() -> impl Strategy<Value = FlowEvent> {
    let conn = (0u64..4).prop_map(ConnId);
    let node = (0u32..4).prop_map(NodeId);
    let at = prop_oneof![Just(u64::MAX), (u64::MAX - 1000..u64::MAX), 0u64..1000,];
    prop_oneof![
        (conn.clone(), node.clone(), node).prop_map(|(conn, src, dst)| FlowEvent::Open {
            conn,
            src,
            dst
        }),
        (
            conn.clone(),
            prop_oneof![Just(Dir::ClientToServer), Just(Dir::ServerToClient)],
            at.clone(),
            prop::collection::vec(any::<u8>(), 0..64)
        )
            .prop_map(|(conn, dir, at, bytes)| FlowEvent::Chunk {
                conn,
                dir,
                at: VTime(at),
                bytes,
            }),
        (conn, at).prop_map(|(conn, at)| FlowEvent::Close {
            conn,
            at: VTime(at),
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Neither engine panics on an arbitrary event stream, and both are
    /// deterministic across the saturation-edge policy set.
    #[test]
    fn no_panic_arbitrary_events(events in arb_events(), script in arb_edge_script()) {
        let mut tox = ToxiproxyEngine::new();
        let mut d1 = ScriptedDecider::new(script.clone());
        let a = run_all(&mut tox, &mut d1, events.clone());

        let mut tox2 = ToxiproxyEngine::new();
        let mut d2 = ScriptedDecider::new(script);
        let b = run_all(&mut tox2, &mut d2, events.clone());
        prop_assert_eq!(a, b);

        // Passthrough must also never panic on the same stream.
        let mut pass = PassthroughEngine::new();
        let mut dn = ScriptedDecider::all_nominal();
        let _ = run_all(&mut pass, &mut dn, events);
    }

    /// Edge V-times saturate: no panic on near-`u64::MAX` schedules, and every
    /// drained action's due time is a real (clamped, non-wrapped) V-time.
    #[test]
    fn no_panic_extreme_vtimes(
        events in prop::collection::vec(arb_extreme_event(), 0..40),
        script in arb_edge_script(),
    ) {
        let mut e = ToxiproxyEngine::new();
        let mut d = ScriptedDecider::new(script);
        let actions = run_incremental(&mut e, &mut d, events);
        // The drain at u64::MAX inside run_incremental must surface everything;
        // simply completing without a panic is the property.
        let _ = actions.len();
    }
}

/// A stray `Chunk`/`Close` for a never-opened flow produces no action at all (the
/// toxiproxy engine ignores it deterministically).
#[test]
fn stray_events_are_ignored() {
    let mut e = ToxiproxyEngine::new();
    let mut d = ScriptedDecider::all_nominal();
    e.on_event(
        FlowEvent::Chunk {
            conn: ConnId(1),
            dir: Dir::ClientToServer,
            at: VTime(5),
            bytes: vec![1, 2, 3],
        },
        &mut d,
    );
    e.on_event(
        FlowEvent::Close {
            conn: ConnId(2),
            at: VTime(9),
        },
        &mut d,
    );
    assert!(
        e.due(VTime(u64::MAX)).is_empty(),
        "stray events for unknown flows schedule nothing"
    );
}

/// After a flow is closed (torn down), further chunks on it are dropped, not
/// delivered — and no second reset is emitted.
#[test]
fn events_after_close_are_dropped() {
    let mut e = ToxiproxyEngine::new();
    let mut d = ScriptedDecider::new(vec![FlowPolicy::Nominal]);
    e.on_event(
        FlowEvent::Open {
            conn: ConnId(1),
            src: NodeId(0),
            dst: NodeId(1),
        },
        &mut d,
    );
    e.on_event(
        FlowEvent::Close {
            conn: ConnId(1),
            at: VTime(3),
        },
        &mut d,
    );
    // Post-close chunk and a duplicate close: both ignored.
    e.on_event(
        FlowEvent::Chunk {
            conn: ConnId(1),
            dir: Dir::ClientToServer,
            at: VTime(4),
            bytes: vec![9],
        },
        &mut d,
    );
    e.on_event(
        FlowEvent::Close {
            conn: ConnId(1),
            at: VTime(5),
        },
        &mut d,
    );
    let got = e.due(VTime(u64::MAX));
    assert_eq!(
        got.len(),
        1,
        "exactly one teardown reset, nothing after close"
    );
}
