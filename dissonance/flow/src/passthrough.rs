// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`PassthroughEngine`] — the trivial reference engine. Every flow is treated as
//! [`Nominal`](crate::FlowPolicy::Nominal): each chunk is delivered verbatim at
//! the V-time it arrived, a close becomes a teardown [`Reset`](FlowAction::Reset),
//! and the decider is **never** consulted.
//!
//! It serves two purposes. It is the **faults-off baseline** — the recovery /
//! `finally_` case where the proxy must not perturb traffic. And it is the proof
//! that [`FlowEngine`] abstracts over more than toxiproxy: a second, independent
//! implementation satisfying the same trait-generic determinism contract is the
//! pluggability the design requires.

use crate::engine::{FlowDecider, FlowEngine};
use crate::sched::Scheduler;
use crate::{FlowAction, FlowEvent, VTime};

/// The faults-off reference engine: deliver everything verbatim, never consult
/// the decider.
#[derive(Clone, Debug, Default)]
pub struct PassthroughEngine {
    /// The same V-time-ordered queue the toxiproxy engine uses, so both share the
    /// identical deterministic drain order.
    sched: Scheduler,
}

impl PassthroughEngine {
    /// A fresh passthrough engine with an empty action queue.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FlowEngine for PassthroughEngine {
    fn on_event(&mut self, ev: FlowEvent, _decider: &mut dyn FlowDecider) {
        // The decider is deliberately never consulted: passthrough has no policy
        // to fetch (acceptance gate 4).
        match ev {
            // An Open carries no V-time and schedules nothing; the flow is implicit.
            FlowEvent::Open { .. } => {}
            FlowEvent::Chunk {
                conn,
                dir,
                at,
                bytes,
            } => self.sched.schedule(FlowAction::Deliver {
                conn,
                dir,
                bytes,
                at,
            }),
            FlowEvent::Close { conn, at } => self.sched.schedule(FlowAction::Reset { conn, at }),
        }
    }

    fn due(&mut self, now: VTime) -> Vec<FlowAction> {
        self.sched.due(now)
    }
}
