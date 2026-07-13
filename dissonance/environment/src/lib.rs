// SPDX-License-Identifier: AGPL-3.0-or-later
//! # environment — the two control planes, the catalog, and the reproducer
//!
//! `environment` is the heart of **dissonance**: it models the whole
//! permutation surface as two **control planes** feeding one [`Moment`]-keyed
//! reproducer.
//!
//! - The **guest control plane** is the seam where a guest meets everything it
//!   cannot answer for itself — entropy, scheduling, fuzz payload, and *faults*.
//!   A guest fault is simply the environment answering a service *non-nominally*
//!   ("EIO" instead of "ok", "dropped" instead of "delivered"): an [`Answer`] at
//!   a [`DecisionPoint`], resolved at [`Environment::decide`].
//! - The **host control plane** ([`HostFault`]) is the workload-agnostic,
//!   guest-oblivious surface — memory corruption, clock skew, CPU modulation,
//!   interrupt timing — dissonance imposes on the machine from outside. It has
//!   no service point, so the frontier applies it imperatively at a [`Moment`].
//!
//! Both planes record into one artifact. [`Action`] is the merged vocabulary
//! ([`Host`](Action::Host) ∪ [`Guest`](Action::Guest)); the reproducer
//! [`EnvSpec`] keys its overrides by [`Moment`] (a retired-instruction count),
//! so host and guest overrides share one ordered timeline and the Progression
//! manipulates them uniformly — `(Moment, opaque Action)` — without ever
//! learning an override's plane. This crate provides the versioned **catalog**
//! ([`DecisionClass`], [`DecisionPoint`], [`Answer`], [`Fault`], [`HostFault`]),
//! the **seam** ([`Environment::decide`] → [`Outcome`]), the **seeded backings**
//! ([`SeededEnv`] and [`RecordedEnv`] materialized from [`EnvSpec`]), and the
//! vocabulary-aware [`EnvCodec`] (`seeded`/`mutate`/`compose`) the Progression calls to
//! propose environments. Determinism is the entire point: the same backing over
//! the same inputs yields the same answers, bit for bit, so every bug replays
//! exactly. Nothing here observes wall-clock time, host entropy,
//! `HashMap`/`HashSet` iteration order, or floating point.
//!
//! ## Module layout
//!
//! [`mod@catalog`] (the guest vocabulary: classes, points, answers, faults) ·
//! [`mod@host`] (the host plane: [`HostFault`], [`Action`], [`Moment`],
//! [`Ratio`], [`BitMask`]) · `prng` (the local xorshift64\* generator) ·
//! `policy` ([`FaultPolicy`], the per-class fault eligibility and probability
//! sampled by [`SeededEnv`]) · `seeded` ([`SeededEnv`], the pure DST backing) ·
//! `recorded` ([`EnvSpec`], [`RecordedEnv`], [`StandingFault`], the reproducer) ·
//! `envcodec` ([`EnvCodec`], the proposal seam) · `codec` (byte-exact,
//! panic-free serialization shared by the public `encode`/`decode` methods) ·
//! [`mod@error`] (the single [`EnvError`] enum).

mod catalog;
mod codec;
mod envcodec;
mod error;
mod host;
mod policy;
mod prng;
mod recorded;
mod seeded;

pub use catalog::{Answer, BlockOp, DecisionClass, DecisionPoint, Fault, FlowEvent};
pub use envcodec::EnvCodec;
pub use error::EnvError;
pub use host::{Action, BitMask, HostFault, Moment, Ratio};
pub use policy::FaultPolicy;
pub use recorded::{EnvSpec, RecordedEnv, StandingFault};
pub use seeded::SeededEnv;

/// The catalog version. Bumps whenever a [`DecisionClass`], [`Fault`], or
/// [`HostFault`] is added *or reshaped*; it pins the shared vocabulary that the
/// control plane (which names classes in its `StopMask`) and every service agree
/// on. Stable [`DecisionClass`] / [`HostFault`] discriminants mean a recorded
/// [`EnvSpec`] keeps replaying across a version bump *when the byte forms are
/// compatible*. Bumped to `2` by task 45 (the host control plane: [`HostFault`],
/// [`Action`]); bumped to `3` by task 50, which reshaped the network class from
/// per-frame `NetSend` to per-flow [`NetFlow`](DecisionClass::NetFlow) — the
/// [`DecisionClass`] discriminant `4` is preserved, but the net [`Fault`] byte
/// vocabulary changed incompatibly, so [`EnvSpec::BLOB_VERSION`] bumped in step to
/// reject a stale blob rather than silently reinterpret an old net fault. Bumped
/// to `4` by task 73, which **added** the [`DecisionClass::Buggify`] class
/// (discriminant `7`) and the [`Fault::BuggifyFire`] fault (byte tag `16`) — both
/// additive with stable discriminants, so a recorded blob whose bytes predate
/// them still replays, while a blob that names them fails loudly on an older
/// reader (unknown class / undefined tag).
pub const CATALOG_VERSION: u16 = 4;

/// The maximum number of bytes one [`Entropy`](DecisionPoint::Entropy) or
/// [`Payload`](DecisionPoint::Payload) decision may supply. A faultable service
/// clamps its request to this before building the point, so `bytes ≤
/// MAX_SUPPLY_LEN` holds at the seam and a [`Answer::Supply`] can never force an
/// unbounded allocation from an untrusted guest-supplied count (conventions
/// rule 4). The seeded backing also clamps defensively.
pub const MAX_SUPPLY_LEN: u32 = 1 << 20; // 1 MiB

/// What [`Environment::decide`] yields for a **guest** decision. A pure backing
/// ([`SeededEnv`], [`RecordedEnv`]) always returns [`Outcome::Resolved`]; the
/// (frontier, out of scope) reactive backing may return [`Outcome::NeedsHost`] to
/// suspend the run and ask the explorer over a socket. The variant lives here so
/// the seam is stable across both backings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The decision was answered locally; carries the [`Answer`].
    Resolved(Answer),
    /// The decision must be answered by the host explorer (reactive backing).
    NeedsHost,
}

/// The **guest** control-plane seam: the one boundary between fault *policy* (the
/// explorer) and fault *mechanism* (the services the guest *requests*). A
/// faultable service consults [`decide`](Environment::decide) before any side
/// effect and acts on the [`Answer`] — answering a guest-requested service
/// non-nominally (an [`Answer::Fault`] in place of [`Answer::Nominal`]) *is* the
/// guest fault.
///
/// This trait models the **guest** plane only. Host-plane perturbations
/// ([`HostFault`]: memory/clock/CPU/IRQ) have no service point — the guest never
/// asks for them — so they never flow through [`decide`](Environment::decide); the
/// frontier applies them imperatively at a [`Moment`] (see [`HostFault`] and
/// `tasks/45-host-control-plane.md`). Both planes nonetheless record into one
/// [`Moment`]-keyed reproducer ([`EnvSpec`]) as the merged [`Action`].
pub trait Environment {
    /// Answer one **guest** [`DecisionPoint`] with an [`Answer`]. Deterministic
    /// given the backing's own state and the point; never panics, even on a
    /// hostile point. A [`HostFault`] is never surfaced here — it has no decision
    /// point.
    fn decide(&mut self, point: &DecisionPoint) -> Outcome;
}

/// An in-guest node (a container/process). Mirrors the integration type
/// (conventions rule 2); the integrator unifies it with the routing layer's
/// `NodeId`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodeId(pub u32);

/// A connection identity derived from a flow's 5-tuple, used only for fault
/// *targeting* in a [`DecisionPoint::NetFlow`]. Mirrors the integration type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ConnId(pub u64);

/// A **duration** on the deterministic V-time axis, in retired-conditional-
/// branch counts. Mirrors the integration type. Fault delays
/// ([`Fault::NetLatency`], [`Fault::BlockLatency`], [`Fault::ProcPause`]) and
/// the [`HostFault::SkewTime`] delta are `Span`s; points on the axis are
/// [`Moment`]s. (The GLOSSARY rename of this crate's former `VTime` newtype —
/// same `u64`, same encoded bytes; "V-time" survives as the name of the
/// work-derived clock itself.)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Span(pub u64);
