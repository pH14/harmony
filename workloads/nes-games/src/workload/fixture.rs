// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
use serde::{Deserialize, Serialize};

use super::{Game, Moves, Never, Step};
use crate::{
    chord::ChordVocabulary,
    nes_backend::NesBackend,
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
}

impl Stepper {
    fn wram(&self) -> [u8; WRAM_SIZE] {
        let mut wram = [0; WRAM_SIZE];
        wram[..2].copy_from_slice(&self.ram);
        wram
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
        _until: StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.frames.clear();
        for chord in std::mem::take(&mut self.staged) {
            for _ in 0..chord.bounded_hold_frames() {
                self.step(chord.buttons);
                self.frames.push(self.wram());
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
