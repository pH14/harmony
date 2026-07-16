// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gate 2 — `FlowEngine::on_event` is a Tier-1 fuzz target. Arbitrary
//! event sequences (arbitrary `Chunk.bytes`, unknown/closed `ConnId`s, duplicate
//! Opens, edge V-times) must never panic, and the engine must be deterministic:
//! replaying the same sequence yields the same `FlowAction` stream.
//!
//! The fuzzer drives a local `Cmd`/`Pol` mirror (which derive `Arbitrary`) and
//! translates it into `flow`'s public vocabulary, so the library stays free of an
//! `arbitrary` dependency. Two engines are exercised: `ToxiproxyEngine` (the
//! stateful one whose stray-ignore and saturation logic this gate targets) and
//! `PassthroughEngine` (which must also never panic).
//!
//! Run (needs the pinned nightly + cargo-fuzz, per the crate IMPLEMENTATION.md):
//!   cargo +nightly-2026-06-16 fuzz run on_event

#![no_main]

use arbitrary::Arbitrary;
use flow::{
    ConnId, Dir, FlowDecider, FlowEngine, FlowEvent, FlowPolicy, NodeId, PassthroughEngine,
    Moment, Span, ToxiproxyEngine,
};
use libfuzzer_sys::fuzz_target;

/// A fuzz-controlled policy mirror.
#[derive(Arbitrary, Debug, Clone)]
enum Pol {
    Nominal,
    Latency(u64),
    Loss { seed: u64, num: u16, den: u16 },
    Throttle { bps: u32 },
    Reset,
}

impl From<Pol> for FlowPolicy {
    fn from(p: Pol) -> Self {
        match p {
            Pol::Nominal => FlowPolicy::Nominal,
            Pol::Latency(d) => FlowPolicy::Latency(Span(d)),
            Pol::Loss { seed, num, den } => FlowPolicy::Loss { seed, num, den },
            Pol::Throttle { bps } => FlowPolicy::Throttle { bps },
            Pol::Reset => FlowPolicy::Reset,
        }
    }
}

/// A fuzz-controlled event mirror.
#[derive(Arbitrary, Debug, Clone)]
enum Cmd {
    Open { conn: u64, src: u32, dst: u32 },
    ChunkC2s { conn: u64, at: u64, bytes: Vec<u8> },
    ChunkS2c { conn: u64, at: u64, bytes: Vec<u8> },
    Close { conn: u64, at: u64 },
    /// Drain everything due at or before this V-time.
    Due { now: u64 },
}

/// The whole fuzz input: a policy script the decider cycles through, plus a
/// command stream.
#[derive(Arbitrary, Debug)]
struct Input {
    script: Vec<Pol>,
    cmds: Vec<Cmd>,
}

/// A decider that cycles a fixed script (empty ⇒ all `Nominal`).
struct ScriptedDecider {
    script: Vec<FlowPolicy>,
    next: usize,
}

impl FlowDecider for ScriptedDecider {
    fn decide_flow(&mut self, _conn: ConnId, _src: NodeId, _dst: NodeId) -> FlowPolicy {
        if self.script.is_empty() {
            return FlowPolicy::Nominal;
        }
        let p = self.script[self.next % self.script.len()].clone();
        self.next += 1;
        p
    }
}

fn to_event(cmd: &Cmd) -> Option<FlowEvent> {
    Some(match cmd {
        Cmd::Open { conn, src, dst } => FlowEvent::Open {
            conn: ConnId(*conn),
            src: NodeId(*src),
            dst: NodeId(*dst),
        },
        Cmd::ChunkC2s { conn, at, bytes } => FlowEvent::Chunk {
            conn: ConnId(*conn),
            dir: Dir::ClientToServer,
            at: Moment(*at),
            bytes: bytes.clone(),
        },
        Cmd::ChunkS2c { conn, at, bytes } => FlowEvent::Chunk {
            conn: ConnId(*conn),
            dir: Dir::ServerToClient,
            at: Moment(*at),
            bytes: bytes.clone(),
        },
        Cmd::Close { conn, at } => FlowEvent::Close {
            conn: ConnId(*conn),
            at: Moment(*at),
        },
        Cmd::Due { .. } => return None,
    })
}

/// Drive one engine over the command stream, collecting every drained action.
fn drive<E: FlowEngine>(mut engine: E, script: &[FlowPolicy], cmds: &[Cmd]) -> Vec<flow::FlowAction> {
    let mut decider = ScriptedDecider {
        script: script.to_vec(),
        next: 0,
    };
    let mut out = Vec::new();
    for cmd in cmds {
        match cmd {
            Cmd::Due { now } => out.extend(engine.due(Moment(*now))),
            other => {
                if let Some(ev) = to_event(other) {
                    engine.on_event(ev, &mut decider);
                }
            }
        }
    }
    // Final full drain so nothing is left pending.
    out.extend(engine.due(Moment(u64::MAX)));
    out
}

fuzz_target!(|input: Input| {
    let script: Vec<FlowPolicy> = input.script.iter().cloned().map(Into::into).collect();

    // No panic, and deterministic: the same input replays identically.
    let a = drive(ToxiproxyEngine::new(), &script, &input.cmds);
    let b = drive(ToxiproxyEngine::new(), &script, &input.cmds);
    assert_eq!(a, b, "ToxiproxyEngine must be deterministic");

    // Passthrough must also never panic on the same stream.
    let _ = drive(PassthroughEngine::new(), &script, &input.cmds);
});
