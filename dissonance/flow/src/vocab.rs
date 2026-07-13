// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shared flow-fault vocabulary every [`FlowEngine`](crate::FlowEngine)
//! implementation speaks: the value types ([`Moment`], [`Span`], [`ConnId`],
//! [`NodeId`], [`Dir`]), the connection-stream event ([`FlowEvent`]), the
//! concrete proxy action ([`FlowAction`]), and the per-flow policy
//! ([`FlowPolicy`]) the decider returns. These are defined locally (conventions
//! rule 2); the frontier maps `environment`'s `Answer`/`NetFlow` vocabulary
//! onto them.

/// A **point** on the V-time axis — a count of retired conditional branches,
/// the *only* deterministic clock in the system. Every scheduled time and
/// event stamp is one of these; there is no wall-clock anywhere in the engine.
/// Mirrors the integration type (conventions rule 2). (The GLOSSARY rename of
/// this crate's former `VTime` newtype — points are `Moment`s, durations are
/// [`Span`]s; "V-time" survives as the clock mechanism's name.)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Moment(pub u64);

/// A **duration** on the V-time axis, in the same retired-branch units as
/// [`Moment`] — the [`Latency`](FlowPolicy::Latency) delay is one of these.
/// Mirrors the integration type (conventions rule 2).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Span(pub u64);

/// A connection identity (derived by the frontier from a flow's 5-tuple). The
/// engine treats it as an opaque key for per-flow state; it never reaches a hash
/// where order could leak (conventions rule 4). Mirrors the integration type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ConnId(pub u64);

/// An in-guest node (a container/process) — the `src`/`dst` of a flow, handed to
/// the decider so a policy can depend on the link. Mirrors the integration type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodeId(pub u32);

/// The direction a chunk travels on a (full-duplex) connection. Each direction
/// is scheduled independently so the two halves of a connection never interfere
/// (e.g. a [`Throttle`](FlowPolicy::Throttle) paces each direction on its own
/// cursor).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Dir {
    /// Bytes flowing from the connection's client toward its server.
    ClientToServer,
    /// Bytes flowing from the connection's server back toward its client.
    ServerToClient,
}

impl Dir {
    /// A small dense index (`0`/`1`) for keying per-direction state. Internal —
    /// the mapping is an implementation detail, not part of the contract.
    pub(crate) fn idx(self) -> usize {
        match self {
            Dir::ClientToServer => 0,
            Dir::ServerToClient => 1,
        }
    }
}

/// One event on the proxied connection stream. In tests these are hand-scripted;
/// in the frontier shell they are produced from real `accept`/`read` on the one
/// central proxy. The `bytes` are guest-controlled, so an engine's handling must
/// never panic on them (conventions rule 4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FlowEvent {
    /// A new flow appeared. The engine consults the decider here (the toxiproxy
    /// engine: exactly once per flow) and records the flow's policy. Carries no
    /// V-time: a flow's identity precedes its first byte.
    Open {
        /// The new connection's identity.
        conn: ConnId,
        /// The node the connection originates from.
        src: NodeId,
        /// The node the connection is addressed to.
        dst: NodeId,
    },
    /// A chunk of bytes arrived on an open flow at V-time `at`. The engine
    /// applies the flow's policy and schedules any resulting delivery.
    Chunk {
        /// Which flow the chunk belongs to.
        conn: ConnId,
        /// Which half-duplex direction the chunk travels.
        dir: Dir,
        /// The V-time the chunk was read off the wire.
        at: Moment,
        /// The chunk's bytes (guest-controlled, arbitrary length).
        bytes: Vec<u8>,
    },
    /// A flow closed at V-time `at`. The engine schedules the connection's
    /// teardown (a [`FlowAction::Reset`]) after any still-pending delivery.
    Close {
        /// Which flow closed.
        conn: ConnId,
        /// The V-time the close was observed.
        at: Moment,
    },
}

/// What the proxy must physically do, drained by V-time through
/// [`FlowEngine::due`]. The frontier shell enacts each one on real sockets; in
/// tests they are compared against a hand-written golden schedule.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FlowAction {
    /// Write `bytes` for `conn` in direction `dir` at V-time `at`.
    Deliver {
        /// The flow to deliver on.
        conn: ConnId,
        /// The direction the bytes travel.
        dir: Dir,
        /// The bytes to write.
        bytes: Vec<u8>,
        /// The V-time the delivery is due.
        at: Moment,
    },
    /// Tear `conn` down (send a `RST` / drop the proxied sockets) at V-time `at`.
    Reset {
        /// The flow to reset.
        conn: ConnId,
        /// The V-time the reset is due.
        at: Moment,
    },
}

impl FlowAction {
    /// The V-time this action is due — the primary key
    /// [`due`](crate::FlowEngine::due) drains on. Every action drained by
    /// `due(now)` satisfies `action.at() <= now`.
    pub fn at(&self) -> Moment {
        match self {
            FlowAction::Deliver { at, .. } | FlowAction::Reset { at, .. } => *at,
        }
    }
}

/// The per-flow policy an engine applies — the decider's answer for one flow.
/// Mirrors task 50's per-flow `NetFlow` fault vocabulary (`environment::Fault`'s
/// `Net*` variants); defined locally (conventions rule 2). `Nominal` is the
/// faults-off answer (deliver normally).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FlowPolicy {
    /// Deliver every chunk verbatim at the V-time it arrived.
    Nominal,
    /// Delay each chunk's delivery by `d` V-time (Linux `netem`). The delay
    /// saturates: a hostile `Latency(u64::MAX)` clamps the delivery time to
    /// `u64::MAX`, never wraps into the past.
    Latency(Span),
    /// Drop each chunk with probability `num/den`, sampled from a connection-local
    /// PRNG seeded by `seed` (never the ambient stream, so replay is exact). A
    /// `num >= den` ratio drops everything (`1/1` is a full drop); `den == 0` is
    /// treated as no loss (deliver), so a malformed fraction never divides by zero.
    Loss {
        /// Seed for the per-connection drop PRNG (taken from the recorded decision).
        seed: u64,
        /// Numerator of the drop fraction.
        num: u16,
        /// Denominator of the drop fraction (`0` ⇒ no loss).
        den: u16,
    },
    /// Pace bytes at `bps` in V-time (Linux `tbf`): delivering `n` bytes occupies
    /// `ceil(n / bps)` V-time units per direction, so the delivery time is the
    /// later of the chunk's arrival and the direction's transmit cursor, plus that
    /// cost. `bps` is bytes per V-time unit (the V-time tick plays the role of the
    /// "second"); `bps == 0` clamps every delivery to `u64::MAX` rather than
    /// dividing by zero.
    Throttle {
        /// Bandwidth cap in bytes per V-time unit.
        bps: u32,
    },
    /// Tear the connection down: schedule a single [`FlowAction::Reset`] at the
    /// first event carrying a V-time and drop everything else on the flow.
    Reset,
}
