// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two seams of the flow-fault engine: [`FlowDecider`] (the policy source —
//! the frontier binds it to the `net_decide` hypercall) and [`FlowEngine`] (the
//! pluggable enforcer-brain — the contract `ToxiproxyEngine`/`PassthroughEngine`
//! implement).

use crate::{ConnId, FlowAction, FlowEvent, FlowPolicy, NodeId, Moment};

/// The decision seam: "what should I do with this flow?". The frontier binds it
/// to the `net_decide` hypercall (→ `environment::decide`, which records the
/// answer into the `Moment`-keyed Environment so it replays); in tests it is a
/// scripted or recording fake. The *engine* decides **when** to consult it — see
/// [`FlowEngine::on_event`].
pub trait FlowDecider {
    /// Return the policy for the flow `conn` (from `src` to `dst`). Called by an
    /// engine when it first needs a flow's policy. May mutate the decider's own
    /// state (e.g. to advance a recorded answer cursor).
    fn decide_flow(&mut self, conn: ConnId, src: NodeId, dst: NodeId) -> FlowPolicy;
}

/// A flow-fault engine: [`FlowEvent`]s plus per-flow decisions in, a
/// deterministic, V-time-scheduled stream of concrete [`FlowAction`]s out. The
/// **contract** lives here; the **mechanism** is each implementation's. The
/// engine's whole state lives in guest RAM (the proxy is a guest process), so
/// consonance snapshots/branches it for free — there is no `save_state`.
///
/// Every implementation must honor (the trait-generic gates assert it):
///
/// - **Deterministic** given (engine state, event sequence, decider answers):
///   identical inputs produce an identical [`FlowAction`] sequence — byte-for-byte,
///   including order.
/// - **V-time-drained**: actions surface only through [`due`](FlowEngine::due), at
///   or before `now`, in `(Moment, seq)` order — ties broken by a deterministic
///   monotonic sequence number, never by map-iteration order.
/// - **Total on guest input**: any `Chunk.bytes`, or an event for an
///   unknown/closed [`ConnId`], is handled deterministically — a stray event is
///   ignored, never a panic (conventions rule 4).
/// - **Saturating V-time**: every scheduled time saturates, so a hostile
///   `Latency(u64::MAX)` or an `at` near `u64::MAX` clamps to `u64::MAX` rather
///   than wrapping to deliver in the past.
pub trait FlowEngine {
    /// Feed one connection event. An implementation consults `decider` when it
    /// needs a flow's policy (the toxiproxy engine: once, on
    /// [`Open`](FlowEvent::Open)), schedules any resulting actions, and returns.
    /// Infallible and deterministic; a stray event for an unknown connection is
    /// deterministically ignored.
    fn on_event(&mut self, ev: FlowEvent, decider: &mut dyn FlowDecider);

    /// Pop every action due at or before `now`, in deterministic `(Moment, seq)`
    /// order. Actions scheduled after `now` stay queued for a later call.
    fn due(&mut self, now: Moment) -> Vec<FlowAction>;
}
