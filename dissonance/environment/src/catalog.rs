// SPDX-License-Identifier: AGPL-3.0-or-later
//! The versioned **guest** catalog — the shared vocabulary every service and the
//! explorer agree on: decision [classes](DecisionClass), the concrete
//! [points](DecisionPoint) a service surfaces, the [answers](Answer) the platform
//! returns, and the per-class [faults](Fault). Every class here is a **guest**
//! control-plane class: it exists only because the guest *requested* a service
//! (conventions: the litmus is "does the guest have to *ask* for this?"). The
//! workload-agnostic host plane ([`HostFault`](crate::HostFault)) is **not** in
//! this catalog — it has no service point and no [`decide`](crate::Environment::decide)
//! entry; see [`HostFault`](crate::HostFault) and `tasks/45-host-control-plane.md`.
//!
//! **Guest, namespaced, layerable (D7).** Per the dissonance ruling
//! (`docs/DISSONANCE.md`, "The guest control planes"), guest decision classes are
//! *namespaced per `harmony-<env>` layer* (`linux.net.drop`, `kube.net.partition`,
//! …) and they **layer**: a higher guest environment may *add* or *constrain* a
//! lower layer's classes but never silently reinterpret them. The proper division
//! of labour is that `environment` owns the **seam**
//! ([`Environment::decide`](crate::Environment::decide)) and the **codec** (the
//! byte-exact, version-stable [`Answer`]/[`Fault`] forms), while a *concrete*
//! catalog is **contributed by a guest environment**, not hardcoded in the engine.
//! The flat enumeration below is therefore the crate's **built-in reference
//! catalog** — the convergent FoundationDB/Antithesis vocabulary — standing in
//! until the per-layer, namespaced catalogs the `harmony-<env>` crates supply.

use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::{ConnId, NodeId, VTime};

/// The class of a **guest** decision: which guest-requested service surfaced it
/// and, therefore, which answers are admissible. `#[repr(u16)]` with stable
/// discriminants — a recorded [`EnvSpec`](crate::EnvSpec) replays across a
/// [`CATALOG_VERSION`](crate::CATALOG_VERSION) bump only because these numbers
/// never move. These are guest-plane classes only; the host plane
/// ([`HostFault`](crate::HostFault)) has no class here (see the module note on
/// the guest/namespaced/layerable framing).
///
/// The first three are **supply** classes (the environment supplies a value, and
/// they never fault); the last three are **fault** classes (the service proceeds
/// nominally or is perturbed, and they never supply).
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum DecisionClass {
    /// The guest pulled entropy.
    Entropy = 1,
    /// The guest pulled a fuzz payload.
    Payload = 2,
    /// A schedulable yield point between in-guest nodes.
    Scheduler = 3,
    /// A network **flow** the guest utility asks the host about — once per
    /// flow/connection, not per frame. The host *decides* a flow policy
    /// ([`Fault::NetLatency`]/[`NetLoss`](Fault::NetLoss)/[`NetThrottle`](Fault::NetThrottle)/[`NetReset`](Fault::NetReset)
    /// or [`Answer::Nominal`]); the guest *enforces* it on the intra-guest CNI.
    /// Discriminant **4** is preserved across the task-50 rename from `NetSend`
    /// (per-frame) so `control-proto`'s `StopMask` bit is unchanged.
    NetFlow = 4,
    /// A block read/write/flush.
    BlockIo = 5,
    /// A node lifecycle point (pause/kill/restart).
    Process = 6,
    /// A named SDK **buggify** site (task 73): the guest asks the host whether
    /// to fire a deliberate perturbation at this catalog-registered point. A
    /// **fault** class — answered [`Answer::Nominal`] (don't fire) or
    /// [`Answer::Fault`]`(`[`Fault::BuggifyFire`]`)` from the domain-separated
    /// *fault* PRNG stream, per-point biased by the [`FaultPolicy`](crate::FaultPolicy)
    /// (the guest never sees probabilities). Unlike the other fault classes it
    /// carries no service-specific bound: a buggify point is a bare, named,
    /// steerable coin flip (the improvement over FoundationDB's anonymous
    /// `get_random`). Discriminant **7** is stable across a
    /// [`CATALOG_VERSION`](crate::CATALOG_VERSION) bump.
    Buggify = 7,
}

impl DecisionClass {
    /// Whether this is a supply class ([`Entropy`](Self::Entropy),
    /// [`Payload`](Self::Payload), [`Scheduler`](Self::Scheduler)): the
    /// environment supplies a value and the class never faults.
    pub fn is_supply(self) -> bool {
        matches!(self, Self::Entropy | Self::Payload | Self::Scheduler)
    }

    /// Whether this is a fault class ([`NetFlow`](Self::NetFlow),
    /// [`BlockIo`](Self::BlockIo), [`Process`](Self::Process),
    /// [`Buggify`](Self::Buggify)): the service proceeds nominally or is
    /// perturbed, and the class never supplies.
    pub fn is_fault(self) -> bool {
        !self.is_supply()
    }

    /// The wire discriminant.
    pub(crate) fn as_u16(self) -> u16 {
        self as u16
    }

    /// Decode a discriminant, rejecting unknown values.
    pub(crate) fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Entropy),
            2 => Some(Self::Payload),
            3 => Some(Self::Scheduler),
            4 => Some(Self::NetFlow),
            5 => Some(Self::BlockIo),
            6 => Some(Self::Process),
            7 => Some(Self::Buggify),
            _ => None,
        }
    }
}

/// A block-I/O operation, the `op` of a [`DecisionPoint::BlockIo`]. It is part
/// of the live decision a service reads, never of a serialized blob, so it needs
/// no wire codec.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum BlockOp {
    /// A sector read.
    Read = 0,
    /// A sector write.
    Write = 1,
    /// A cache flush / barrier.
    Flush = 2,
}

/// What surfaced a [`DecisionPoint::NetFlow`] — the `event` a guest utility
/// reports when it asks the host about a flow. The utility asks **once per
/// flow/connection** (not per frame), so today the only event is the flow
/// [`Open`](Self::Open).
///
/// `#[repr(u16)]` and deliberately **extensible**: a later layer may add events
/// (e.g. a flow close or a half-open transition) without moving
/// [`Open`](Self::Open)'s discriminant. It is part of the *live* decision a
/// service reads, never of a serialized blob (the same as [`BlockOp`]), so it
/// needs no wire codec.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FlowEvent {
    /// The utility first saw this flow and is asking the host for its policy.
    Open = 0,
}

/// A concrete decision the platform must answer, carrying its class plus the
/// service-specific context a policy reads to choose an answer. Those context
/// fields never reach a hash or an encoded byte in any order-dependent way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DecisionPoint {
    /// The guest pulled `bytes` of entropy (`bytes ≤ MAX_SUPPLY_LEN`, clamped by
    /// the service).
    Entropy {
        /// Requested entropy length in bytes.
        bytes: u32,
    },
    /// The guest pulled `bytes` of fuzz payload (`bytes ≤ MAX_SUPPLY_LEN`).
    Payload {
        /// Requested payload length in bytes.
        bytes: u32,
    },
    /// A scheduler yield point with `ready` runnable nodes; the answer selects
    /// one runnable index in `0..ready`.
    Scheduler {
        /// Count of runnable nodes.
        ready: u32,
    },
    /// A network **flow** from `src` to `dst` on connection `conn`, surfaced by
    /// the guest utility on `event` — **once per flow**, not per frame. The host
    /// answers a flow-level policy ([`Answer::Nominal`] or a net
    /// [`Answer::Fault`]) the guest then enforces on the intra-guest CNI. This is
    /// the `net_decide` request shape (task 50); the response is an [`Answer`]
    /// for the [`NetFlow`](DecisionClass::NetFlow) class, carried by the existing
    /// [`Answer::encode`]/[`decode`](Answer::decode) — no new codec.
    NetFlow {
        /// Source node.
        src: NodeId,
        /// Destination node.
        dst: NodeId,
        /// Connection identity (for fault targeting).
        conn: ConnId,
        /// What surfaced this flow decision (today, always [`FlowEvent::Open`]).
        event: FlowEvent,
    },
    /// A block I/O of `op` at `lba` for `len` bytes.
    BlockIo {
        /// The operation.
        op: BlockOp,
        /// Logical block address.
        lba: u64,
        /// I/O length in bytes.
        len: u32,
    },
    /// A lifecycle point for `node`.
    Process {
        /// The node whose lifecycle is in question.
        node: NodeId,
    },
    /// A named SDK **buggify** site (task 73). The guest asks whether to fire a
    /// deliberate perturbation at `point`; the host answers [`Answer::Nominal`]
    /// (don't fire) or [`Answer::Fault`]`(`[`Fault::BuggifyFire`]`)` from the
    /// fault stream, per-point biased by the [`FaultPolicy`](crate::FaultPolicy).
    /// The `point` is a catalog-registered site id — identity the guest owns,
    /// steering the host owns.
    Buggify {
        /// The catalog-registered buggify site id.
        point: u32,
    },
}

impl DecisionPoint {
    /// The class of this decision.
    pub fn class(&self) -> DecisionClass {
        match self {
            Self::Entropy { .. } => DecisionClass::Entropy,
            Self::Payload { .. } => DecisionClass::Payload,
            Self::Scheduler { .. } => DecisionClass::Scheduler,
            Self::NetFlow { .. } => DecisionClass::NetFlow,
            Self::BlockIo { .. } => DecisionClass::BlockIo,
            Self::Process { .. } => DecisionClass::Process,
            Self::Buggify { .. } => DecisionClass::Buggify,
        }
    }

    /// Whether `ans` is an **admissible** answer for this decision — the right
    /// class **and** within the point's bounds. This is the single source of
    /// truth for admissibility: [`RecordedEnv`](crate::RecordedEnv) consults it
    /// to decide whether an override wins, and the (frontier) reactive backend
    /// applies the same check to a decoded resolve answer before staging it, so
    /// the two can never drift.
    ///
    /// - Supply classes ([`Entropy`](Self::Entropy)/[`Payload`](Self::Payload)/[`Scheduler`](Self::Scheduler)):
    ///   only a [`Answer::Supply`] of the exact requested length — for
    ///   [`Scheduler`](Self::Scheduler), exactly 4 bytes decoding to a selection
    ///   `< ready`.
    /// - Fault classes ([`NetFlow`](Self::NetFlow)/[`BlockIo`](Self::BlockIo)/[`Process`](Self::Process)/[`Buggify`](Self::Buggify)):
    ///   [`Answer::Nominal`], or a [`Answer::Fault`] of the same class within
    ///   bounds (a [`Fault::BlockTorn`] no longer than the request;
    ///   [`Fault::BuggifyFire`] is bound-free).
    ///
    /// Total and panic-free on any pairing.
    pub fn admits(&self, ans: &Answer) -> bool {
        match (self, ans) {
            (Self::Entropy { bytes }, Answer::Supply(v))
            | (Self::Payload { bytes }, Answer::Supply(v)) => v.len() as u64 == *bytes as u64,
            (Self::Scheduler { ready }, Answer::Supply(v)) => {
                v.len() == 4 && u32::from_le_bytes([v[0], v[1], v[2], v[3]]) < *ready
            }
            (
                Self::NetFlow { .. }
                | Self::BlockIo { .. }
                | Self::Process { .. }
                | Self::Buggify { .. },
                Answer::Nominal,
            ) => true,
            (
                Self::NetFlow { .. }
                | Self::BlockIo { .. }
                | Self::Process { .. }
                | Self::Buggify { .. },
                Answer::Fault(f),
            ) => f.class() == self.class() && self.fault_bounds_ok(f),
            // Every remaining pairing is a class mismatch (a supply class with a
            // non-Supply, a fault class with a Supply).
            _ => false,
        }
    }

    /// Whether a same-class fault's parameters fit this point's bounds. Only
    /// [`Fault::BlockTorn`] has a point-relative bound (`n ≤ len`); every other
    /// fault's parameters are bound-free (delays are any V-time; the flow-level
    /// net policies carry no point-relative bound — a [`NetFlow`](Self::NetFlow)
    /// has no frame length to bound against).
    fn fault_bounds_ok(&self, fault: &Fault) -> bool {
        match (self, fault) {
            (Self::BlockIo { len, .. }, Fault::BlockTorn(n)) => *n as u64 <= *len as u64,
            _ => true,
        }
    }
}

/// The fault catalog, grouped by the class it applies to. The vocabulary is
/// convergent across FoundationDB / Antithesis; delays are in [`VTime`]
/// branch-count units. The byte form (see [`Answer::encode`]) uses stable
/// discriminants that a recorded [`EnvSpec`](crate::EnvSpec) replay depends on.
///
/// **Network faults are per-flow policies (task 50).** The host *decides* a
/// flow-level policy ([`NetLatency`](Self::NetLatency) / [`NetLoss`](Self::NetLoss)
/// / [`NetThrottle`](Self::NetThrottle) / [`NetReset`](Self::NetReset)), recorded
/// into the [`Moment`](crate::Moment)-keyed reproducer; the guest *enforces* it on
/// the intra-guest CNI with Linux's own mechanisms (netem / tbf / a reset),
/// taking every input from a determinized source (delays in guest V-time, losses
/// from a seeded PRNG). The host is in the **control** path, never the **data**
/// path. The retired per-*frame* faults (`NetDrop`/`NetDelay`/`NetReorder`/
/// `NetDup`/`NetCorrupt`, with `CorruptSpec`) needed a host-side switch on the
/// frame stream, which task 50 removed with `dissonance/pv-net`.
///
/// **Per-message faults moved up, not away.** Reordering, duplicating, or
/// corrupting a *specific* message needs message boundaries the network layer
/// cannot see; together with L2 byte-corruption they belong to the **SDK / L7
/// tier** (a later task), not this flow-level catalog. They are deferred, not
/// dropped.
///
/// There is deliberately no `Partition` variant: a network partition is not a
/// per-flow fault but a standing, correlated topology policy (a link and a V-time
/// window where *all* traffic drops together), carried as a
/// [`StandingFault`](crate::StandingFault) and enforced guest-side by the utility
/// (e.g. an nftables rule for the window). A single-flow "partition" would just be
/// [`Fault::NetLoss`] at `1/1` (full drop) or [`Fault::NetReset`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Fault {
    /// Add `d` of guest-time delay to the flow (Linux `netem`); `d` is in
    /// [`VTime`] units so the delay is measured in determinized guest time.
    NetLatency(VTime),
    /// Drop a `num/den` fraction of the flow's packets, sampled from a seeded
    /// PRNG (never a host RNG). `1/1` is a full drop; `den` is the denominator of
    /// the fraction and the seeded enforcer treats `den == 0` as a deterministic
    /// no-op (deliver), so an out-of-range parameter never divides by zero —
    /// exactly as the retired per-frame `NetCorrupt` reduced its offset modulo the
    /// length at enforcement.
    NetLoss {
        /// Numerator of the drop fraction.
        num: u16,
        /// Denominator of the drop fraction (`den == 0` ⇒ no-op at enforcement).
        den: u16,
    },
    /// Cap the flow's bandwidth to `bps` (Linux `tbf`).
    NetThrottle {
        /// Bandwidth cap (`bps`); the guest enforcer maps it to a `tbf` rate.
        bps: u32,
    },
    /// Refuse / reset the connection (a `RST`).
    NetReset,
    /// Fail a block I/O with `EIO`.
    BlockEio,
    /// Complete a block I/O after the given V-time latency.
    BlockLatency(VTime),
    /// Tear a block write/read at `n` bytes (the rest is not transferred).
    BlockTorn(u32),
    /// Fail a block write with `ENOSPC`.
    BlockNospc,
    /// Pause a node for the given V-time.
    ProcPause(VTime),
    /// Kill a node.
    ProcKill,
    /// Restart a node.
    ProcRestart,
    /// Fire a named SDK **buggify** site (task 73) — the guest-plane
    /// perturbation a [`DecisionClass::Buggify`] point resolves to when the host
    /// decides to fire. Parameterless: the site identity lives in the
    /// [`DecisionPoint::Buggify`]'s `point`, not in the fault. The byte tag
    /// (`16`) is disjoint from every earlier tag so a stale blob can never
    /// reinterpret into it.
    BuggifyFire,
}

impl Fault {
    /// The class this fault belongs to (its only admissible
    /// [`DecisionClass`]).
    pub fn class(&self) -> DecisionClass {
        match self {
            Self::NetLatency(_)
            | Self::NetLoss { .. }
            | Self::NetThrottle { .. }
            | Self::NetReset => DecisionClass::NetFlow,
            Self::BlockEio | Self::BlockLatency(_) | Self::BlockTorn(_) | Self::BlockNospc => {
                DecisionClass::BlockIo
            }
            Self::ProcPause(_) | Self::ProcKill | Self::ProcRestart => DecisionClass::Process,
            Self::BuggifyFire => DecisionClass::Buggify,
        }
    }
}

/// The **guest** control-plane answer the platform returns at a
/// [`DecisionPoint`] — the value a guest-requested service receives, nominally or
/// not. A host-plane perturbation is **not** an `Answer` (it has no decision
/// point); it is a [`HostFault`](crate::HostFault), the other arm of
/// [`Action`](crate::Action) in the [`Moment`](crate::Moment)-keyed reproducer.
///
/// - **Supply classes** ([`Entropy`](DecisionClass::Entropy) /
///   [`Payload`](DecisionClass::Payload) / [`Scheduler`](DecisionClass::Scheduler)):
///   a non-fault answer is [`Supply`](Answer::Supply) — the entropy/payload
///   bytes, or for `Scheduler` the chosen runnable index as a little-endian
///   `u32`. These classes never [`Fault`](Answer::Fault).
/// - **Fault classes** ([`NetFlow`](DecisionClass::NetFlow) /
///   [`BlockIo`](DecisionClass::BlockIo) / [`Process`](DecisionClass::Process)):
///   the service proceeds ([`Nominal`](Answer::Nominal)) or is perturbed
///   ([`Fault`](Answer::Fault)). These classes never [`Supply`](Answer::Supply).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer {
    /// The service proceeds without perturbation.
    Nominal,
    /// The environment supplies these bytes (entropy/payload, or a 4-byte
    /// little-endian scheduler selection).
    Supply(Vec<u8>),
    /// The service is perturbed by this fault.
    Fault(Fault),
}

impl Answer {
    /// Encode to the byte-deterministic form the control plane carries as an
    /// opaque `Answer(Vec<u8>)`. Governed by
    /// [`CATALOG_VERSION`](crate::CATALOG_VERSION); the tag bytes are stable
    /// across versions.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_answer(&mut w, self);
        w
    }

    /// Decode bytes produced by [`encode`](Answer::encode). Never panics on bad
    /// bytes; rejects them with [`EnvError`]. Structural only — whether the
    /// decoded answer is *admissible for a particular outstanding decision* is a
    /// separate check (see [`RecordedEnv`](crate::RecordedEnv)).
    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let a = codec::read_answer(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(a)
    }
}
