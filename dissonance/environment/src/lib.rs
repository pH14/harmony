// SPDX-License-Identifier: AGPL-3.0-or-later
//! # environment — the `decide` seam, the fault catalog, and the seeded backings
//!
//! `environment` is the heart of **dissonance**: it owns the one seam where a
//! guest meets everything it cannot answer for itself — entropy, scheduling,
//! fuzz payload, and **faults** — and it owns the deterministic backings that
//! answer that seam. A fault is not a separate subsystem here: it is simply the
//! environment answering a service *non-nominally* ("EIO" instead of "ok",
//! "dropped" instead of "delivered"). This crate provides three things: the
//! versioned **catalog** of decision classes and fault kinds ([`DecisionClass`],
//! [`DecisionPoint`], [`Answer`], [`Fault`]) that every service and the explorer
//! share; the **seam** itself ([`Environment::decide`] returning an [`Outcome`]);
//! and the **seeded backings** — [`SeededEnv`] (a pure PRNG answers every
//! decision, no host round-trip) and [`RecordedEnv`] (a seed plus sparse,
//! admissibility-guarded explorer overrides) materialized from the serialized
//! [`EnvSpec`] reproducer. Determinism is the entire point: the same backing
//! over the same [`DecisionPoint`] sequence yields the same [`Answer`] sequence,
//! bit for bit, so every bug dissonance finds replays exactly. Nothing here
//! observes wall-clock time, host entropy, `HashMap`/`HashSet` iteration order,
//! or floating point.
//!
//! ## Module layout
//!
//! [`mod@catalog`] (the shared vocabulary: classes, points, answers, faults) ·
//! `prng` (the local xorshift64\* generator) · `policy` ([`FaultPolicy`], the
//! per-class fault eligibility and probability sampled by [`SeededEnv`]) ·
//! `seeded` ([`SeededEnv`], the pure DST backing) · `recorded` ([`EnvSpec`],
//! [`RecordedEnv`], [`StandingFault`], the reproducer) · `codec` (byte-exact,
//! panic-free serialization shared by the public `encode`/`decode` methods) ·
//! [`mod@error`] (the single [`EnvError`] enum).

mod catalog;
mod codec;
mod error;
mod policy;
mod prng;
mod recorded;
mod seeded;

pub use catalog::{Answer, BlockOp, CorruptSpec, DecisionClass, DecisionPoint, Fault};
pub use error::EnvError;
pub use policy::FaultPolicy;
pub use recorded::{EnvSpec, RecordedEnv, StandingFault};
pub use seeded::SeededEnv;

/// The catalog version. Bumps whenever a [`DecisionClass`] or [`Fault`] is added;
/// it pins the shared vocabulary that the control plane (which names classes in
/// its `StopMask`) and every service agree on. Stable [`DecisionClass`] /
/// [`Fault`] discriminants mean a recorded [`EnvSpec`] keeps replaying across a
/// version bump.
pub const CATALOG_VERSION: u16 = 1;

/// The maximum number of bytes one [`Entropy`](DecisionPoint::Entropy) or
/// [`Payload`](DecisionPoint::Payload) decision may supply. A faultable service
/// clamps its request to this before building the point, so `bytes ≤
/// MAX_SUPPLY_LEN` holds at the seam and a [`Answer::Supply`] can never force an
/// unbounded allocation from an untrusted guest-supplied count (conventions
/// rule 4). The seeded backing also clamps defensively.
pub const MAX_SUPPLY_LEN: u32 = 1 << 20; // 1 MiB

/// What [`Environment::decide`] yields. A pure backing ([`SeededEnv`],
/// [`RecordedEnv`]) always returns [`Outcome::Resolved`]; the (frontier, out of
/// scope) reactive backing may return [`Outcome::NeedsHost`] to suspend the run
/// and ask the explorer over a socket. The variant lives here so the seam is
/// stable across both backings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The decision was answered locally; carries the [`Answer`].
    Resolved(Answer),
    /// The decision must be answered by the host explorer (reactive backing).
    NeedsHost,
}

/// The one seam between fault *policy* (the explorer) and fault *mechanism* (the
/// services). A faultable service consults [`decide`](Environment::decide) before
/// any side effect and acts on the [`Answer`].
pub trait Environment {
    /// Answer one [`DecisionPoint`]. Deterministic given the backing's own state
    /// and the point; never panics, even on a hostile point.
    fn decide(&mut self, point: &DecisionPoint) -> Outcome;
}

/// An in-guest node (a container/process). Mirrors the integration type
/// (conventions rule 2); the integrator unifies it with the routing layer's
/// `NodeId`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodeId(pub u32);

/// A connection identity derived from a frame's 5-tuple, used only for fault
/// *targeting* in a [`DecisionPoint::NetSend`]. Mirrors the integration type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ConnId(pub u64);

/// V-time: a count of retired conditional branches — the project's only
/// deterministic clock. Mirrors the integration type. Fault delays
/// ([`Fault::NetDelay`], [`Fault::BlockLatency`], [`Fault::ProcPause`]) are in
/// these branch-count units.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct VTime(pub u64);

/// The index of a decision since the last branch (monotonic, zero-based). A
/// [`RecordedEnv`] override is keyed by this, so a `Branch`/`Replay` re-applies
/// the right fault at the right decision.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DecisionId(pub u64);
