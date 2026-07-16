// SPDX-License-Identifier: AGPL-3.0-or-later
//! # flow — the central flow-fault proxy engine
//!
//! `flow` is the pure-logic core of the one central L4 proxy that all inter-node
//! traffic routes through inside a harmony guest. Task 50 moved network-fault
//! *enforcement* into the guest: the hypervisor **decides** per flow
//! (`net_decide`), a guest utility **enforces**. This crate is that enforcer's
//! brain, decoupled from sockets and the hypercall — it turns per-flow fault
//! *decisions* into a deterministic, V-time-scheduled stream of concrete
//! connection *actions*.
//!
//! The shape is toxiproxy's: a connection arrives, the engine asks the decider
//! "what should I do with this flow?", and applies the answer by transforming the
//! byte stream — delay, drop, throttle, or reset. Per the integrator's ruling the
//! engine is **pluggable**: the [`FlowEngine`] trait captures the contract and
//! [`ToxiproxyEngine`] is the implementation we ship, so a different fault model
//! can slot in later without touching the proxy shell or the seam.
//! [`PassthroughEngine`] is the trivial reference engine — the faults-off baseline
//! and the proof the trait abstracts over more than one mechanism.
//!
//! ## Determinism
//!
//! The engine's whole state lives in guest RAM (the proxy is a guest process), so
//! consonance snapshots and branches it for free — there is **no `save_state` /
//! `restore_state`** (the win over the retired host-side `pv-net`). Replay
//! determinism is proven by re-running the event sequence, not by serializing
//! state. Concretely: [`Loss`](FlowPolicy::Loss) rolls from a per-connection PRNG
//! seeded by the recorded decision (same decision ⇒ same drops); multiplexed
//! connections are serviced in a deterministic `(Moment, seq)` order, never by
//! incidental map/iteration order; and every scheduled V-time saturates so a
//! hostile delay can never wrap into the past. Nothing here reads a wall-clock,
//! a hash-ordered map into an action, or a float (conventions rule 4).
//!
//! ## Scope
//!
//! Pure logic only. The real `accept`/`splice` TCP proxy, the transparent
//! redirect that routes inter-node traffic through the one central proxy, the
//! [`FlowDecider`] that issues the `net_decide` hypercall and maps
//! `environment::Answer` → [`FlowPolicy`], and the enacting of [`FlowAction`]s on
//! real sockets are all **frontier**, built later against this crate. Per-message
//! / L7 faults belong to a later SDK/L7 tier.
//!
//! ## Module layout
//!
//! [`mod@vocab`] (the shared value types, [`FlowEvent`], [`FlowAction`],
//! [`FlowPolicy`]) · [`mod@engine`] (the [`FlowDecider`] and [`FlowEngine`]
//! seams) · `toxiproxy` ([`ToxiproxyEngine`]) · `passthrough`
//! ([`PassthroughEngine`]) · `sched` (the `(Moment, seq)` action queue) · `prng`
//! (the local xorshift64\* generator).

mod engine;
mod passthrough;
mod prng;
mod sched;
mod toxiproxy;
mod vocab;

pub use engine::{FlowDecider, FlowEngine};
pub use passthrough::PassthroughEngine;
pub use toxiproxy::ToxiproxyEngine;
pub use vocab::{ConnId, Dir, FlowAction, FlowEvent, FlowPolicy, Moment, NodeId, Span};
