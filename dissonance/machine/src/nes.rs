// SPDX-License-Identifier: AGPL-3.0-or-later

//! NES emulator implementation of the machine boundary, wrapping TetaNES.
//!
//! The environment is a controller action suffix: each action is one button
//! mask held for a bounded frame count, applied during [`Machine::run`] and
//! released at the end of its hold. It travels as an opaque [`Reproducer`]
//! blob that only this module mints and parses, so the searcher never sees a
//! controller. [`Machine::read`] serves the whole CPU address space.
//!
//! The console has no cooperating guest, so it never surfaces a decision, a
//! snapshot point, or an assertion; an invalid opcode surfaces as
//! [`StopReason::Crash`]. Snapshot export/import and the film accessors are
//! emulator extras outside the mirrored verb set.

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

use crate::{
    Answer, CrashInfo, CrashKind, Machine, MachineError, Moment, Reproducer, SnapId,
    StopConditions, StopReason,
};

/// Size of the NES CPU work RAM, the low mirror-free window of the address
/// space [`Machine::read`] serves.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Bytes the NES CPU can address, the length of [`Machine::read`]'s space.
pub const ADDRESS_SPACE_SIZE: u64 = 64 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;

/// Blob format version of a NES [`Reproducer`]: a flat sequence of
/// `(buttons, hold_frames)` byte pairs in execution order.
pub const ENV_BLOB_VERSION: u16 = 1;

/// Mint the environment blob for one controller action suffix.
#[must_use]
pub fn reproducer(actions: &[ButtonChord]) -> Reproducer {
    let mut bytes = Vec::with_capacity(actions.len() * 2);
    for action in actions {
        bytes.push(action.buttons);
        bytes.push(action.bounded_hold_frames());
    }
    Reproducer {
        blob_version: ENV_BLOB_VERSION,
        bytes,
    }
}

/// Parse an environment blob back into its controller action suffix.
///
/// # Errors
///
/// Returns an error for another format version or a truncated blob.
pub fn actions_of(env: &Reproducer) -> Result<Vec<ButtonChord>, MachineError> {
    if env.blob_version != ENV_BLOB_VERSION {
        return Err(MachineError::BadEnvVersion);
    }
    if env.bytes.len() % 2 != 0 {
        return Err(MachineError::MalformedEnv);
    }
    Ok(env
        .bytes
        .chunks_exact(2)
        .map(|pair| ButtonChord::new(pair[0], pair[1]))
        .collect())
}

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

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let actions = actions_of(env)?;
        let bytes = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?
            .clone();
        self.restore_bytes(&bytes)?;
        self.staged = actions.into();
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

    /// The console has no cooperating guest, so no class in `until.on` can
    /// ever surface and the mask is honored vacuously.
    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        if resolve.is_some() {
            return Err(MachineError::ResolveWithoutDecision);
        }
        loop {
            if self.deck.cpu_corrupted() {
                return Ok(StopReason::Crash {
                    vtime: Moment(self.vtime),
                    info: CrashInfo {
                        kind: CrashKind::UnrecoverableFault,
                        detail: b"cpu executed an invalid opcode".to_vec(),
                    },
                });
            }
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

    /// Reads run over the console's whole 64 KiB CPU address space, the NES
    /// analogue of guest-physical memory. Reads are side-effect free: an
    /// address mapped to a hardware register reports its current value
    /// without the read the console itself would perform. The low
    /// [`WRAM_SIZE`] window is served straight from work RAM, which is the
    /// same memory that window addresses.
    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let length = u64::from(len);
        let end = addr
            .checked_add(length)
            .filter(|end| *end <= ADDRESS_SPACE_SIZE)
            .ok_or(MachineError::ReadOutOfBounds)?;
        if end <= WRAM_SIZE as u64 {
            let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
            let finish = usize::try_from(end).map_err(|_| MachineError::ReadOutOfBounds)?;
            return Ok(self.deck.wram()[start..finish].to_vec());
        }
        Ok((addr..end)
            .map(|address| {
                self.deck
                    .bus()
                    .peek(u16::try_from(address).unwrap_or(u16::MAX))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADDRESS_SPACE_SIZE, ButtonChord, ENV_BLOB_VERSION, MAX_HOLD_FRAMES, NesMachine, RenderMode,
        WRAM_SIZE, actions_of, reproducer,
    };
    use crate::{Answer, Machine, MachineError, Moment, Reproducer, StopConditions, StopReason};

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
                &reproducer(&[ButtonChord::new(0x01, 4), ButtonChord::new(0, 2)]),
            )
            .expect("branch");
        let stop = machine
            .run(
                StopConditions {
                    deadline: Some(Moment(3)),
                    ..StopConditions::default()
                },
                None,
            )
            .expect("run to deadline");
        assert_eq!(stop, StopReason::Deadline { vtime: Moment(3) });
        let stop = machine
            .run(StopConditions::default(), None)
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
            .branch(start, &reproducer(&[ButtonChord::new(0x01, 8)]))
            .expect("branch");
        machine.run(StopConditions::default(), None).expect("run");
        machine.replay(start).expect("replay");
        assert_eq!(machine.read(0, 2048).expect("read restored wram"), before);
        // The window past work RAM is still addressable; only the space's
        // end bounds a read.
        assert_eq!(machine.read(2048, 1).expect("read past wram").len(), 1);
        assert_eq!(
            machine
                .read(1, u32::try_from(WRAM_SIZE).expect("len"))
                .expect("read straddling the wram window")
                .len(),
            WRAM_SIZE
        );
        assert!(machine.read(u64::MAX, 1).is_err());
        assert!(machine.read(ADDRESS_SPACE_SIZE, 1).is_err());
        assert!(machine.read(ADDRESS_SPACE_SIZE - 1, 2).is_err());
    }

    /// A read spanning the work-RAM boundary must agree byte for byte with
    /// the two reads that meet at it, so the fast path and the bus path are
    /// the same memory.
    #[test]
    fn the_work_ram_window_and_the_bus_agree() {
        let rom = synthetic_nrom();
        let mut machine = NesMachine::from_rom_bytes(&rom, RenderMode::Neither).expect("load rom");
        for (offset, byte) in [(0_usize, 0x11_u8), (1, 0x22), (2047, 0x33)] {
            machine.poke_wram(offset, byte);
        }
        let straddling = machine.read(2040, 16).expect("read across the boundary");
        let inside = machine.read(2040, 8).expect("read inside work ram");
        let outside = machine.read(2048, 8).expect("read past work ram");
        assert_eq!(straddling[..8], inside[..]);
        assert_eq!(straddling[8..], outside[..]);
        // 0x0800 mirrors work RAM on the NES bus.
        assert_eq!(machine.read(2048, 3).expect("mirror"), vec![0x11, 0x22, 0]);
    }

    #[test]
    fn an_environment_blob_round_trips_and_rejects_foreign_versions() {
        let actions = vec![ButtonChord::new(0x81, 4), ButtonChord::new(0, 200)];
        let env = reproducer(&actions);
        assert_eq!(env.blob_version, ENV_BLOB_VERSION);
        assert_eq!(actions_of(&env).expect("round trip"), actions);
        assert_eq!(
            actions_of(&Reproducer {
                blob_version: ENV_BLOB_VERSION + 1,
                bytes: env.bytes.clone(),
            }),
            Err(MachineError::BadEnvVersion)
        );
        assert_eq!(
            actions_of(&Reproducer {
                blob_version: ENV_BLOB_VERSION,
                bytes: vec![0x01],
            }),
            Err(MachineError::MalformedEnv)
        );
    }

    #[test]
    fn a_run_that_answers_nothing_is_refused() {
        let rom = synthetic_nrom();
        let mut machine = NesMachine::from_rom_bytes(&rom, RenderMode::Neither).expect("load rom");
        assert_eq!(
            machine.run(StopConditions::default(), Some(&Answer(vec![0]))),
            Err(MachineError::ResolveWithoutDecision)
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
