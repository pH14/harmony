// SPDX-License-Identifier: AGPL-3.0-or-later

//! NES emulator implementation of the machine boundary, wrapping TetaNES.
//!
//! The environment is a controller action suffix: each action is one button
//! mask held for a bounded frame count, applied during [`Machine::run`] and
//! released at the end of its hold. Work RAM is served by [`Machine::read`].
//! Snapshot export/import and the film accessors are emulator extras outside
//! the mirrored verb set.

use std::{
    collections::{BTreeMap, VecDeque},
    io::Cursor,
};

use serde::{Deserialize, Serialize};
use tetanes_core::{
    control_deck::{Config, ControlDeck, HeadlessMode},
    input::{JoypadBtnState, Player},
    memory::RamState,
};

use crate::{Machine, MachineError, Moment, SnapId, StopConditions, StopReason};

/// Size of the NES CPU work RAM exposed through [`Machine::read`].
pub const WRAM_SIZE: usize = 2 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;

/// One total NES input action: an eight-button mask held for a bounded frame count.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ButtonChord {
    /// Standard NES controller bits: A, B, Select, Start, Up, Down, Left, Right.
    pub buttons: u8,
    /// Requested hold duration. Execution clamps this to `1..=MAX_HOLD_FRAMES`.
    pub hold_frames: u8,
}

impl ButtonChord {
    /// Construct a chord, normalizing its duration into the machine's total domain.
    #[must_use]
    pub fn new(buttons: u8, hold_frames: u8) -> Self {
        Self {
            buttons,
            hold_frames: hold_frames.clamp(1, MAX_HOLD_FRAMES),
        }
    }

    /// Return the normalized hold duration used by execution.
    #[must_use]
    pub fn bounded_hold_frames(self) -> u8 {
        self.hold_frames.clamp(1, MAX_HOLD_FRAMES)
    }
}

/// Whether the emulator synthesizes audio and video alongside execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderMode {
    /// Video only, for film boundary frames.
    Video,
    /// Neither, for campaigns.
    Neither,
    /// Both, for film rendering with sound.
    Both,
}

/// Deterministic TetaNES-backed machine.
#[derive(Debug)]
pub struct NesMachine {
    deck: ControlDeck,
    snapshots: BTreeMap<u64, Vec<u8>>,
    next_snap: u64,
    staged: VecDeque<ButtonChord>,
    hold_remaining: u8,
    vtime: u64,
}

impl NesMachine {
    /// Load a ROM at power-on with zeroed RAM.
    ///
    /// # Errors
    ///
    /// Returns an error when ROM loading fails.
    pub fn from_rom_bytes(rom: &[u8], render: RenderMode) -> Result<Self, MachineError> {
        let headless_mode = match render {
            RenderMode::Video => HeadlessMode::NO_AUDIO,
            RenderMode::Neither => HeadlessMode::NO_AUDIO | HeadlessMode::NO_VIDEO,
            RenderMode::Both => HeadlessMode::empty(),
        };
        let mut deck = ControlDeck::with_config(Config {
            ram_state: RamState::AllZeros,
            headless_mode,
            sram_dir: None,
            run_ahead: 0,
            ..Config::default()
        });
        deck.load_rom("campaign.nes", &mut Cursor::new(rom))
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        Ok(Self {
            deck,
            snapshots: BTreeMap::new(),
            next_snap: 0,
            staged: VecDeque::new(),
            hold_remaining: 0,
            vtime: 0,
        })
    }

    /// The machine's current time: total frames emulated by this instance,
    /// staged runs and boot walks included. Restores do not touch it.
    #[must_use]
    pub fn now(&self) -> Moment {
        Moment(self.vtime)
    }

    fn restore_bytes(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        self.deck
            .load_state(Cursor::new(bytes))
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        self.staged.clear();
        self.hold_remaining = 0;
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        Ok(())
    }

    /// Copy one held snapshot's bytes out for persistence. Outside the
    /// mirrored verb set.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle.
    pub fn export_snapshot(&self, snap: SnapId) -> Result<Vec<u8>, MachineError> {
        self.snapshots
            .get(&snap.0)
            .cloned()
            .ok_or(MachineError::UnknownSnapshot)
    }

    /// Hold externally persisted snapshot bytes behind a fresh handle.
    /// Outside the mirrored verb set; validity surfaces at restore.
    pub fn import_snapshot(&mut self, bytes: &[u8]) -> SnapId {
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes.to_vec());
        SnapId(id)
    }

    /// Advance exactly one frame under a raw controller mask, for film
    /// rendering only. Outside the mirrored verb set.
    ///
    /// # Errors
    ///
    /// Returns an error when frame execution fails.
    pub fn clock_frame_with(&mut self, buttons: u8) -> Result<(), MachineError> {
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(buttons));
        self.deck
            .clock_frame()
            .map(|_| ())
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        self.vtime = self.vtime.saturating_add(1);
        Ok(())
    }

    /// Release every controller button, for film rendering only.
    pub fn release_buttons(&mut self) {
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
    }

    /// Return the latest RGBA frame for film generation.
    #[must_use]
    pub fn frame_rgba(&mut self) -> Vec<u8> {
        self.deck.frame_buffer().to_vec()
    }

    /// Return the sound samples mixed for the most recently clocked frame.
    ///
    /// The deck clears this buffer at the start of every clock, so each read
    /// is exactly one frame of audio: 48 kHz mono `f32` under the deck's
    /// default sample rate. Empty without [`RenderMode::Both`].
    #[must_use]
    pub fn audio_samples(&self) -> &[f32] {
        self.deck.audio_samples()
    }

    /// Overwrite one work-RAM byte. Test support only, outside the mirrored
    /// verb set: campaigns never write guest memory.
    #[doc(hidden)]
    pub fn poke_wram(&mut self, addr: usize, byte: u8) {
        if let Some(slot) = self.deck.wram_mut().get_mut(addr) {
            *slot = byte;
        }
    }
}

impl Machine for NesMachine {
    type Env = Vec<ButtonChord>;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let mut bytes = Vec::new();
        self.deck
            .save_state(&mut bytes)
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes);
        Ok(SnapId(id))
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snapshots
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot)
    }

    fn branch(&mut self, snap: SnapId, env: Self::Env) -> Result<(), MachineError> {
        let bytes = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?
            .clone();
        self.restore_bytes(&bytes)?;
        self.staged = env.into();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let bytes = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?
            .clone();
        self.restore_bytes(&bytes)
    }

    fn run(&mut self, until: StopConditions) -> Result<StopReason, MachineError> {
        loop {
            if let Some(deadline) = until.deadline
                && self.vtime >= deadline.0
            {
                return Ok(StopReason::Deadline {
                    vtime: Moment(self.vtime),
                });
            }
            if self.hold_remaining == 0 {
                let Some(chord) = self.staged.pop_front() else {
                    return Ok(StopReason::Quiescent {
                        vtime: Moment(self.vtime),
                    });
                };
                self.deck.joypad_mut(Player::One).buttons =
                    JoypadBtnState::from_bits_truncate(u16::from(chord.buttons));
                self.hold_remaining = chord.bounded_hold_frames();
            }
            self.deck
                .clock_frame()
                .map(|_| ())
                .map_err(|error| MachineError::Backend(error.to_string()))?;
            self.vtime = self.vtime.saturating_add(1);
            self.hold_remaining -= 1;
            if self.hold_remaining == 0 {
                self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
            }
        }
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
        let length = len as usize;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= WRAM_SIZE)
            .ok_or(MachineError::ReadOutOfBounds)?;
        Ok(self.deck.wram()[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonChord, MAX_HOLD_FRAMES, NesMachine, RenderMode, WRAM_SIZE};
    use crate::{Machine, Moment, StopConditions, StopReason};

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn chord_duration_is_total_and_bounded() {
        assert_eq!(ButtonChord::new(0x81, 0).hold_frames, 1);
        assert_eq!(ButtonChord::new(0x81, u8::MAX).hold_frames, MAX_HOLD_FRAMES);
    }

    #[test]
    fn run_consumes_staged_chords_and_respects_the_deadline() {
        let rom = synthetic_nrom();
        let mut machine = NesMachine::from_rom_bytes(&rom, RenderMode::Neither).expect("load rom");
        let start = machine.snapshot().expect("snapshot power-on");
        machine
            .branch(
                start,
                vec![ButtonChord::new(0x01, 4), ButtonChord::new(0, 2)],
            )
            .expect("branch");
        let stop = machine
            .run(StopConditions {
                deadline: Some(Moment(3)),
            })
            .expect("run to deadline");
        assert_eq!(stop, StopReason::Deadline { vtime: Moment(3) });
        let stop = machine
            .run(StopConditions::default())
            .expect("run to quiescence");
        assert_eq!(stop, StopReason::Quiescent { vtime: Moment(6) });
        assert_eq!(machine.now(), Moment(6));
    }

    #[test]
    fn branch_restores_and_reads_stay_in_bounds() {
        let rom = synthetic_nrom();
        let mut machine = NesMachine::from_rom_bytes(&rom, RenderMode::Neither).expect("load rom");
        let start = machine.snapshot().expect("snapshot power-on");
        let before = machine.read(0, 2048).expect("read wram");
        machine
            .branch(start, vec![ButtonChord::new(0x01, 8)])
            .expect("branch");
        machine.run(StopConditions::default()).expect("run");
        machine.replay(start).expect("replay");
        assert_eq!(machine.read(0, 2048).expect("read restored wram"), before);
        assert!(machine.read(2048, 1).is_err());
        assert!(machine.read(u64::MAX, 1).is_err());
        assert!(
            machine
                .read(1, u32::try_from(WRAM_SIZE).expect("len"))
                .is_err()
        );
    }

    #[test]
    fn export_and_import_round_trip_a_snapshot() {
        let rom = synthetic_nrom();
        let mut machine = NesMachine::from_rom_bytes(&rom, RenderMode::Neither).expect("load rom");
        let snap = machine.snapshot().expect("snapshot");
        let bytes = machine.export_snapshot(snap).expect("export");
        machine.drop_snapshot(snap).expect("drop");
        assert!(machine.export_snapshot(snap).is_err());
        let imported = machine.import_snapshot(&bytes);
        machine.replay(imported).expect("replay imported");
        assert_eq!(machine.export_snapshot(imported).expect("re-export"), bytes);
    }
}
