// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers: the decider fakes (scripted + recording), proptest
//! strategies over the event/policy vocabulary, and a runner that drives an
//! engine over an event sequence and drains every action. Each `tests/*.rs`
//! pulls only what it needs.
#![allow(dead_code)]

use flow::{
    ConnId, Dir, FlowAction, FlowDecider, FlowEngine, FlowEvent, FlowPolicy, Moment, NodeId, Span,
};
use proptest::prelude::*;

/// A decider that hands back a fixed script of policies, one per `decide_flow`
/// call, cycling so it never runs dry. Deterministic given the script.
pub struct ScriptedDecider {
    script: Vec<FlowPolicy>,
    next: usize,
}

impl ScriptedDecider {
    pub fn new(script: Vec<FlowPolicy>) -> Self {
        Self { script, next: 0 }
    }

    /// A decider that answers every flow `Nominal` (used when the policy does not
    /// matter, only the call pattern).
    pub fn all_nominal() -> Self {
        Self::new(vec![FlowPolicy::Nominal])
    }
}

impl FlowDecider for ScriptedDecider {
    fn decide_flow(&mut self, _conn: ConnId, _src: NodeId, _dst: NodeId) -> FlowPolicy {
        let policy = self.script[self.next % self.script.len()].clone();
        self.next += 1;
        policy
    }
}

/// A decider that records the `(conn, src, dst)` of every consultation in call
/// order, then answers from an inner script. Lets a test assert *how often* and
/// *in what order* the engine consults the decider (acceptance gates 4 and 5).
pub struct RecordingDecider {
    pub calls: Vec<(ConnId, NodeId, NodeId)>,
    inner: ScriptedDecider,
}

impl RecordingDecider {
    pub fn new(script: Vec<FlowPolicy>) -> Self {
        Self {
            calls: Vec::new(),
            inner: ScriptedDecider::new(script),
        }
    }

    pub fn all_nominal() -> Self {
        Self::new(vec![FlowPolicy::Nominal])
    }

    /// The `ConnId`s consulted, in call order.
    pub fn conn_order(&self) -> Vec<ConnId> {
        self.calls.iter().map(|(c, _, _)| *c).collect()
    }
}

impl FlowDecider for RecordingDecider {
    fn decide_flow(&mut self, conn: ConnId, src: NodeId, dst: NodeId) -> FlowPolicy {
        self.calls.push((conn, src, dst));
        self.inner.decide_flow(conn, src, dst)
    }
}

/// Feed `events` to `engine` through `decider`, then drain every action with a
/// single `due(u64::MAX)`. Returns the full, ordered action stream.
pub fn run_all<E: FlowEngine>(
    engine: &mut E,
    decider: &mut dyn FlowDecider,
    events: Vec<FlowEvent>,
) -> Vec<FlowAction> {
    for ev in events {
        engine.on_event(ev, decider);
    }
    engine.due(Moment(u64::MAX))
}

/// Feed `events`, draining after each event at that event's own V-time, then a
/// final full drain. Exercises the incremental `due(now)` contract — actions must
/// surface only at or before `now` — and still yields the complete stream.
pub fn run_incremental<E: FlowEngine>(
    engine: &mut E,
    decider: &mut dyn FlowDecider,
    events: Vec<FlowEvent>,
) -> Vec<FlowAction> {
    let mut out = Vec::new();
    for ev in &events {
        let now = event_time(ev);
        engine.on_event(ev.clone(), decider);
        if let Some(now) = now {
            out.extend(engine.due(now));
        }
    }
    out.extend(engine.due(Moment(u64::MAX)));
    out
}

/// The V-time an event carries, if any (`Open` carries none).
fn event_time(ev: &FlowEvent) -> Option<Moment> {
    match ev {
        FlowEvent::Open { .. } => None,
        FlowEvent::Chunk { at, .. } | FlowEvent::Close { at, .. } => Some(*at),
    }
}

// ---- proptest strategies over the vocabulary ----

/// An arbitrary direction.
pub fn arb_dir() -> impl Strategy<Value = Dir> {
    prop_oneof![Just(Dir::ClientToServer), Just(Dir::ServerToClient)]
}

/// An arbitrary policy across the whole `FlowPolicy` surface, with bounded
/// parameters so a generated `Throttle`/`Latency` still produces observable
/// (non-saturated) schedules most of the time.
pub fn arb_policy() -> impl Strategy<Value = FlowPolicy> {
    prop_oneof![
        Just(FlowPolicy::Nominal),
        (0u64..1_000).prop_map(|d| FlowPolicy::Latency(Span(d))),
        (any::<u64>(), 0u16..8, 1u16..8).prop_map(|(seed, num, den)| FlowPolicy::Loss {
            seed,
            num,
            den
        }),
        (1u32..64).prop_map(|bps| FlowPolicy::Throttle { bps }),
        Just(FlowPolicy::Reset),
    ]
}

/// An arbitrary event over a small connection-id space (so flows collide and the
/// engine sees multiplexing, duplicate opens, and strays). V-times are bounded
/// well below `u64::MAX` so ordinary schedules do not all saturate; the
/// saturation edges are exercised by dedicated tests.
pub fn arb_event() -> impl Strategy<Value = FlowEvent> {
    let conn = (0u64..4).prop_map(ConnId);
    let node = (0u32..4).prop_map(NodeId);
    prop_oneof![
        (conn.clone(), node.clone(), node).prop_map(|(conn, src, dst)| FlowEvent::Open {
            conn,
            src,
            dst
        }),
        (
            conn.clone(),
            arb_dir(),
            0u64..10_000,
            prop::collection::vec(any::<u8>(), 0..32)
        )
            .prop_map(|(conn, dir, at, bytes)| FlowEvent::Chunk {
                conn,
                dir,
                at: Moment(at),
                bytes,
            }),
        (conn, 0u64..10_000).prop_map(|(conn, at)| FlowEvent::Close {
            conn,
            at: Moment(at),
        }),
    ]
}

/// A bounded arbitrary event sequence.
pub fn arb_events() -> impl Strategy<Value = Vec<FlowEvent>> {
    prop::collection::vec(arb_event(), 0..40)
}
