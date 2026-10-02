// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cell::RefCell, fmt};

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

    fn keep_run_capture(&mut self, _frames: usize) {}

    fn capture_nes(&mut self, base: Option<&P>) -> Result<P, MachineError> {
        let snapshot = self.snapshot()?;
        let exported = self.export_nes(snapshot, base);
        let released = self.release_exported(snapshot);
        let state = exported?;
        released?;
        Ok(state)
    }

    fn restore_nes(&mut self, state: &P) -> Result<(), MachineError> {
        let imported = self.import_nes(state)?;
        let replayed = self.replay(imported);
        let dropped = self.drop_snapshot(imported);
        replayed?;
        dropped
    }
}

thread_local! {
    static QUICKNES_STATE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static QUICKNES_PACKED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

impl NesBackend<Vec<u8>> for QuickNesMachine {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        _base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        pack_quicknes_state(self.snapshot_bytes(snapshot)?)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let state = unpack_quicknes_state(self, portable)?;
        Ok(self.import_snapshot(&state))
    }

    fn keep_run_capture(&mut self, frames: usize) {
        QuickNesMachine::keep_run_capture(self, frames);
    }

    fn capture_nes(&mut self, _base: Option<&Vec<u8>>) -> Result<Vec<u8>, MachineError> {
        QUICKNES_STATE.with_borrow_mut(|state| {
            self.capture_into(state)?;
            pack_quicknes_state(state)
        })
    }

    fn restore_nes(&mut self, portable: &Vec<u8>) -> Result<(), MachineError> {
        QUICKNES_STATE.with_borrow_mut(|state| {
            unpack_quicknes_state_into(self, portable, state)?;
            self.restore_bytes(state)
        })
    }
}

fn pack_quicknes_state(state: &[u8]) -> Result<Vec<u8>, MachineError> {
    let declared = u32::try_from(state.len())
        .map_err(|_| MachineError::Backend("snapshot is too large to compress".to_owned()))?;
    QUICKNES_PACKED.with_borrow_mut(|packed| {
        packed.resize(4 + lz4_flex::block::get_maximum_output_size(state.len()), 0);
        packed[..4].copy_from_slice(&declared.to_le_bytes());
        let written = lz4_flex::block::compress_into(state, &mut packed[4..]).map_err(|error| {
            MachineError::Backend(format!("snapshot does not compress: {error}"))
        })?;
        Ok(packed[..4 + written].to_vec())
    })
}

pub fn unpack_quicknes_state(
    machine: &QuickNesMachine,
    packed: &[u8],
) -> Result<Vec<u8>, MachineError> {
    let mut state = Vec::new();
    unpack_quicknes_state_into(machine, packed, &mut state)?;
    Ok(state)
}

fn unpack_quicknes_state_into(
    machine: &QuickNesMachine,
    packed: &[u8],
    state: &mut Vec<u8>,
) -> Result<(), MachineError> {
    let undecodable =
        |error| MachineError::Backend(format!("snapshot does not decompress: {error}"));
    let (declared, block) = lz4_flex::block::uncompressed_size(packed).map_err(undecodable)?;
    let expected = machine.snapshot_len();
    if declared != expected {
        return Err(MachineError::Backend(format!(
            "snapshot declares {declared} bytes; this core's states are {expected}"
        )));
    }
    state.resize(expected, 0);
    let written = lz4_flex::block::decompress_into(block, state).map_err(undecodable)?;
    if written != expected {
        return Err(MachineError::Backend(format!(
            "snapshot decompresses to {written} bytes; this core's states are {expected}"
        )));
    }
    Ok(())
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

    use super::NesBackend;

    #[test]
    fn native_snapshots_store_a_compressed_state_that_restores_exactly() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let held = machine.snapshot().expect("snapshot");
        let state = machine.take_snapshot(held).expect("raw state");
        let held = machine.import_snapshot(&state);
        let stored = machine.export_nes(held, None).expect("export");
        assert_eq!(stored, lz4_flex::block::compress_prepend_size(&state));
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
        let captured = machine.capture_nes(None).expect("capture");
        let held = machine.snapshot().expect("snapshot");
        let raw = machine.take_snapshot(held).expect("raw state");
        assert_eq!(captured, lz4_flex::block::compress_prepend_size(&raw));
        machine.poke_wram(0x10, 9);
        machine.restore_nes(&captured).expect("restore");
        assert_eq!(machine.read_wram().expect("wram")[0x10], 7);
        assert_eq!(machine.capture_nes(None).expect("capture"), captured);
        assert!(machine.restore_nes(&vec![4, 0, 0, 0, 0xf0]).is_err());
        assert_eq!(machine.capture_nes(None).expect("capture"), captured);
    }
}
