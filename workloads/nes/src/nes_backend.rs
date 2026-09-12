// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

use machine::{Machine, MachineError, SnapId};
use serde::{Serialize, de::DeserializeOwned};

pub trait SnapshotState:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned
{
    fn memory_charge(&self) -> usize;
}

impl SnapshotState for Vec<u8> {
    fn memory_charge(&self) -> usize {
        self.len()
    }
}

pub trait NesBackend<P>: Machine
where
    P: SnapshotState,
{
    fn starts_at_power_on(&self) -> bool {
        true
    }

    fn export_nes(&mut self, snapshot: SnapId, base: Option<&P>) -> Result<P, MachineError>;

    fn import_nes(&mut self, portable: &P) -> Result<SnapId, MachineError>;

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
        self.take_snapshot(snapshot)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        Ok(self.import_snapshot(portable))
    }

    fn release_exported(&mut self, _snapshot: SnapId) -> Result<(), MachineError> {
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
