// SPDX-License-Identifier: AGPL-3.0-or-later

//! Machine interface for deterministic search targets.
//!
//! The trait mirrors the consonance control protocol — snapshot, drop,
//! branch, replay, run, read — with local types and no dependency on
//! consonance crates, so one searcher drives the NES emulator today and a
//! control-protocol client later. The mirroring is structural, not partial:
//! the environment is an opaque versioned blob, `run` takes a class mask and
//! an answer to the prior decision, and [`StopReason`] carries the whole
//! outcome set including the crash and cooperating-guest stops. An
//! implementation that cannot produce a stop simply never returns it, so
//! searcher code that handles the set compiles and runs against both.
//!
//! Resume persistence uses snapshot export/import on the emulator
//! implementation only, outside the mirrored verb set, because rebuilding
//! every snapshot by re-running inputs would price a whole-tree resume in
//! re-emulation the export already paid for.

pub mod nes;

use std::fmt;

/// One run's recorded environment, carried as an opaque versioned blob.
///
/// The searcher never parses these bytes; their structure belongs to the
/// machine that mints and consumes them. `blob_version` lets an
/// implementation reject a blob from another format without inspecting it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reproducer {
    /// The blob format version, validated by the machine.
    pub blob_version: u16,
    /// The opaque serialized environment.
    pub bytes: Vec<u8>,
}

/// The opaque resolution of one [`StopReason::Decision`], carried
/// schema-blind exactly as [`Reproducer`] is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer(pub Vec<u8>);

/// Handle for one captured machine state, valid only on the instance that
/// issued it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapId(pub u64);

/// A point on the machine's deterministic clock. The NES machine counts
/// total frames emulated by the instance; snapshots do not carry it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Moment(pub u64);

/// Identifies the one outstanding [`StopReason::Decision`]. A single
/// deterministic execution axis means at most one is ever outstanding.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecisionId(pub u64);

/// Stop-class discriminants. [`StopMask::arm`] takes one and sets bit
/// `1 << class_bit`; the numbers are the control protocol's, so a mask built
/// here means the same thing on the wire.
pub mod class_bit {
    /// The guest pulled entropy.
    pub const ENTROPY: u16 = 1;
    /// The guest pulled a fuzz payload.
    pub const PAYLOAD: u16 = 2;
    /// A schedulable yield point.
    pub const SCHEDULER: u16 = 3;
    /// A per-flow network decision.
    pub const NET_FLOW: u16 = 4;
    /// A block read, write, or flush.
    pub const BLOCK_IO: u16 = 5;
    /// A node lifecycle point.
    pub const PROCESS: u16 = 6;
    /// A buggify decision. Per-point rather than per-class, so it is never
    /// armed to auto-service a whole class; the bit is reserved.
    pub const BUGGIFY: u16 = 7;
    /// The guest lifecycle snapshot point.
    pub const SNAPSHOT_POINT: u16 = 8;
    /// A guest assertion.
    pub const ASSERTION: u16 = 9;
}

/// A bitset over stop classes selecting which non-terminal stops surface
/// from a [`Machine::run`] rather than being serviced and run through.
///
/// The terminals — deadline, quiescence, crash — always stop regardless of
/// the mask, so [`StopMask::NONE`] runs a cooperating guest straight to its
/// terminal.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StopMask(pub u32);

impl StopMask {
    /// The empty mask: only the terminal classes surface.
    pub const NONE: Self = StopMask(0);

    /// Arm the given class so its stops surface. A `class_bit` past the
    /// bitset's width cannot be represented and is a no-op.
    #[must_use]
    pub fn arm(self, class_bit: u16) -> Self {
        match 1_u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => StopMask(self.0 | bit),
            None => self,
        }
    }

    /// Whether the given class is armed.
    #[must_use]
    pub fn armed(self, class_bit: u16) -> bool {
        match 1_u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

/// What a [`Machine::run`] advances toward.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StopConditions {
    /// Stop with [`StopReason::Deadline`] at this machine time, if set.
    pub deadline: Option<Moment>,
    /// Which stop classes surface rather than being serviced.
    pub on: StopMask,
}

/// The kind of a [`StopReason::Crash`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CrashKind {
    /// A guest panic.
    Panic,
    /// The guest entered a fault state it cannot return from.
    UnrecoverableFault,
    /// An orderly guest-requested shutdown the test treats as a crash.
    Shutdown,
}

/// Detail accompanying a [`StopReason::Crash`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashInfo {
    /// The crash classification.
    pub kind: CrashKind,
    /// Opaque diagnostic bytes.
    pub detail: Vec<u8>,
}

/// A reference to the guest event behind a [`StopReason::Assertion`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRef {
    /// The event identifier.
    pub id: u32,
    /// Opaque event payload.
    pub data: Vec<u8>,
}

/// Why a [`Machine::run`] stopped.
///
/// The terminals always surface. The cooperating-guest stops need a guest
/// that produces them and their class armed in the run's [`StopMask`]; a
/// machine with no such guest never returns them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The run reached its deadline.
    Deadline {
        /// The machine time at which the run stopped.
        vtime: Moment,
    },
    /// The staged environment is exhausted.
    Quiescent {
        /// The machine time of quiescence.
        vtime: Moment,
    },
    /// The guest crashed.
    Crash {
        /// The machine time of the crash.
        vtime: Moment,
        /// What kind of crash, plus detail.
        info: CrashInfo,
    },
    /// A decision surfaced because its class was armed; answer it with the
    /// next run's `resolve`.
    Decision {
        /// The machine time of the decision.
        vtime: Moment,
        /// The outstanding decision's identity.
        id: DecisionId,
        /// Opaque service context for the searcher's policy.
        ctx: Vec<u8>,
    },
    /// A guest lifecycle snapshot point.
    SnapshotPoint {
        /// The machine time of the snapshot point.
        vtime: Moment,
    },
    /// A guest assertion fired.
    Assertion {
        /// The machine time of the assertion.
        vtime: Moment,
        /// The event identifying the assertion.
        ev: EventRef,
    },
}

/// Failure to drive a machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineError {
    /// The snapshot handle names no held snapshot.
    UnknownSnapshot,
    /// A read fell outside the machine's addressable memory.
    ReadOutOfBounds,
    /// The environment blob names a format version this machine does not
    /// accept.
    BadEnvVersion,
    /// The environment blob is the right version but does not decode.
    MalformedEnv,
    /// A run staged an answer with no decision outstanding.
    ResolveWithoutDecision,
    /// The backing implementation failed.
    Backend(String),
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSnapshot => formatter.write_str("snapshot handle names no held snapshot"),
            Self::ReadOutOfBounds => formatter.write_str("read falls outside the machine's memory"),
            Self::BadEnvVersion => {
                formatter.write_str("environment blob version is not accepted by this machine")
            }
            Self::MalformedEnv => formatter.write_str("environment blob does not decode"),
            Self::ResolveWithoutDecision => {
                formatter.write_str("run staged an answer with no decision outstanding")
            }
            Self::Backend(detail) => write!(formatter, "machine backend failed: {detail}"),
        }
    }
}

impl std::error::Error for MachineError {}

/// The machine boundary required by deterministic search workloads.
pub trait Machine {
    /// Capture the current state behind a handle.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing implementation cannot capture state.
    fn snapshot(&mut self) -> Result<SnapId, MachineError>;

    /// Release a captured state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle.
    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError>;

    /// Restore a snapshot and stage a new environment — the explore path.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle, a failed restore, or an
    /// environment blob this machine does not accept.
    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError>;

    /// Restore a snapshot verbatim with nothing staged — the reproduce path.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle or a failed restore.
    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError>;

    /// Advance the machine through its staged environment. `resolve` answers
    /// the immediately prior [`StopReason::Decision`].
    ///
    /// # Errors
    ///
    /// Returns an error when execution fails, or when `resolve` is set with
    /// no decision outstanding.
    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError>;

    /// Read the machine's address space.
    ///
    /// # Errors
    ///
    /// Returns an error for a range outside the machine's memory.
    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError>;
}
