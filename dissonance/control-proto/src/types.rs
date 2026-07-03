// SPDX-License-Identifier: AGPL-3.0-or-later
//! The control-plane value types: the opaque carried units the explorer ferries
//! schema-blind ([`Environment`], [`Answer`]), the handles and addressing
//! ([`SnapId`], [`VTime`], [`DecisionId`]), the request/reply verbs
//! ([`Request`], [`Reply`]), the run-control inputs ([`StopConditions`],
//! [`StopMask`], [`HashScope`]), and the guest-observable run outcomes
//! ([`StopReason`] and its payloads). All are plain data; the wire codec lives in
//! [`mod@crate::codec`].

/// One run's **environment** — entropy, scheduling, payload, and faults — carried
/// as an **opaque, versioned blob**. R2 is schema-blind: it never parses these
/// bytes (their structure is `environment::EnvSpec`'s contract). `blob_version`
/// lets the backend answer [`BadEnvVersion`](crate::ControlError::BadEnvVersion)
/// without the codec ever validating it (the codec carries any version through).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Environment {
    /// The `EnvSpec` blob-format version (validated by the backend, not the codec).
    pub blob_version: u16,
    /// The opaque serialized `EnvSpec`.
    pub bytes: Vec<u8>,
}

/// The opaque resolution of one [`Decision`](StopReason::Decision), carried
/// schema-blind. Its structure is `environment::Answer`'s contract; the backend
/// checks it for admissibility before staging, never the codec.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer(pub Vec<u8>);

/// An opaque host-plane perturbation, carried schema-blind — the host-plane
/// analogue of [`Answer`]. Its structure is `environment::HostFault`'s contract
/// (the bytes of `HostFault::encode`); the backend decodes and applies it, never
/// the codec. Staged by [`Perturb`](Request::Perturb).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostFault(pub Vec<u8>);

/// A moment on the single deterministic axis — a retired-instruction count.
/// Mirrors `environment::Moment` (conventions rule 2 — defined locally, not
/// imported); a host fault is staged at one via [`Perturb`](Request::Perturb).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Moment(pub u64);

/// A pool-wide snapshot handle returned by [`Snapshot`](Request::Snapshot).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SnapId(pub u64);

/// A moment in virtual time — a retired-branch count. Single-vCPU determinism
/// makes a bare V-time a unique moment.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct VTime(pub u64);

/// Identifies the one outstanding [`Decision`](StopReason::Decision). Single-vCPU
/// determinism guarantees at most one is ever outstanding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DecisionId(pub u64);

/// Decision-class discriminants, frozen to mirror `environment::DecisionClass`
/// (conventions rule 2 — defined locally, not imported). [`StopMask::arm`] takes
/// one of these as its `class_bit` and sets bit `1 << class_bit`; both crates
/// encode the identical bit so the armed-class set can never diverge. The numbers
/// are task 24's `DecisionClass` enum (`1..=6`) and never move.
pub mod class_bit {
    /// `DecisionClass::Entropy` — the guest pulled entropy.
    pub const ENTROPY: u16 = 1;
    /// `DecisionClass::Payload` — the guest pulled a fuzz payload.
    pub const PAYLOAD: u16 = 2;
    /// `DecisionClass::Scheduler` — a schedulable yield point.
    pub const SCHEDULER: u16 = 3;
    /// `DecisionClass::NetFlow` — a per-flow network decision (the host decides a
    /// flow policy the guest enforces in-guest; task 50 reshaped this from the
    /// per-frame `NetSend` and retired `pv-net`). The `NET_SEND` const name is
    /// retained for wire stability — the discriminant `4` (and thus the `StopMask`
    /// bit) is unchanged.
    pub const NET_SEND: u16 = 4;
    /// `DecisionClass::BlockIo` — a block read/write/flush.
    pub const BLOCK_IO: u16 = 5;
    /// `DecisionClass::Process` — a node lifecycle point.
    pub const PROCESS: u16 = 6;
}

/// A bitset over decision/exit **classes** that selects which non-terminal
/// decisions surface from a [`Run`](Request::Run) (vs. being auto-serviced by the
/// seed). Crash / assertion / quiescence always stop regardless of the mask.
///
/// Bit layout is the integrator-pinned mapping: `bit N == (1 << class_bit)` where
/// `class_bit` is the [`class_bit`] (i.e. `environment::DecisionClass`)
/// discriminant. The same bit is computed in both crates so the armed-class set
/// can never diverge.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopMask(pub u32);

impl StopMask {
    /// The empty mask — only the always-on terminal classes surface.
    pub const NONE: Self = StopMask(0);

    /// Arm the given class so its decisions surface. Sets bit `1 << class_bit`.
    /// A `class_bit ≥ 32` cannot be represented and is a no-op (panic-free); the
    /// real discriminants are `1..=6`.
    #[must_use]
    pub fn arm(self, class_bit: u16) -> Self {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => StopMask(self.0 | bit),
            None => self,
        }
    }

    /// Whether the given class is armed. `false` for any `class_bit ≥ 32`.
    pub fn armed(&self, class_bit: u16) -> bool {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

/// What a [`Run`](Request::Run) advances toward: an optional V-time `deadline`
/// and the class mask `on` selecting which decisions surface.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopConditions {
    /// Stop with [`Deadline`](StopReason::Deadline) at this V-time, if set.
    pub deadline: Option<VTime>,
    /// Which decision classes surface (vs. auto-service).
    pub on: StopMask,
}

/// The scope of a [`Hash`](Request::Hash) digest — the determinism primitive.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum HashScope {
    /// The whole VM state.
    Whole,
    /// The disk only.
    Disk,
    /// A `[base, base + len)` region of guest physical memory.
    Region {
        /// Region base address.
        base: u64,
        /// Region length in bytes.
        len: u64,
    },
}

/// An out-of-band control-plane request. [`Hello`](Self::Hello) must be the first
/// frame on a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request {
    /// Negotiate protocol/blob versions and coverage geometry. Must be first.
    Hello(Caps),
    /// Capture state at a quiescent point → [`SnapId`](Reply::SnapId).
    Snapshot,
    /// Release a snapshot (corpus GC) → [`Unit`](Reply::Unit).
    Drop(SnapId),
    /// Restore + reseed from `env` — the explore path → [`Unit`](Reply::Unit).
    Branch {
        /// The base snapshot to restore.
        snap: SnapId,
        /// The new environment to reseed with.
        env: Environment,
    },
    /// Restore verbatim — the reproduce / determinism-gate path →
    /// [`Unit`](Reply::Unit).
    Replay(SnapId),
    /// Advance the VM. `resolve` answers the immediately-prior
    /// [`Decision`](StopReason::Decision); a `resolve` with no outstanding
    /// decision is a loud [`ResolveWithoutDecision`](crate::ControlError::ResolveWithoutDecision),
    /// never silently dropped. Returns a [`Stop`](Reply::Stop).
    Run {
        /// When and on which classes to stop.
        until: StopConditions,
        /// The staged answer to the prior decision, if any.
        resolve: Option<Answer>,
    },
    /// Canonical state digest → [`Hash`](Reply::Hash).
    Hash {
        /// What to hash.
        scope: HashScope,
    },
    /// Stage a host-plane [`HostFault`] at `at`, recorded into the active
    /// environment → [`Unit`](Reply::Unit). The host plane rides this out-of-band
    /// channel (the guest never sees it); the backend decodes `fault` and applies
    /// it at its `Moment` during a `Run`. Mirrors the dissonance ruling's
    /// `perturb(fault, at)` verb.
    Perturb {
        /// The opaque host fault to stage (`environment::HostFault` bytes).
        fault: HostFault,
        /// The `Moment` (retired-instruction count) to apply it at.
        at: Moment,
    },
    /// Fetch a **page** of the link-tier SDK event capture of the current run
    /// (task 73), starting at event index `offset` → [`SdkEvents`](Reply::SdkEvents).
    /// The `Moment`-stamped `(moment, event_id, bytes)` stream a cooperating guest
    /// SDK emitted, so a remote client (the campaign's `SocketMachine`) can decode
    /// it into `RunTrace.events` — the server-side capture a socket client cannot
    /// otherwise see. The server bounds each page to the control frame limit, so a
    /// long capture is fetched by paging (`offset += page.len()`) until an empty
    /// page. Empty for a guest with no SDK, or once `offset` reaches the end.
    SdkEvents {
        /// The event index to start the page at.
        offset: u32,
    },
}

/// A successful reply to a [`Request`]. Pairs with [`ControlError`](crate::ControlError)
/// in the `Result<Reply, ControlError>` the codec carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
    /// The negotiated capabilities (reply to [`Hello`](Request::Hello)).
    Hello(Caps),
    /// A new snapshot handle (reply to [`Snapshot`](Request::Snapshot)).
    SnapId(SnapId),
    /// An acknowledgement with no value (reply to `Drop`/`Branch`/`Replay`).
    Unit,
    /// A guest-observable run outcome (reply to [`Run`](Request::Run)).
    Stop(StopReason),
    /// A 32-byte canonical digest (reply to [`Hash`](Request::Hash)).
    Hash([u8; 32]),
    /// The link-tier SDK event capture (reply to [`SdkEvents`](Request::SdkEvents)):
    /// the `Moment`-stamped `(moment, event_id, bytes)` stream, order-preserving.
    /// Empty for a guest with no SDK.
    SdkEvents(Vec<(u64, u32, Vec<u8>)>),
}

/// The guest-observable outcome of a [`Run`](Request::Run) — the explorer's
/// reaction surface. The first three are always present (the substrate); the last
/// three appear only with a cooperating guest / SDK.
///
/// There is deliberately no `Host` variant: an in-band hypercall is serviced by
/// the consonance plane and the run continues; anything R2 must react to arrives
/// as [`Decision`](Self::Decision) / [`SnapshotPoint`](Self::SnapshotPoint) /
/// [`Assertion`](Self::Assertion).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StopReason {
    /// The run reached its [`deadline`](StopConditions::deadline).
    Deadline {
        /// The V-time at which the run stopped.
        vtime: VTime,
    },
    /// HLT with an empty timer queue — the test ended.
    Quiescent {
        /// The V-time of quiescence.
        vtime: VTime,
    },
    /// The guest crashed.
    Crash {
        /// The V-time of the crash.
        vtime: VTime,
        /// What kind of crash, plus detail.
        info: CrashInfo,
    },
    /// A decision surfaced (its class was armed in the [`StopMask`]); answer it
    /// with the next [`Run`](Request::Run)'s `resolve`.
    Decision {
        /// The V-time of the decision.
        vtime: VTime,
        /// The outstanding decision's identity.
        id: DecisionId,
        /// Opaque service context for the explorer's policy.
        ctx: Vec<u8>,
    },
    /// An SDK lifecycle "ready" point.
    SnapshotPoint {
        /// The V-time of the snapshot point.
        vtime: VTime,
    },
    /// An SDK assertion fired (an `Always` violated or a `Sometimes` hit).
    Assertion {
        /// The V-time of the assertion.
        vtime: VTime,
        /// The event reference identifying the assertion.
        ev: EventRef,
    },
}

/// The kind of a guest [`Crash`](StopReason::Crash).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CrashKind {
    /// A guest kernel/userspace panic.
    Panic,
    /// A CPU triple fault.
    TripleFault,
    /// An orderly guest-requested shutdown that the test treats as a crash.
    Shutdown,
}

/// Detail accompanying a [`Crash`](StopReason::Crash): its [`CrashKind`] and an
/// opaque diagnostic blob.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CrashInfo {
    /// The crash classification.
    pub kind: CrashKind,
    /// Opaque diagnostic bytes (a message, register dump, etc.).
    pub detail: Vec<u8>,
}

/// A reference to an SDK event surfaced by an [`Assertion`](StopReason::Assertion).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EventRef {
    /// The event identifier (an SDK assertion id).
    pub id: u32,
    /// Opaque event payload.
    pub data: Vec<u8>,
}

/// Session capabilities, exchanged in [`Hello`](Request::Hello) and its reply.
/// Version mismatches are detectable from these fields alone; the coverage-map
/// bytes themselves never travel on the socket (only their geometry).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Caps {
    /// The negotiated application protocol version (distinct from the wire
    /// [`PROTO_VERSION`](crate::PROTO_VERSION) carried in the frame header).
    pub protocol_version: u16,
    /// Lowest `Environment` blob version this peer accepts.
    pub env_version_min: u16,
    /// Highest `Environment` blob version this peer accepts.
    pub env_version_max: u16,
    /// Where/shape of the coverage shmem map (its bytes are never serialized).
    pub coverage: CoverageGeometry,
    /// Capability flags (e.g. `guest_has_sdk`, coverage-producer kind).
    pub flags: CapFlags,
}

/// The shape of the coverage shmem map. Only geometry crosses the socket; the map
/// bytes live in shared memory the integrator maps out of band.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CoverageGeometry {
    /// The map size in bytes.
    pub map_bytes: u32,
    /// The coverage-producer kind (an opaque tag the integrator interprets).
    pub producer: u8,
}

/// A capability bitset carried in [`Caps::flags`]. The bit meanings are the
/// backend's contract; the codec only round-trips the `u32`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CapFlags(pub u32);

impl CapFlags {
    /// No flags set.
    pub const NONE: Self = CapFlags(0);
    /// The guest carries a cooperating SDK (decisions/assertions/snapshot points
    /// can surface).
    pub const GUEST_HAS_SDK: Self = CapFlags(1);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: CapFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with every bit in `other` also set.
    #[must_use]
    pub fn with(self, other: CapFlags) -> Self {
        CapFlags(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_mask_arm_sets_one_shifted_bit() {
        // The integrator-pinned mapping: armed bit == 1 << class_bit.
        for cb in [
            class_bit::ENTROPY,
            class_bit::PAYLOAD,
            class_bit::SCHEDULER,
            class_bit::NET_SEND,
            class_bit::BLOCK_IO,
            class_bit::PROCESS,
        ] {
            let m = StopMask::NONE.arm(cb);
            assert_eq!(m.0, 1u32 << cb);
            assert!(m.armed(cb));
            // No other class is armed.
            for other in 0u16..32 {
                if other != cb {
                    assert!(!m.armed(other));
                }
            }
        }
    }

    #[test]
    fn stop_mask_arm_is_idempotent_and_composes() {
        let m = StopMask::NONE
            .arm(class_bit::BLOCK_IO)
            .arm(class_bit::NET_SEND)
            .arm(class_bit::BLOCK_IO);
        assert!(m.armed(class_bit::BLOCK_IO));
        assert!(m.armed(class_bit::NET_SEND));
        assert!(!m.armed(class_bit::ENTROPY));
        assert_eq!(
            m.0,
            (1u32 << class_bit::BLOCK_IO) | (1u32 << class_bit::NET_SEND)
        );
    }

    #[test]
    fn stop_mask_out_of_range_class_is_a_total_noop() {
        // class_bit >= 32 cannot be represented; arm is a no-op and armed is
        // false — never a shift-overflow panic.
        for cb in [32u16, 33, 100, u16::MAX] {
            assert_eq!(StopMask::NONE.arm(cb), StopMask::NONE);
            assert!(!StopMask::NONE.arm(class_bit::BLOCK_IO).armed(cb));
        }
        assert!(!StopMask(u32::MAX).armed(32));
    }

    #[test]
    fn cap_flags_contains_and_with() {
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::GUEST_HAS_SDK));
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::NONE));
        assert!(!CapFlags::NONE.contains(CapFlags::GUEST_HAS_SDK));
        let both = CapFlags(0b10).with(CapFlags::GUEST_HAS_SDK);
        assert!(both.contains(CapFlags::GUEST_HAS_SDK));
        assert!(both.contains(CapFlags(0b10)));
        assert_eq!(both.0, 0b11);
        // Overlapping bits distinguish set-union (`|`) from XOR: `with` is
        // idempotent (re-adding a set bit keeps it; XOR would clear it).
        assert_eq!(
            CapFlags::GUEST_HAS_SDK.with(CapFlags::GUEST_HAS_SDK),
            CapFlags::GUEST_HAS_SDK,
            "with is a set-union, not XOR"
        );
        assert_eq!(CapFlags(0b11).with(CapFlags(0b01)), CapFlags(0b11));
    }
}
