// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **host control plane**: substrate-level perturbations imposed on the
//! machine from outside, with no guest service point.
//!
//! Where a guest fault is the environment answering a service *non-nominally*
//! (an [`Answer`](crate::Answer) at a [`DecisionPoint`](crate::DecisionPoint)),
//! a [`HostFault`] is something dissonance does *to* a guest that is merely
//! spinning — memory corruption, clock skew, CPU modulation, interrupt timing.
//! It is guest-oblivious, workload-agnostic, and identical for every guest
//! world. The two planes meet in one reproducer: [`Action`] is the merged
//! vocabulary ([`Host`](Action::Host) ∪ [`Guest`](Action::Guest)) keyed on the
//! single [`Moment`] axis, so the search loop orders and manipulates overrides
//! uniformly without knowing which plane any one belongs to.

use crate::Span;
use crate::catalog::Answer;
use crate::codec::{self, Reader};
use crate::error::EnvError;

/// The single deterministic time axis: **the deterministic V-time axis** (`vns`,
/// effective virtual nanoseconds) — the same axis as `run(deadline)`, snapshot
/// addressing, and `state_hash` points. Retired-instruction work counts are the
/// *derivation* of this axis, not its unit (they coincide only at clock ratio 1);
/// see the integrator ruling in `docs/INTEGRATION.md` §6b. Every override — host
/// *and* guest — is keyed by a `Moment`, which is what lets the search loop treat
/// them as one ordered timeline (`(Moment, opaque Action)`) without learning an
/// override's plane. Virtual time (whose durations are [`Span`]s) is a *derived view* of this same
/// axis, not a second clock.
///
/// A bare `u64` alias, exactly as the dissonance ruling specifies — a `Moment`
/// is an absolute position on this axis, not a typed handle, so the codec and
/// `EnvCodec`'s re-keying are plain integer arithmetic.
pub type Moment = u64;

/// An exact, **float-free** rational — the `SetClockRate` knob (retired-branches
/// → V-time slope) and any other fixed-point ratio the host plane needs. The
/// denominator is always `≥ 1` (a constructor and the decoder both reject zero),
/// so a consumer can divide by it without a panic and no float ever reaches
/// state (conventions rule 4).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Ratio {
    num: u64,
    den: u64,
}

impl Ratio {
    /// Build a ratio `num/den`. Returns `None` when `den == 0` (a zero
    /// denominator is meaningless and would later divide-by-zero), so every
    /// constructed `Ratio` is valid and `encode`/`decode` round-trips.
    pub fn new(num: u64, den: u64) -> Option<Self> {
        (den != 0).then_some(Self { num, den })
    }

    /// The numerator.
    pub fn num(self) -> u64 {
        self.num
    }

    /// The denominator (always `≥ 1`).
    pub fn den(self) -> u64 {
        self.den
    }
}

/// An XOR bit-flip pattern applied to one guest-physical word — the payload of a
/// [`CorruptMemory`](HostFault::CorruptMemory) single-event-upset. Any `u64` is a
/// valid mask, so it round-trips unconditionally; the upset at a `Moment` is the
/// pure function `word ^ mask`, which is why replay re-applies it bit-for-bit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct BitMask(pub u64);

/// A host control-plane perturbation — the workload-agnostic surface that
/// "punches straight through to the hypervisor". Applied from outside, between
/// instructions, at a chosen [`Moment`]; there is no service point, so it never
/// flows through [`Environment::decide`](crate::Environment::decide). The
/// frontier (`consonance/vmm-core`) enforces each one at its `Moment` during a
/// run; this crate defines, stamps, and round-trips them.
///
/// The byte form ([`encode`](HostFault::encode)) uses stable tag discriminants
/// that a recorded reproducer's replay depends on, exactly like [`Fault`](crate::Fault).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum HostFault {
    /// Jitter virtual time by the given [`Span`] delta.
    SkewTime(Span),
    /// CPU modulation: reset the retired-branches → V-time slope to `Ratio`.
    SetClockRate(Ratio),
    /// Single-event-upset: XOR the word at guest-physical address `gpa` with
    /// `mask`.
    CorruptMemory {
        /// Guest-physical address of the word to corrupt.
        gpa: u64,
        /// The XOR bit-flip pattern.
        mask: BitMask,
    },
    /// Delivery-timing perturbation: inject interrupt `vector`.
    ///
    /// The vector is a `u32` because interrupt identities are per-arch: x86
    /// vectors fit 8 bits, but GIC INTIDs exceed them (`docs/ARCH-BOUNDARY.md`
    /// §C). The enforcing vendor rejects a vector outside its own range.
    InjectInterrupt {
        /// The interrupt vector to deliver.
        vector: u32,
    },
}

impl HostFault {
    /// Encode to the byte-deterministic form the control transport carries as an
    /// opaque blob for the `perturb` verb (the host-plane analogue of
    /// [`Answer::encode`](crate::Answer::encode)). Tag bytes are stable across
    /// [`CATALOG_VERSION`](crate::CATALOG_VERSION) bumps.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_host_fault(&mut w, self);
        w
    }

    /// Decode bytes produced by [`encode`](HostFault::encode). Strict and total:
    /// arbitrary or mutated bytes (an unknown tag, a zero `Ratio` denominator,
    /// truncation, trailing bytes) yield [`EnvError::Malformed`], never a panic.
    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let f = codec::read_host_fault(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(f)
    }
}

/// One override on the single [`Moment`] axis, from *either* control plane. This
/// is the load-bearing unification of the dissonance model: the reproducer's
/// override map is `BTreeMap<Moment, Action>`, so a host perturbation and a guest
/// decision sit on one ordered timeline and the search loop manipulates them
/// identically — adding a fault grows this vocabulary and the codec, never the
/// search.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// A host control-plane perturbation, applied imperatively by the frontier
    /// at its `Moment` (never through [`decide`](crate::Environment::decide)).
    Host(HostFault),
    /// A guest control-plane decision answer, resolved at the
    /// [`decide`](crate::Environment::decide) seam when its `Moment` surfaces
    /// (the task-24 [`Answer`]).
    Guest(Answer),
}

impl Action {
    /// Encode to a byte-deterministic, self-describing form (a one-byte plane tag
    /// then the plane's own encoding). Used by the reproducer blob; also handy
    /// for a transport that carries a whole `Action` rather than a bare
    /// [`HostFault`]/[`Answer`].
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_action(&mut w, self);
        w
    }

    /// Decode bytes produced by [`encode`](Action::encode). Strict and total;
    /// any defect is [`EnvError::Malformed`].
    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let a = codec::read_action(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(a)
    }

    /// The [`HostFault`] if this is a [`Host`](Action::Host) action, else `None`
    /// — the frontier uses this to pull the host-plane perturbations it enforces
    /// imperatively.
    pub fn host_fault(&self) -> Option<HostFault> {
        match self {
            Self::Host(f) => Some(*f),
            Self::Guest(_) => None,
        }
    }

    /// The guest [`Answer`] if this is a [`Guest`](Action::Guest) action, else
    /// `None` — [`materialize`](crate::EnvSpec::materialize) uses this to route
    /// guest overrides into the [`decide`](crate::Environment::decide) backing.
    pub fn guest_answer(&self) -> Option<&Answer> {
        match self {
            Self::Guest(a) => Some(a),
            Self::Host(_) => None,
        }
    }
}

impl From<HostFault> for Action {
    fn from(f: HostFault) -> Self {
        Self::Host(f)
    }
}

impl From<Answer> for Action {
    fn from(a: Answer) -> Self {
        Self::Guest(a)
    }
}
