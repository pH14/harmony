// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cell::RefCell, fmt, ops::Range};

#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use machine::{Machine, MachineError, SnapId, nes::WRAM_SIZE};
use serde::{Serialize, de::DeserializeOwned};

#[must_use]
pub fn native_identity_sha256(core_sha256: &str, execution_sha256: &str) -> String {
    format!("{core_sha256};prepared-execution-sha256={execution_sha256}")
}

pub trait SnapshotState:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned + 'static
{
    type WorkRam: WorkRamCopy;

    fn memory_charge(&self) -> usize;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>>;

    fn logical_identity(&self) -> Result<Self, MachineError> {
        Ok(self.clone())
    }
}

pub trait WorkRamCopy:
    Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned + 'static
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
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.array::<u8>(self.capacity())
    }

    fn logical_identity(&self) -> Result<Self, MachineError> {
        let mut identity = self.clone();
        if let Ok((_, header_len)) = packed_preamble(self)
            && let Some(header) = identity
                .get_mut(PACKED_PREAMBLE_LEN..)
                .and_then(|rest| rest.get_mut(..header_len))
        {
            machine::nes::clear_execution_sha256(header);
        }
        Ok(identity)
    }
}

pub trait NesBackend<P>: Machine
where
    P: SnapshotState,
{
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

pub trait NesMemory {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError>;

    fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError>;

    fn capture_save_ram_per_frame(&mut self, range: Range<usize>);

    fn save_ram_frames(&self) -> &[u8];

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError>;

    fn write_save_ram(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MachineError>;
}

#[cfg(unix)]
impl NesMemory for QuickNesMachine {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        QuickNesMachine::read_wram(self)
    }

    fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        QuickNesMachine::poke_wram(self, addr, byte)
    }

    fn capture_save_ram_per_frame(&mut self, range: Range<usize>) {
        QuickNesMachine::capture_save_ram_per_frame(self, range);
    }

    fn save_ram_frames(&self) -> &[u8] {
        QuickNesMachine::save_ram_frames(self)
    }

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        QuickNesMachine::read_save_ram(self)
    }

    fn write_save_ram(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MachineError> {
        QuickNesMachine::write_save_ram(self, offset, bytes)
    }
}

#[cfg(unix)]
thread_local! {
    static QUICKNES_STATE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    static PACKED_STATE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[cfg(unix)]
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

const PACKED_MARKER: &[u8; 4] = b"QNP2";
const PACKED_PREAMBLE_LEN: usize = 12;

pub fn pack_quicknes_state(state: &[u8]) -> Result<Vec<u8>, MachineError> {
    let too_large = || MachineError::Backend("snapshot is too large to compress".to_owned());
    let declared = u32::try_from(state.len()).map_err(|_| too_large())?;
    let header_len = state.len() - machine::nes::core_state(state).len();
    let (header, core) = state.split_at(header_len);
    let header_declared = u32::try_from(header_len).map_err(|_| too_large())?;
    PACKED_STATE.with_borrow_mut(|packed| {
        let block_start = PACKED_PREAMBLE_LEN + header_len;
        packed.resize(
            block_start + lz4_flex::block::get_maximum_output_size(core.len()),
            0,
        );
        packed[..4].copy_from_slice(PACKED_MARKER);
        packed[4..8].copy_from_slice(&declared.to_le_bytes());
        packed[8..12].copy_from_slice(&header_declared.to_le_bytes());
        packed[PACKED_PREAMBLE_LEN..block_start].copy_from_slice(header);
        let written =
            lz4_flex::block::compress_into(core, &mut packed[block_start..]).map_err(|error| {
                MachineError::Backend(format!("snapshot does not compress: {error}"))
            })?;
        Ok(packed[..block_start + written].to_vec())
    })
}

pub fn packed_state_len(packed: &[u8]) -> Result<usize, MachineError> {
    Ok(packed_preamble(packed)?.0)
}

fn packed_preamble(packed: &[u8]) -> Result<(usize, usize), MachineError> {
    if packed.get(..4) != Some(PACKED_MARKER.as_slice()) || packed.len() < PACKED_PREAMBLE_LEN {
        return Err(MachineError::Backend(
            "snapshot is not a packed QuickNES state of format 2".to_owned(),
        ));
    }
    let field = |range: Range<usize>| {
        packed[range]
            .try_into()
            .map(u32::from_le_bytes)
            .map_err(|_| MachineError::MalformedEnv)
            .and_then(|value| usize::try_from(value).map_err(|_| MachineError::MalformedEnv))
    };
    Ok((field(4..8)?, field(8..12)?))
}

#[cfg(unix)]
pub fn unpack_quicknes_state(
    machine: &QuickNesMachine,
    packed: &[u8],
) -> Result<Vec<u8>, MachineError> {
    let mut state = Vec::new();
    unpack_quicknes_state_into(machine, packed, &mut state)?;
    Ok(state)
}

#[cfg(unix)]
fn unpack_quicknes_state_into(
    machine: &QuickNesMachine,
    packed: &[u8],
    state: &mut Vec<u8>,
) -> Result<(), MachineError> {
    unpack_state_into(packed, machine.snapshot_len(), state)
}

pub fn unpack_state_into(
    packed: &[u8],
    expected: usize,
    state: &mut Vec<u8>,
) -> Result<(), MachineError> {
    let undecodable =
        |error| MachineError::Backend(format!("snapshot does not decompress: {error}"));
    let (declared, header_len) = packed_preamble(packed)?;
    if declared != expected {
        return Err(MachineError::Backend(format!(
            "snapshot declares {declared} bytes; this core's states are {expected}"
        )));
    }
    let block_start = PACKED_PREAMBLE_LEN
        .checked_add(header_len)
        .filter(|start| header_len <= expected && *start <= packed.len())
        .ok_or_else(|| MachineError::Backend("snapshot header is truncated".to_owned()))?;
    state.resize(expected, 0);
    state[..header_len].copy_from_slice(&packed[PACKED_PREAMBLE_LEN..block_start]);
    let written =
        lz4_flex::block::decompress_into(&packed[block_start..], &mut state[header_len..])
            .map_err(undecodable)?;
    if header_len + written != expected {
        return Err(MachineError::Backend(format!(
            "snapshot decompresses to {} bytes; this core's states are {expected}",
            header_len + written
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

    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.reserve(
            self.checkpoint_owned_bytes()
                .ok_or(searcher::search::checkpoint::image::ImageLimit)?,
        )?;
        self.visit_checkpoint_allocations(|domain, identity, bytes| {
            budget.shared(domain, identity, bytes)
        })
    }

    fn logical_identity(&self) -> Result<Self, MachineError> {
        self.logical_identity().map_err(MachineError::Backend)
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

#[cfg(all(test, unix))]
mod tests {
    use machine::{Machine, nes::STATE_HEADER_LEN, quicknes::QuickNesMachine};

    use super::{NesBackend, pack_quicknes_state, packed_state_len, unpack_state_into};

    fn unpacked(packed: &[u8], len: usize) -> Vec<u8> {
        let mut state = Vec::new();
        unpack_state_into(packed, len, &mut state).expect("unpack");
        state
    }

    #[test]
    fn native_snapshots_store_a_compressed_state_that_restores_exactly() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let held = machine.snapshot().expect("snapshot");
        let state = machine.take_snapshot(held).expect("raw state");
        let held = machine.import_snapshot(&state);
        let stored = machine.export_nes(held, None).expect("export");
        assert_eq!(&stored[..4], b"QNP2");
        assert_eq!(packed_state_len(&stored).expect("length"), state.len());
        assert_eq!(
            &stored[12..12 + STATE_HEADER_LEN],
            &state[..STATE_HEADER_LEN]
        );
        assert_eq!(unpacked(&stored, state.len()), state);
        assert!(stored.len() < state.len());
        assert_eq!(stored.capacity(), stored.len());
        let restored = machine.import_nes(&stored).expect("import");
        assert_eq!(machine.take_snapshot(restored).expect("raw state"), state);
        assert!(machine.import_nes(&vec![4, 0, 0, 0, 0xf0]).is_err());
        assert!(
            machine
                .import_nes(&lz4_flex::block::compress_prepend_size(&state))
                .is_err()
        );
        let mut oversized = stored.clone();
        oversized[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(machine.import_nes(&oversized).is_err());
        let mut long_header = stored.clone();
        long_header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(machine.import_nes(&long_header).is_err());
        let mut short = pack_quicknes_state(&state[..state.len() - 1]).expect("pack");
        short[4..8].copy_from_slice(&u32::try_from(state.len()).unwrap().to_le_bytes());
        assert!(machine.import_nes(&short).is_err());
    }

    #[test]
    fn a_captured_state_restores_the_machine_it_came_from() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        machine.poke_wram(0x10, 7).unwrap();
        let captured = machine.capture_nes(None).expect("capture");
        let held = machine.snapshot().expect("snapshot");
        let raw = machine.take_snapshot(held).expect("raw state");
        assert_eq!(captured, pack_quicknes_state(&raw).expect("pack"));
        machine.poke_wram(0x10, 9).unwrap();
        machine.restore_nes(&captured).expect("restore");
        assert_eq!(machine.read_wram().expect("wram")[0x10], 7);
        assert_eq!(machine.capture_nes(None).expect("capture"), captured);
        assert!(machine.restore_nes(&vec![4, 0, 0, 0, 0xf0]).is_err());
        assert_eq!(machine.capture_nes(None).expect("capture"), captured);
    }

    #[test]
    fn equal_core_states_pack_to_equal_lengths_under_different_execution_hashes() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let held = machine.snapshot().expect("snapshot");
        let own = machine.take_snapshot(held).expect("raw state");
        let mut foreign = own.clone();
        for (index, byte) in foreign[112..176].iter_mut().enumerate() {
            *byte = b"0123456789abcdef"[(index * 7 + index / 5) % 16];
        }
        assert_ne!(own[112..176], foreign[112..176]);
        assert_eq!(own[STATE_HEADER_LEN..], foreign[STATE_HEADER_LEN..]);
        let packed_own = pack_quicknes_state(&own).expect("pack");
        let packed_foreign = pack_quicknes_state(&foreign).expect("pack");
        assert_eq!(packed_own.len(), packed_foreign.len());
        assert_eq!(unpacked(&packed_own, own.len()), own);
        assert_eq!(unpacked(&packed_foreign, foreign.len()), foreign);
        machine
            .restore_nes(&packed_own)
            .expect("own build restores");
        assert!(machine.restore_nes(&packed_foreign).is_err());
    }

    #[test]
    fn a_state_without_the_machine_header_packs_whole() {
        let state = vec![7_u8; 300];
        let packed = pack_quicknes_state(&state).expect("pack");
        assert_eq!(&packed[8..12], &0_u32.to_le_bytes());
        assert_eq!(unpacked(&packed, state.len()), state);
    }
}
