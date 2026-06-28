// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`PassthroughEngine`] — the trivial reference engine. Every flow is treated as
//! [`Nominal`](crate::FlowPolicy::Nominal): each chunk of a **live** flow is
//! delivered verbatim at the V-time it arrived, a close becomes a teardown
//! [`Reset`](FlowAction::Reset), and the decider is **never** consulted.
//!
//! It still honors the [`FlowEngine`](crate::FlowEngine) totality contract: an
//! event for an unknown or already-closed [`ConnId`](crate::ConnId) is a stray and
//! is deterministically **ignored** (never delivered, never a spurious reset). The
//! only thing that distinguishes it from [`ToxiproxyEngine`](crate::ToxiproxyEngine)
//! is that it applies no policy and asks no decider — it tracks just enough flow
//! lifecycle to tell a live flow from a stray.
//!
//! It serves two purposes. It is the **faults-off baseline** — the recovery /
//! `finally_` case where the proxy must not perturb traffic. And it is the proof
//! that [`FlowEngine`](crate::FlowEngine) abstracts over more than toxiproxy: a
//! second, independent implementation satisfying the same trait-generic
//! determinism contract is the pluggability the design requires.

use std::collections::BTreeMap;

use crate::engine::{FlowDecider, FlowEngine};
use crate::sched::Scheduler;
use crate::{FlowAction, FlowEvent, VTime};

/// The minimal per-flow lifecycle passthrough needs: the latest delivery already
/// scheduled (so the close reset is ordered after pending data) and whether the
/// flow has been torn down. No policy, no PRNG — passthrough never perturbs
/// traffic; it tracks this only to tell a live flow from a stray.
#[derive(Clone, Debug, Default)]
struct ConnState {
    /// Latest V-time a delivery has been scheduled for on this flow.
    last_deliver: VTime,
    /// Once `true`, the flow is closed; every further event on it is ignored.
    torn: bool,
}

/// The faults-off reference engine: deliver every live flow's bytes verbatim,
/// never consult the decider, ignore strays.
#[derive(Clone, Debug, Default)]
pub struct PassthroughEngine {
    /// Known flows keyed by [`ConnId`](crate::ConnId), so a stray event for an
    /// unopened/closed flow can be ignored. A `BTreeMap`, never a `HashMap` —
    /// though for passthrough no order ever reaches an action anyway.
    conns: BTreeMap<u64, ConnState>,
    /// The same V-time-ordered queue the toxiproxy engine uses, so both share the
    /// identical deterministic drain order.
    sched: Scheduler,
}

impl PassthroughEngine {
    /// A fresh passthrough engine with no live flows and an empty action queue.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FlowEngine for PassthroughEngine {
    fn on_event(&mut self, ev: FlowEvent, _decider: &mut dyn FlowDecider) {
        // The decider is deliberately never consulted: passthrough has no policy
        // to fetch (acceptance gate 4). Open only registers the flow.
        match ev {
            FlowEvent::Open { conn, .. } => {
                // Once per flow: a duplicate/closed-conn Open is left as-is.
                self.conns.entry(conn.0).or_default();
            }
            FlowEvent::Chunk {
                conn,
                dir,
                at,
                bytes,
            } => {
                let Some(state) = self.conns.get_mut(&conn.0) else {
                    // Stray chunk for an unknown flow: ignore deterministically.
                    return;
                };
                if state.torn {
                    // Data after teardown: drop it, never deliver past a close.
                    return;
                }
                if at > state.last_deliver {
                    state.last_deliver = at;
                }
                self.sched.schedule(FlowAction::Deliver {
                    conn,
                    dir,
                    bytes,
                    at,
                });
            }
            FlowEvent::Close { conn, at } => {
                let Some(state) = self.conns.get_mut(&conn.0) else {
                    // Stray close for an unknown flow: ignore (no spurious reset).
                    return;
                };
                if state.torn {
                    // Already closed: a duplicate close schedules nothing.
                    return;
                }
                // Tear down after any still-pending delivery so the reset never
                // precedes delivered data for this flow.
                let when = VTime(at.0.max(state.last_deliver.0));
                self.sched.schedule(FlowAction::Reset { conn, at: when });
                state.torn = true;
            }
        }
    }

    fn due(&mut self, now: VTime) -> Vec<FlowAction> {
        self.sched.due(now)
    }
}
