// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

use machine::{Machine, MachineError, SnapId, nes::WRAM_SIZE, quicknes::QuickNesMachine};
use serde::{Serialize, de::DeserializeOwned};

pub trait SnapshotState:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned
{
    type WorkRam: WorkRamCopy;

    fn memory_charge(&self) -> usize;
}

pub trait WorkRamCopy:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned
{
    fn copy_of(wram: &[u8; WRAM_SIZE]) -> Self;

    fn work_ram(&self) -> Option<&[u8]>;

    fn memory_charge(&self) -> usize;
}

impl WorkRamCopy for () {
    fn copy_of(_wram: &[u8; WRAM_SIZE]) -> Self {}

    fn work_ram(&self) -> Option<&[u8]> {
        None
    }

    fn memory_charge(&self) -> usize {
        0
    }
}

impl WorkRamCopy for Box<[u8]> {
    fn copy_of(wram: &[u8; WRAM_SIZE]) -> Self {
        wram.as_slice().into()
    }

    fn work_ram(&self) -> Option<&[u8]> {
        Some(self)
    }

    fn memory_charge(&self) -> usize {
        self.len()
    }
}

impl SnapshotState for Vec<u8> {
    type WorkRam = ();

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

impl NesBackend<Vec<u8>> for QuickNesMachine {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        _base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        let mut packed = lz4_flex::block::compress_prepend_size(self.snapshot_bytes(snapshot)?);
        packed.shrink_to_fit();
        Ok(packed)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let state = unpack_quicknes_state(self, portable)?;
        Ok(self.import_snapshot(&state))
    }
}

pub fn unpack_quicknes_state(
    machine: &QuickNesMachine,
    packed: &[u8],
) -> Result<Vec<u8>, MachineError> {
    let undecodable =
        |error| MachineError::Backend(format!("snapshot does not decompress: {error}"));
    let (declared, block) = lz4_flex::block::uncompressed_size(packed).map_err(undecodable)?;
    let expected = machine.snapshot_len();
    if declared != expected {
        return Err(MachineError::Backend(format!(
            "snapshot declares {declared} bytes; this core's states are {expected}"
        )));
    }
    let mut state = vec![0; expected];
    let written = lz4_flex::block::decompress_into(block, &mut state).map_err(undecodable)?;
    if written != expected {
        return Err(MachineError::Backend(format!(
            "snapshot decompresses to {written} bytes; this core's states are {expected}"
        )));
    }
    Ok(state)
}

pub fn capture_nes<M, P>(machine: &mut M, base: Option<&P>) -> Result<P, MachineError>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    let snapshot = machine.snapshot()?;
    let exported = machine.export_nes(snapshot, base);
    let released = machine.release_exported(snapshot);
    let state = exported?;
    released?;
    Ok(state)
}

pub fn restore_nes<M, P>(machine: &mut M, state: &P) -> Result<(), MachineError>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    let imported = machine.import_nes(state)?;
    let replayed = machine.replay(imported);
    let dropped = machine.drop_snapshot(imported);
    replayed?;
    dropped
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl SnapshotState for machine::consonance::ConsonancePortable {
    type WorkRam = Box<[u8]>;

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

#[cfg(test)]
mod tests {
    use machine::{Machine, quicknes::QuickNesMachine};

    use super::{NesBackend, capture_nes, restore_nes};

    #[test]
    fn native_snapshots_store_a_compressed_state_that_restores_exactly() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let held = machine.snapshot().expect("snapshot");
        let state = machine.take_snapshot(held).expect("raw state");
        let held = machine.import_snapshot(&state);
        let stored = machine.export_nes(held, None).expect("export");
        assert!(stored.len() < state.len());
        assert_eq!(stored.capacity(), stored.len());
        let restored = machine.import_nes(&stored).expect("import");
        assert_eq!(machine.take_snapshot(restored).expect("raw state"), state);
        assert!(machine.import_nes(&vec![4, 0, 0, 0, 0xf0]).is_err());
        let mut oversized = stored.clone();
        oversized[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(machine.import_nes(&oversized).is_err());
        let mut short = lz4_flex::block::compress_prepend_size(&state[..state.len() - 1]);
        short[..4].copy_from_slice(&u32::try_from(state.len()).unwrap().to_le_bytes());
        assert!(machine.import_nes(&short).is_err());
    }

    #[test]
    fn a_captured_state_restores_the_machine_it_came_from() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        machine.poke_wram(0x10, 7);
        let captured = capture_nes(&mut machine, None).expect("capture");
        machine.poke_wram(0x10, 9);
        restore_nes(&mut machine, &captured).expect("restore");
        assert_eq!(machine.read_wram().expect("wram")[0x10], 7);
        assert_eq!(capture_nes(&mut machine, None).expect("capture"), captured);
    }
}
