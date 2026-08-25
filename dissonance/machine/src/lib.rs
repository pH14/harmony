// SPDX-License-Identifier: AGPL-3.0-or-later

//! Machine interface for deterministic search targets.
//!
//! The trait mirrors the verb set of the consonance control protocol —
//! snapshot, drop, branch, replay, run, read — with local types and no
//! dependency on consonance crates, so a searcher written against it runs
//! unchanged on the NES emulator today and on a control-protocol client
//! later. Resume persistence uses snapshot export/import on the emulator
//! implementation only, outside the mirrored verb set, because rebuilding
//! every snapshot by re-running inputs would price a whole-tree resume in
//! re-emulation the export already paid for.

pub mod nes;

use std::fmt;

/// Handle for one captured machine state, valid only on the instance that
/// issued it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapId(pub u64);

/// A point on the machine's deterministic clock. The NES machine counts
/// total frames emulated by the instance; snapshots do not carry it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Moment(pub u64);

/// What a [`Machine::run`] advances toward.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StopConditions {
    /// Stop with [`StopReason::Deadline`] at this machine time, if set.
    pub deadline: Option<Moment>,
}

/// Why a [`Machine::run`] stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
}

/// Failure to drive a machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineError {
    /// The snapshot handle names no held snapshot.
    UnknownSnapshot,
    /// A read fell outside the machine's addressable memory.
    ReadOutOfBounds,
    /// The backing implementation failed.
    Backend(String),
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSnapshot => formatter.write_str("snapshot handle names no held snapshot"),
            Self::ReadOutOfBounds => formatter.write_str("read falls outside the machine's memory"),
            Self::Backend(detail) => write!(formatter, "machine backend failed: {detail}"),
        }
    }
}

impl std::error::Error for MachineError {}

/// The machine boundary required by deterministic search workloads.
pub trait Machine {
    /// Recorded inputs a branch replays; for the NES machine, a controller
    /// action suffix.
    type Env;

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
    /// Returns an error for an unknown handle or a failed restore.
    fn branch(&mut self, snap: SnapId, env: Self::Env) -> Result<(), MachineError>;

    /// Restore a snapshot verbatim with nothing staged — the reproduce path.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle or a failed restore.
    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError>;

    /// Advance the machine through its staged environment.
    ///
    /// # Errors
    ///
    /// Returns an error when execution fails.
    fn run(&mut self, until: StopConditions) -> Result<StopReason, MachineError>;

    /// Read machine memory.
    ///
    /// # Errors
    ///
    /// Returns an error for a range outside the machine's memory.
    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError>;
}
