// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 (per-policy golden, `ToxiproxyEngine`), gate 4 (`PassthroughEngine` is
//! nominal and never consults the decider), and the saturation half of gate 2.
//!
//! Each golden pins one `FlowPolicy`'s exact action schedule against a
//! hand-written expectation — the toxic semantics and the seeded PRNG frozen
//! against drift. The `Loss` expectations are the kept/dropped set for a known
//! seed, derived from the documented xorshift64\* stream.

mod common;

use common::{RecordingDecider, ScriptedDecider, run_all};
use flow::{
    ConnId, Dir, FlowAction, FlowEvent, FlowPolicy, NodeId, PassthroughEngine, ToxiproxyEngine,
    VTime,
};

const C: ConnId = ConnId(1);

fn open(policy: FlowPolicy) -> (ToxiproxyEngine, ScriptedDecider, Vec<FlowEvent>) {
    let engine = ToxiproxyEngine::new();
    let decider = ScriptedDecider::new(vec![policy]);
    let events = vec![FlowEvent::Open {
        conn: C,
        src: NodeId(0),
        dst: NodeId(1),
    }];
    (engine, decider, events)
}

fn chunk(at: u64, dir: Dir, bytes: &[u8]) -> FlowEvent {
    FlowEvent::Chunk {
        conn: C,
        dir,
        at: VTime(at),
        bytes: bytes.to_vec(),
    }
}

fn deliver(at: u64, dir: Dir, bytes: &[u8]) -> FlowAction {
    FlowAction::Deliver {
        conn: C,
        dir,
        bytes: bytes.to_vec(),
        at: VTime(at),
    }
}

fn reset(at: u64) -> FlowAction {
    FlowAction::Reset {
        conn: C,
        at: VTime(at),
    }
}

const C2S: Dir = Dir::ClientToServer;
const S2C: Dir = Dir::ServerToClient;

/// `Nominal` → each chunk delivered verbatim at its arrival V-time; close becomes
/// a teardown reset, ordered at/after the last delivery.
#[test]
fn golden_nominal() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Nominal);
    ev.push(chunk(3, C2S, &[9]));
    ev.push(chunk(8, S2C, &[8]));
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(8),
    });
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(
        got,
        vec![
            deliver(3, C2S, &[9]),
            // Deliver@8 was scheduled before the close's reset@8, so it wins the tie.
            deliver(8, S2C, &[8]),
            reset(8),
        ]
    );
}

/// `Latency(d)` → each chunk delivered at `at + d`; the close reset is ordered
/// after the latest delayed delivery (`max(close_at, last_deliver)`).
#[test]
fn golden_latency() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Latency(VTime(5)));
    ev.push(chunk(0, C2S, &[1]));
    ev.push(chunk(10, C2S, &[2]));
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(20),
    });
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(
        got,
        vec![
            deliver(5, C2S, &[1]),
            deliver(15, C2S, &[2]),
            reset(20), // max(close@20, last_deliver@15)
        ]
    );
}

/// `Throttle{bps}` → bytes paced at `bps` per V-time unit. A 25-byte chunk at
/// `bps=10` costs `ceil(25/10)=3`; the transmit cursor carries forward so
/// back-to-back chunks queue behind one another.
#[test]
fn golden_throttle() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Throttle { bps: 10 });
    ev.push(chunk(0, C2S, &[0; 25])); // start 0, cost 3 -> 3, cursor=3
    ev.push(chunk(1, C2S, &[1; 25])); // start max(1,3)=3, cost 3 -> 6, cursor=6
    ev.push(chunk(10, C2S, &[2; 5])); // start max(10,6)=10, cost 1 -> 11
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(
        got,
        vec![
            deliver(3, C2S, &[0; 25]),
            deliver(6, C2S, &[1; 25]),
            deliver(11, C2S, &[2; 5]),
        ]
    );
}

/// Throttle paces each direction on its own cursor: a server→client chunk is not
/// delayed by client→server traffic. Two chunks land on the same V-time (3) and
/// drain in insertion order (gate-6 tie-break).
#[test]
fn golden_throttle_is_per_direction() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Throttle { bps: 10 });
    ev.push(chunk(0, C2S, &[10; 25])); // C2S cursor: 0 -> 3
    ev.push(chunk(1, C2S, &[11; 25])); // C2S cursor: 3 -> 6
    ev.push(chunk(0, S2C, &[20; 25])); // S2C cursor: 0 -> 3 (independent)
    ev.push(chunk(10, C2S, &[12; 5])); // C2S: 10 -> 11
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(
        got,
        vec![
            deliver(3, C2S, &[10; 25]), // seq 0
            deliver(3, S2C, &[20; 25]), // seq 2, same V-time -> after seq 0
            deliver(6, C2S, &[11; 25]),
            deliver(11, C2S, &[12; 5]),
        ]
    );
}

/// `Loss{seed,num,den}` → the exact kept/dropped set for a known seed. With
/// `seed=0xC0FFEE`, `1/2`, the documented xorshift64\* stream drops chunks at
/// indices 2, 4, 5 of the first eight (`roll % 2 == 0`).
#[test]
fn golden_loss_one_in_two() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Loss {
        seed: 0xC0FFEE,
        num: 1,
        den: 2,
    });
    for i in 0u64..8 {
        ev.push(chunk(i, C2S, &[i as u8]));
    }
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(8),
    });
    let got = run_all(&mut e, &mut d, ev);
    // Kept: 0,1,3,6,7 ; dropped: 2,4,5. Reset after the last delivery (@7) -> 8.
    assert_eq!(
        got,
        vec![
            deliver(0, C2S, &[0]),
            deliver(1, C2S, &[1]),
            deliver(3, C2S, &[3]),
            deliver(6, C2S, &[6]),
            deliver(7, C2S, &[7]),
            reset(8),
        ]
    );
}

/// `Loss` at `1/1` is a full drop: every chunk vanishes and only the close
/// teardown survives.
#[test]
fn golden_loss_full_drop() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Loss {
        seed: 42,
        num: 1,
        den: 1,
    });
    for i in 0u64..5 {
        ev.push(chunk(i, C2S, &[i as u8]));
    }
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(9),
    });
    let got = run_all(&mut e, &mut d, ev);
    // Nothing delivered (last_deliver stays 0), so the reset lands at the close.
    assert_eq!(got, vec![reset(9)]);
}

/// `Loss` with `den == 0` is a no-op (deliver) — a malformed fraction never
/// divides by zero.
#[test]
fn golden_loss_zero_denominator_delivers() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Loss {
        seed: 7,
        num: 3,
        den: 0,
    });
    ev.push(chunk(2, C2S, &[1, 2, 3]));
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![deliver(2, C2S, &[1, 2, 3])]);
}

/// `Reset` → a single teardown at the first event carrying a V-time, then nothing
/// further is delivered.
#[test]
fn golden_reset() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Reset);
    ev.push(chunk(4, C2S, &[1]));
    ev.push(chunk(9, C2S, &[2]));
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(12),
    });
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![reset(4)]); // first chunk tears down; rest dropped
}

/// `Reset` with no data: only an Open then a Close still produces exactly one
/// teardown, at the close's V-time.
#[test]
fn golden_reset_close_only() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Reset);
    ev.push(FlowEvent::Close {
        conn: C,
        at: VTime(3),
    });
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![reset(3)]);
}

// ---- Gate 4 — PassthroughEngine is nominal, decider never consulted ----

/// Passthrough delivers every chunk verbatim at its arrival V-time, resets on
/// close, and — the load-bearing assertion — never consults the decider.
#[test]
fn passthrough_is_nominal_and_never_decides() {
    let mut e = PassthroughEngine::new();
    let mut d = RecordingDecider::all_nominal();
    let events = vec![
        FlowEvent::Open {
            conn: C,
            src: NodeId(0),
            dst: NodeId(1),
        },
        chunk(3, C2S, &[1, 2, 3]),
        chunk(8, S2C, &[4, 5]),
        FlowEvent::Close {
            conn: C,
            at: VTime(8),
        },
    ];
    let got = run_all(&mut e, &mut d, events);
    assert_eq!(
        got,
        vec![
            deliver(3, C2S, &[1, 2, 3]),
            deliver(8, S2C, &[4, 5]),
            reset(8),
        ]
    );
    assert!(
        d.calls.is_empty(),
        "PassthroughEngine must never consult the decider"
    );
}

/// Passthrough is stateless about opens: a chunk with no preceding Open is still
/// delivered verbatim (it has no per-flow gate to make an event "stray").
#[test]
fn passthrough_delivers_without_open() {
    let mut e = PassthroughEngine::new();
    let mut d = RecordingDecider::all_nominal();
    let got = run_all(&mut e, &mut d, vec![chunk(5, C2S, &[7])]);
    assert_eq!(got, vec![deliver(5, C2S, &[7])]);
    assert!(d.calls.is_empty());
}

// ---- Saturation edges (gate 2) ----

/// `Latency(u64::MAX)` clamps the delivery time to `u64::MAX` rather than wrapping
/// into the past; the delivery is still drainable at `u64::MAX`.
#[test]
fn latency_saturates_at_u64_max() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Latency(VTime(u64::MAX)));
    ev.push(chunk(5, C2S, &[1]));
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![deliver(u64::MAX, C2S, &[1])]);
}

/// An `at` near `u64::MAX` plus any delay also clamps, never wraps.
#[test]
fn latency_saturates_when_at_is_near_max() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Latency(VTime(10)));
    ev.push(chunk(u64::MAX - 3, C2S, &[1]));
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![deliver(u64::MAX, C2S, &[1])]);
}

/// `Throttle{bps:0}` stalls the flow: deliveries clamp to `u64::MAX` instead of
/// dividing by zero.
#[test]
fn throttle_zero_bps_saturates() {
    let (mut e, mut d, mut ev) = open(FlowPolicy::Throttle { bps: 0 });
    ev.push(chunk(5, C2S, &[1, 2, 3]));
    let got = run_all(&mut e, &mut d, ev);
    assert_eq!(got, vec![deliver(u64::MAX, C2S, &[1, 2, 3])]);
}
