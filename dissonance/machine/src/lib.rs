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
//! implementation, outside the mirrored verb set, because rebuilding
//! every snapshot by re-running inputs would price a whole-tree resume in
//! re-emulation the export already paid for.

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod consonance;
pub mod nes;
#[cfg(unix)]
pub mod quicknes;

use std::{fmt, sync::Arc};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, ser::SerializeSeq,
};

const SHARED_STATE_CHUNK_SIZE: usize = 512;

#[derive(Debug, Eq, PartialEq)]
struct SharedStateInner {
    chunks: Vec<Arc<[u8; SHARED_STATE_CHUNK_SIZE]>>,
    len: usize,
}

/// A portable byte state whose full 512-byte chunks can be shared with a base.
///
/// `SharedState` has the same Serde representation as `Vec<u8>`: it is
/// serialized as a sequence of bytes, with no chunk or sharing metadata on the
/// wire. An exported state records the full allocation size of chunks it newly
/// owns relative to its optional export base; a deserialized state has no base
/// and therefore charges every allocated chunk. The charge is explicit and
/// deterministic, and never depends on the process-global reference count.
#[derive(Clone)]
pub struct SharedState {
    inner: Arc<SharedStateInner>,
    newly_owned_bytes: usize,
}

impl fmt::Debug for SharedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedState")
            .field("len", &self.inner.len)
            .field("chunks", &self.inner.chunks.len())
            .field("newly_owned_bytes", &self.newly_owned_bytes)
            .finish()
    }
}

impl PartialEq for SharedState {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl Eq for SharedState {}

impl SharedState {
    pub(crate) fn from_bytes(bytes: Vec<u8>, base: Option<&Self>) -> Self {
        let mut chunks = Vec::with_capacity(bytes.len().div_ceil(SHARED_STATE_CHUNK_SIZE));
        let mut newly_owned_chunks = 0_usize;
        for (index, source) in bytes.chunks(SHARED_STATE_CHUNK_SIZE).enumerate() {
            let mut chunk = [0_u8; SHARED_STATE_CHUNK_SIZE];
            chunk[..source.len()].copy_from_slice(source);
            let shared = base
                .and_then(|state| state.inner.chunks.get(index))
                .filter(|existing| existing.as_ref() == &chunk)
                .cloned();
            let chunk = match shared {
                Some(existing) => existing,
                None => {
                    newly_owned_chunks = newly_owned_chunks.saturating_add(1);
                    Arc::new(chunk)
                }
            };
            chunks.push(chunk);
        }
        Self {
            inner: Arc::new(SharedStateInner {
                chunks,
                len: bytes.len(),
            }),
            newly_owned_bytes: newly_owned_chunks.saturating_mul(SHARED_STATE_CHUNK_SIZE),
        }
    }

    pub(crate) fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.inner.len);
        for chunk in &self.inner.chunks {
            let remaining = self.inner.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(SHARED_STATE_CHUNK_SIZE)]);
        }
        bytes
    }

    pub(crate) fn memory_charge(&self) -> usize {
        self.newly_owned_bytes
    }
}

impl Serialize for SharedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.inner.len))?;
        for index in 0..self.inner.len {
            let chunk = self
                .inner
                .chunks
                .get(index / SHARED_STATE_CHUNK_SIZE)
                .ok_or_else(|| <S::Error as serde::ser::Error>::custom("invalid shared state"))?;
            let byte = chunk
                .get(index % SHARED_STATE_CHUNK_SIZE)
                .ok_or_else(|| <S::Error as serde::ser::Error>::custom("invalid shared state"))?;
            sequence.serialize_element(byte)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for SharedState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(bytes, None))
    }
}

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
    /// Archive representation of a captured machine state.
    type Portable: Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned;

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

    /// Export a held snapshot into the archive representation.
    ///
    /// When `base` is supplied, an implementation may share identical
    /// content with it. The resulting portable value owns the newly allocated
    /// portion and reports that portion through
    /// [`Machine::portable_memory_charge`].
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown snapshot or a failed export.
    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError>;

    /// Import an archive representation behind a fresh snapshot handle.
    ///
    /// Import does not make the state current; use [`Machine::replay`] or
    /// [`Machine::branch`] with the returned handle to restore it.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing implementation cannot hold the
    /// portable state.
    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError>;

    /// Return the deterministic bytes charged for this portable state.
    ///
    /// For chunk-shared states this is the full allocation size of chunks
    /// newly owned by the value at export time. A value reconstructed by
    /// deserialization has no base and charges all of its allocated chunks.
    #[must_use]
    fn portable_memory_charge(portable: &Self::Portable) -> usize;

    /// Return the lifetime machine time. Restoring a snapshot does not rewind
    /// this clock.
    #[must_use]
    fn now(&self) -> Moment;

    /// Work RAM captured after each frame of the most recent [`Machine::run`].
    #[must_use]
    fn frames(&self) -> &[[u8; 2048]];
}

#[cfg(test)]
mod tests {
    use super::SharedState;

    #[test]
    fn shared_state_charges_new_full_chunks_not_reference_counts() {
        let base = SharedState::from_bytes(vec![7_u8; 513], None);
        assert_eq!(base.memory_charge(), 2 * 512);

        let same = SharedState::from_bytes(vec![7_u8; 513], Some(&base));
        assert_eq!(same.memory_charge(), 0);

        let mut changed_bytes = vec![7_u8; 513];
        changed_bytes[512] = 8;
        let changed = SharedState::from_bytes(changed_bytes.clone(), Some(&base));
        assert_eq!(changed.memory_charge(), 512);
        assert_eq!(changed.materialize(), changed_bytes);
    }
}
