// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, VecDeque},
    ops::Range,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
use serde::{Deserialize, Serialize};

use super::{Game, Moves, Never, Step};
use crate::{
    chord::ChordVocabulary,
    nes_backend::{NesBackend, NesMemory},
    search::assertion::{Assertion, Declarations, Number},
};

pub const RIGHT: u8 = 1;
pub const LEFT: u8 = 2;
pub const WARP: u8 = 4;

#[derive(Default)]
pub struct Stepper {
    ram: [u8; 2],
    held: BTreeMap<u64, [u8; 2]>,
    next: u64,
    staged: Vec<ButtonChord>,
    frames: Vec<[u8; WRAM_SIZE]>,
    video: Option<VecDeque<[u8; 2]>>,
    audio: Option<Vec<i16>>,
    pub runs: usize,
    pub drift: u8,
}

impl Stepper {
    fn wram(&self) -> [u8; WRAM_SIZE] {
        let mut wram = [0; WRAM_SIZE];
        wram[..2].copy_from_slice(&self.ram);
        wram
    }

    fn frame(&mut self, buttons: u8) {
        self.step(buttons);
        self.ram[0] = self.ram[0].wrapping_add(self.drift);
        self.frames.push(self.wram());
        if let Some(video) = &mut self.video {
            video.push_back(self.ram);
        }
        if let Some(audio) = &mut self.audio {
            audio.extend([0, 0]);
        }
    }

    fn step(&mut self, buttons: u8) {
        if buttons & WARP != 0 {
            self.ram = [0, self.ram[1].wrapping_add(1)];
        } else if buttons & RIGHT != 0 {
            self.ram[0] = self.ram[0].saturating_add(1);
        } else if buttons & LEFT != 0 {
            self.ram[0] = self.ram[0].saturating_sub(1);
        }
    }
}

impl Machine for Stepper {
    type Portable = Vec<u8>;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        self.next += 1;
        self.held.insert(self.next, self.ram);
        Ok(SnapId(self.next))
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.held.remove(&snap.0);
        Ok(())
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        self.replay(snap)?;
        self.staged = actions_of(env)?;
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.ram = *self
            .held
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        Ok(())
    }

    fn run(
        &mut self,
        until: StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.frames.clear();
        self.runs += 1;
        if until.deadline.is_some() {
            let Some(chord) = self.staged.first().copied() else {
                return Ok(StopReason::Quiescent { vtime: Moment(0) });
            };
            self.frame(chord.buttons);
            match chord.bounded_hold_frames() {
                0 | 1 => {
                    self.staged.remove(0);
                }
                hold => self.staged[0] = ButtonChord::new(chord.buttons, hold - 1),
            }
            return Ok(StopReason::Deadline { vtime: Moment(0) });
        }
        for chord in std::mem::take(&mut self.staged) {
            for _ in 0..chord.bounded_hold_frames() {
                self.frame(chord.buttons);
            }
        }
        Ok(StopReason::Quiescent { vtime: Moment(0) })
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let start = usize::try_from(addr).map_err(|_| MachineError::UnknownSnapshot)?;
        let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
        self.wram()
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(MachineError::UnknownSnapshot)
    }

    fn export(&mut self, snap: SnapId, _base: Option<&Vec<u8>>) -> Result<Vec<u8>, MachineError> {
        self.held
            .get(&snap.0)
            .map(|ram| ram.to_vec())
            .ok_or(MachineError::UnknownSnapshot)
    }

    fn import(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let ram = portable
            .as_slice()
            .try_into()
            .map_err(|_| MachineError::UnknownSnapshot)?;
        self.next += 1;
        self.held.insert(self.next, ram);
        Ok(SnapId(self.next))
    }

    fn portable_memory_charge(portable: &Vec<u8>) -> usize {
        portable.len()
    }

    fn now(&self) -> Moment {
        Moment(0)
    }

    fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        &self.frames
    }
}

impl NesBackend<Vec<u8>> for Stepper {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        self.export(snapshot, base)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        self.import(portable)
    }
}

impl NesMemory for Stepper {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        Ok(self.wram())
    }

    fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        *self
            .ram
            .get_mut(addr)
            .ok_or_else(|| MachineError::Backend(format!("no work RAM at {addr}")))? = byte;
        Ok(())
    }

    fn capture_save_ram_per_frame(&mut self, _range: Range<usize>) {}

    fn save_ram_frames(&self) -> &[u8] {
        &[]
    }

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        Ok(Vec::new())
    }

    fn write_save_ram(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), MachineError> {
        Ok(())
    }
}

#[cfg(unix)]
impl crate::nes_backend::FilmMachine for Stepper {
    fn set_video_capture(&mut self, enabled: bool) {
        self.video = enabled.then(VecDeque::new);
    }

    fn set_audio_capture(&mut self, enabled: bool) {
        self.audio = enabled.then(Vec::new);
    }

    fn take_video_frame(&mut self) -> Option<machine::quicknes::VideoFrame> {
        self.video
            .as_mut()?
            .pop_front()
            .map(|[x, room]| machine::quicknes::VideoFrame {
                width: 1,
                height: 1,
                rgb24: vec![x, room, 0],
            })
    }

    fn take_audio_samples(&mut self) -> Vec<i16> {
        self.audio.as_mut().map(std::mem::take).unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Spot {
    pub x: u8,
    pub room: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Ledge;

impl Game for Ledge {
    type State = Spot;
    type Move = Never;

    const NAME: &'static str = "ledge";
    const STRATEGY: &'static str = r#"
capacity = 1
goal = "room >= 9"
spread = "room"
identity = "highest(x, room)"
[[stage]]
name = "ledge"
goal = "far"
latch = true
[[stage]]
name = "beyond"
goal = "false"
"#;
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &[RIGHT, LEFT, WARP],
        tap: None,
        short_hold: (1, 2),
        long_hold: (3, 4),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        Vec::new()
    }

    fn genesis(&self, _: &Spot) -> Result<(), String> {
        Ok(())
    }

    fn decode(&self, _: Option<&Spot>, ram: &[u8]) -> Spot {
        Spot {
            x: ram[0],
            room: ram[1],
        }
    }

    fn declare(&self, state: &Spot, out: &mut impl Declarations) {
        out.declare(Assertion::Value {
            name: "x",
            value: Number::byte(state.x),
        });
        out.declare(Assertion::Value {
            name: "room",
            value: Number::byte(state.room),
        });
        out.declare(Assertion::Sometimes {
            name: "far",
            condition: state.x >= 3,
        });
    }

    fn moves(&self, _: &Spot) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _: &Spot) -> Option<Step<Never>> {
        match mv {}
    }

    fn move_frames(&self, mv: Never) -> u64 {
        match mv {}
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LedgeKey {
    pub stage: u8,
    pub room: u8,
    pub band: u8,
}

impl LedgeKey {
    #[must_use]
    pub fn raw(spot: Spot) -> Self {
        Self {
            stage: u8::from(spot.x >= 3),
            room: spot.room,
            band: spot.x,
        }
    }
}

impl crate::search::archive::ArchiveKey for LedgeKey {
    type Place = u8;
    type Progress = u8;
    type Identity = u8;
    type Lineage = ();
    fn place(self) -> u8 {
        self.room
    }
    fn progress(self) -> u8 {
        self.stage
    }
    fn identity(self) -> u8 {
        self.band
    }
    fn capacity() -> usize {
        1
    }
    fn complete(self, parent: Option<(Self, &())>) -> Self {
        let Some((parent, ())) = parent else {
            return self;
        };
        Self {
            stage: self.stage.max(parent.stage),
            band: if parent.room == self.room {
                self.band.max(parent.band)
            } else {
                self.band
            },
            ..self
        }
    }
    fn record(_: &mut (), _: Self) {}
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Landing {
    pub x: u8,
    pub room: u8,
    pub dwell: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Climb(u8);

#[derive(Clone, Copy, Debug, Default)]
pub struct Stairs;

impl Game for Stairs {
    type State = Landing;
    type Move = Climb;

    const NAME: &'static str = "stairs";
    const STRATEGY: &'static str = r#"
capacity = 2
goal = "room >= 9"
spread = "room"
identity = ["highest(x, room)", "dwell"]
"#;
    const CHORDS: ChordVocabulary = Ledge::CHORDS;

    fn setup(&self) -> Vec<ButtonChord> {
        Vec::new()
    }

    fn genesis(&self, _: &Landing) -> Result<(), String> {
        Ok(())
    }

    fn decode(&self, previous: Option<&Landing>, ram: &[u8]) -> Landing {
        let room = ram[1];
        Landing {
            x: ram[0],
            room,
            dwell: previous
                .filter(|previous| previous.room == room)
                .map_or(0, |previous| previous.dwell.saturating_add(1)),
        }
    }

    fn declare(&self, state: &Landing, out: &mut impl Declarations) {
        out.declare(Assertion::Value {
            name: "x",
            value: Number::byte(state.x),
        });
        out.declare(Assertion::Value {
            name: "room",
            value: Number::byte(state.room),
        });
        out.declare(Assertion::Value {
            name: "dwell",
            value: Number::byte(state.dwell),
        });
    }

    fn moves(&self, state: &Landing) -> Moves<Climb> {
        if state.x == 6 {
            Moves::forced(Climb(3))
        } else {
            Moves::none()
        }
    }

    fn play(&self, Climb(left): Climb, _: &Landing) -> Option<Step<Climb>> {
        Some(Step {
            chord: ButtonChord::new(if left == 1 { LEFT } else { RIGHT }, 1),
            rest: (left > 1).then(|| Climb(left - 1)),
        })
    }

    fn move_frames(&self, Climb(left): Climb) -> u64 {
        u64::from(left)
    }
}
