// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reusable NES backend seams for workload-owned targets.
//!
//! Native QuickNES snapshots intentionally remain their existing byte vector
//! on the wire.  Whole-VM backends use their own portable snapshot value so a
//! workload can retain the Consonance identity and sparse-page sharing rules
//! without teaching the search coordinator about either backend.

use std::fmt;

use machine::{Machine, MachineError, SnapId};
use serde::{Serialize, de::DeserializeOwned};

/// A portable snapshot value owned by one NES backend.
pub trait SnapshotState:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned
{
    /// Conservative resident memory charge for one archived snapshot.
    fn memory_charge(&self) -> usize;
}

impl SnapshotState for Vec<u8> {
    fn memory_charge(&self) -> usize {
        self.len()
    }
}

/// The machine operations needed by NES workload targets.
pub trait NesBackend<P>: Machine
where
    P: SnapshotState,
{
    /// Whether a newly constructed machine is at the game-neutral NES
    /// power-on state expected by workload boot walks.
    fn starts_at_power_on(&self) -> bool {
        true
    }

    /// Export a held machine snapshot in the workload's portable form.
    fn export_nes(&mut self, snapshot: SnapId, base: Option<&P>) -> Result<P, MachineError>;

    /// Import a workload portable snapshot behind a fresh machine handle.
    fn import_nes(&mut self, portable: &P) -> Result<SnapId, MachineError>;

    /// Release the handle passed to [`Self::export_nes`]. Native QuickNES
    /// consumes that handle while exporting its legacy byte representation.
    fn release_exported(&mut self, snapshot: SnapId) -> Result<(), MachineError> {
        self.drop_snapshot(snapshot)
    }
}

impl NesBackend<Vec<u8>> for machine::quicknes::QuickNesMachine {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        _base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        // `take_snapshot` preserves the pre-extraction QuickNES snapshot
        // bytes exactly; using Machine::export here would replace them with
        // the SharedState serde representation.
        self.take_snapshot(snapshot)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        Ok(self.import_snapshot(portable))
    }

    fn release_exported(&mut self, _snapshot: SnapId) -> Result<(), MachineError> {
        // `take_snapshot` consumed the handle as part of export.
        Ok(())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl SnapshotState for machine::consonance::ConsonancePortable {
    fn memory_charge(&self) -> usize {
        self.memory_charge()
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl NesBackend<machine::consonance::ConsonancePortable>
    for machine::consonance::ConsonanceMachine
{
    fn starts_at_power_on(&self) -> bool {
        machine::consonance::ConsonanceMachine::starts_at_power_on(self)
    }

    fn export_nes(
        &mut self,
        snapshot: SnapId,
        base: Option<&machine::consonance::ConsonancePortable>,
    ) -> Result<machine::consonance::ConsonancePortable, MachineError> {
        self.export(snapshot, base)
    }

    fn import_nes(
        &mut self,
        portable: &machine::consonance::ConsonancePortable,
    ) -> Result<SnapId, MachineError> {
        self.import(portable)
    }
}
