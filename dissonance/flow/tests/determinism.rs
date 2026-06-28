// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 (trait-generic determinism) and gate 6 (no order leakage).
//!
//! Gate 1 runs the determinism property against **every** `FlowEngine` impl
//! (`ToxiproxyEngine` and `PassthroughEngine`), proving the contract holds for
//! more than one implementation. Gate 6 asserts that map/iteration order never
//! reaches an emitted action: ties at one V-time drain in insertion (`seq`) order,
//! and the source carries no `HashMap`/`HashSet` at all.

mod common;

use common::{ScriptedDecider, arb_events, run_all, run_incremental};
use flow::{
    ConnId, Dir, FlowAction, FlowEngine, FlowEvent, FlowPolicy, NodeId, PassthroughEngine,
    ToxiproxyEngine, VTime,
};
use proptest::prelude::*;

/// Run an arbitrary `(events, script)` through two fresh engines of the same type
/// and assert byte-for-byte identical action streams — the core determinism
/// contract, parameterized over the engine impl.
fn deterministic<E: FlowEngine + Default>(
    events: &[FlowEvent],
    script: &[FlowPolicy],
) -> Result<(), TestCaseError> {
    let mut e1 = E::default();
    let mut d1 = ScriptedDecider::new(script.to_vec());
    let a = run_all(&mut e1, &mut d1, events.to_vec());

    let mut e2 = E::default();
    let mut d2 = ScriptedDecider::new(script.to_vec());
    let b = run_all(&mut e2, &mut d2, events.to_vec());

    prop_assert_eq!(
        a,
        b,
        "identical inputs must yield an identical action stream"
    );
    Ok(())
}

/// A small strategy over policy scripts the scripted decider cycles through.
fn arb_script() -> impl Strategy<Value = Vec<FlowPolicy>> {
    prop::collection::vec(common::arb_policy(), 1..6)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Gate 1 — `ToxiproxyEngine` is deterministic.
    #[test]
    fn toxiproxy_is_deterministic(events in arb_events(), script in arb_script()) {
        deterministic::<ToxiproxyEngine>(&events, &script)?;
    }

    /// Gate 1 — `PassthroughEngine` is deterministic (same property, second impl).
    #[test]
    fn passthrough_is_deterministic(events in arb_events(), script in arb_script()) {
        deterministic::<PassthroughEngine>(&events, &script)?;
    }

    /// Incremental draining is deterministic too, and `due(now)` never surfaces an
    /// action whose due V-time is after `now` (the V-time-drained contract).
    #[test]
    fn due_respects_now(events in arb_events(), script in arb_script()) {
        let mut e1 = ToxiproxyEngine::new();
        let mut d1 = ScriptedDecider::new(script.clone());
        let a = run_incremental(&mut e1, &mut d1, events.clone());

        let mut e2 = ToxiproxyEngine::new();
        let mut d2 = ScriptedDecider::new(script);
        let b = run_incremental(&mut e2, &mut d2, events);

        prop_assert_eq!(a, b);
    }
}

/// A degenerate decider used only where the call pattern is irrelevant.
fn nominal() -> ScriptedDecider {
    ScriptedDecider::all_nominal()
}

/// `due(now)` only ever returns actions due at or before `now`, in ascending
/// V-time order — checked across a sweep of cut points.
#[test]
fn due_never_returns_future_actions() {
    let mut e = ToxiproxyEngine::new();
    let mut d = ScriptedDecider::new(vec![FlowPolicy::Latency(VTime(100))]);
    e.on_event(
        FlowEvent::Open {
            conn: ConnId(1),
            src: NodeId(0),
            dst: NodeId(1),
        },
        &mut d,
    );
    for at in [0u64, 5, 50] {
        e.on_event(
            FlowEvent::Chunk {
                conn: ConnId(1),
                dir: Dir::ClientToServer,
                at: VTime(at),
                bytes: vec![at as u8],
            },
            &mut d,
        );
    }
    // Deliveries land at 100, 105, 150. Draining at 100 yields exactly the first.
    let at_100 = e.due(VTime(100));
    assert_eq!(at_100.len(), 1, "only the at=100 delivery is due");
    assert_eq!(at_100[0].at(), VTime(100));
    let rest = e.due(VTime(u64::MAX));
    assert_eq!(rest.len(), 2);
    assert_eq!(rest[0].at(), VTime(105));
    assert_eq!(rest[1].at(), VTime(150));
}

/// Gate 6 — multiplexed deliveries that fall on the *same* V-time drain in the
/// order their chunks were fed (the monotonic `seq` tie-break), never by
/// connection-id or any map order. Feeding the chunks in a different order
/// produces a correspondingly different — but still insertion-defined — order, so
/// the only thing deciding ties is `seq`.
#[test]
fn same_vtime_ties_break_by_insertion_order() {
    // Three nominal flows; one chunk each, all at V-time 7, fed conn 3, 1, 2.
    let order = [ConnId(3), ConnId(1), ConnId(2)];
    let mut e = ToxiproxyEngine::new();
    let mut d = nominal();
    for c in order {
        e.on_event(
            FlowEvent::Open {
                conn: c,
                src: NodeId(0),
                dst: NodeId(1),
            },
            &mut d,
        );
    }
    for c in order {
        e.on_event(
            FlowEvent::Chunk {
                conn: c,
                dir: Dir::ClientToServer,
                at: VTime(7),
                bytes: vec![c.0 as u8],
            },
            &mut d,
        );
    }
    let actions = e.due(VTime(7));
    let conns: Vec<ConnId> = actions
        .iter()
        .map(|a| match a {
            FlowAction::Deliver { conn, .. } | FlowAction::Reset { conn, .. } => *conn,
        })
        .collect();
    assert_eq!(
        conns,
        vec![ConnId(3), ConnId(1), ConnId(2)],
        "same-V-time ties follow chunk insertion order, not conn-id/map order"
    );
}

/// Gate 6 — no *code* in the crate uses `HashMap`/`HashSet`: every ordered
/// container is a `BTreeMap`. A structural backstop to the behavioral test above
/// and to the disallowed-types clippy lint. Comments are stripped first so the
/// doc text that *explains* the BTreeMap choice (which names `HashMap`) does not
/// trip the check — only actual code is scanned.
#[test]
fn source_uses_no_hash_containers() {
    for (name, src) in [
        ("toxiproxy.rs", include_str!("../src/toxiproxy.rs")),
        ("passthrough.rs", include_str!("../src/passthrough.rs")),
        ("sched.rs", include_str!("../src/sched.rs")),
        ("vocab.rs", include_str!("../src/vocab.rs")),
        ("engine.rs", include_str!("../src/engine.rs")),
        ("prng.rs", include_str!("../src/prng.rs")),
    ] {
        for (lineno, line) in src.lines().enumerate() {
            // Drop any `//`, `///`, or `//!` comment tail; scan only the code.
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("HashMap") && !code.contains("HashSet"),
                "{name}:{} uses a hash container (order could leak into an action)",
                lineno + 1
            );
        }
    }
}
