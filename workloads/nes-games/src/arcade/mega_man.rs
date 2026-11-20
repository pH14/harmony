// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    mm2::{
        action::Mm2Action,
        campaign::{Mm2CampaignRun, Mm2Game},
        game::{MegaMan2, Mm2State},
        target::{
            GAMEPLAY_BANK, Mm2MechanicalState, Mm2Observations, Mm2Snapshot, PLAYER_STATE_STANDING,
            decode_state, facing_right,
        },
    },
    nes_backend::{NesBackend, NesMemory},
    search::draw::DrawMixture,
    workload::Arcade,
};
use machine::nes::{ButtonChord, WRAM_SIZE};
use serde::{Deserialize, Serialize};

use super::{ArcadeAction, ArcadeGame, HIDDEN, Machines};

impl ArcadeAction for Mm2Action {
    fn chord(&self) -> Option<ButtonChord> {
        match self {
            Self::Chord(chord) => Some(*chord),
            Self::Switch(_) => None,
        }
    }

    fn planned_frames(&self) -> u64 {
        Mm2Action::planned_frames(self)
    }
}

const ORDER: [u8; 14] = [7, 5, 6, 1, 3, 0, 2, 4, 8, 9, 10, 11, 12, 13];
const STAGE_COUNT: u8 = 14;
const SCREEN_HEIGHT: u16 = 224;
const CROP_TOP: u8 = 8;
const STAGES: [(u8, &[u8]); 14] = [
    (0, &[0x47, 0x40, 0x4a, 0x40, 0x20, 0x20, 0x00]),
    (0, &[0x49, 0x40, 0x28, 0x20, 0x00]),
    (
        0,
        &[
            0x44, 0x40, 0x20, 0x20, 0x20, 0x80, 0x80, 0x44, 0x40, 0x40, 0x40, 0x22, 0x20, 0x00,
        ],
    ),
    (0, &[0x44, 0x40, 0x40, 0x40, 0x26, 0x24, 0x20, 0x00]),
    (
        0,
        &[
            0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40,
            0x40, 0x22, 0x20, 0x00,
        ],
    ),
    (
        0,
        &[0x46, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x24, 0x20, 0x00],
    ),
    (0, &[0x49, 0x40, 0x28, 0x20, 0x00]),
    (
        0,
        &[
            0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
            0x21, 0x20, 0x00,
        ],
    ),
    (24, &[0x88, 0x80, 0x80, 0x80, 0x80, 0x80, 0x04]),
    (22, &[0x45, 0x40, 0x40, 0x40, 0x45, 0x40, 0x40, 0x00]),
    (
        24,
        &[0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x22, 0x00],
    ),
    (
        22,
        &[
            0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x41, 0x40, 0x40, 0x40, 0x40, 0x23, 0x00,
        ],
    ),
    (24, &[0x00]),
    (22, &[0x40, 0x40, 0x40, 0x25, 0x00]),
];

fn cell(stage: u8, screen: u8) -> (u16, u16) {
    let (start, entries) = STAGES[usize::from(stage.min(STAGE_COUNT - 1))];
    let mut placed = Vec::new();
    let (mut col, mut row, mut first) = (0_i32, 0_i32, i32::from(start));
    for &entry in entries {
        let count = i32::from(entry & 31) + 1;
        for k in 0..count {
            placed.push((first + k, col + k, row));
        }
        first += count;
        col += count - 1;
        if entry & 0x40 != 0 {
            row += 1;
        } else if entry & 0x80 != 0 {
            row -= 1;
        } else if entry & 0x20 != 0 {
            col += 1;
        }
    }
    let top = placed.iter().map(|p| p.2).min().unwrap_or(0);
    let found = placed
        .iter()
        .find(|p| p.0 == i32::from(screen))
        .or_else(|| placed.iter().find(|p| p.0 == i32::from(start)))
        .copied()
        .unwrap_or((0, 0, 0));
    (
        u16::try_from(found.1).unwrap_or(0),
        u16::try_from(found.2 - top).unwrap_or(0),
    )
}

#[derive(Clone, Copy)]
struct Body {
    stage: u8,
    screen: u8,
    x: u8,
    y: u8,
    health: u8,
    standing: bool,
    playable: bool,
    dead: bool,
    goal: bool,
    facing_right: bool,
}

impl From<Mm2MechanicalState> for Body {
    fn from(state: Mm2MechanicalState) -> Self {
        Self {
            stage: state.stage,
            screen: state.screen,
            x: state.x,
            y: state.y,
            health: state.health,
            standing: state.player_state == PLAYER_STATE_STANDING,
            playable: state.playable(),
            dead: state.is_dead() || state.is_dying(),
            goal: state.bank == GAMEPLAY_BANK && state.stage < STAGE_COUNT && state.stage_cleared(),
            facing_right: false,
        }
    }
}

impl From<&Mm2State> for Body {
    fn from(state: &Mm2State) -> Self {
        Self {
            stage: state.stage,
            screen: state.screen,
            x: state.x,
            y: state.y,
            health: state.health,
            standing: state.player_state == PLAYER_STATE_STANDING,
            playable: state.playable(),
            dead: state.is_dead() || state.is_dying(),
            goal: state.bank == GAMEPLAY_BANK && state.stage < STAGE_COUNT && state.stage_cleared(),
            facing_right: state.facing_right,
        }
    }
}

fn level(body: Body) -> u8 {
    ORDER
        .iter()
        .position(|&stage| stage == body.stage)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or(0)
}

fn position(body: Body) -> (u16, u16) {
    let (col, row) = cell(body.stage, body.screen);
    let top = row * SCREEN_HEIGHT;
    (
        col * 256 + u16::from(body.x),
        (top + u16::from(body.y.saturating_sub(CROP_TOP))).min(top + SCREEN_HEIGHT - 1),
    )
}

fn in_play(body: Body) -> bool {
    body.stage < STAGE_COUNT && body.playable
}

fn cleared(body: Body) -> u8 {
    if body.stage == STAGE_COUNT - 1 && body.goal {
        STAGE_COUNT
    } else {
        level(body) + u8::from(body.goal)
    }
}

fn motion(body: Body) -> [u16; 4] {
    if in_play(body) && !body.dead {
        let (x, y) = position(body);
        [u16::from(level(body)), x, y, u16::from(!body.facing_right)]
    } else {
        [HIDDEN, 0, 0, 0]
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    goal: bool,
    on_ground: bool,
    loading: bool,
    cleared: u8,
    climb: u16,
}

fn view(body: Body) -> View {
    let (x, y) = position(body);
    let start = STAGES[usize::from(body.stage.min(STAGE_COUNT - 1))].0;
    View {
        x,
        y,
        health: if body.dead { 0 } else { body.health },
        level: level(body),
        goal: body.goal,
        on_ground: body.standing,
        loading: !in_play(body),
        cleared: cleared(body),
        climb: u16::from(body.screen.saturating_sub(start)) * 256 + u16::from(body.x),
    }
}

fn state(memory: &[u8]) -> Result<Mm2MechanicalState, Box<dyn Error>> {
    Ok(decode_state(
        memory
            .get(..WRAM_SIZE)
            .ok_or("Mega Man 2 needs 2 KiB of work RAM")?,
    )?)
}

impl<M: NesBackend<Vec<u8>> + NesMemory + 'static> ArcadeGame for Mm2Game<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::whole_game(
            rom,
            identity,
            identity_sha256,
            Vec::new(),
            Box::new(move || machines()),
        )
    }

    fn run() -> Mm2CampaignRun {
        Mm2CampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        state(wram).map_or([HIDDEN, 0, 0, 0], |state| {
            motion(Body {
                facing_right: facing_right(wram),
                ..Body::from(state)
            })
        })
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        state(memory).map_or(0, |state| cleared(state.into()))
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(state(memory)?.into()))
    }

    fn view(observation: &Mm2Observations) -> View {
        view(observation.decoded.into())
    }

    fn frame_count(observation: &Mm2Observations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &Mm2Observations) -> u8 {
        cleared(observation.decoded.into())
    }

    fn snapshot_frame_count(snapshot: &Mm2Snapshot) -> u64 {
        snapshot.frame_count()
    }

    fn emulator_state(snapshot: &Mm2Snapshot) -> &[u8] {
        snapshot.emulator_state()
    }
}

impl Arcade for MegaMan2 {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &Mm2State) -> View {
        view(state.into())
    }

    fn motion(state: &Mm2State) -> [u16; 4] {
        motion(state.into())
    }

    fn cleared(state: &Mm2State) -> u8 {
        cleared(state.into())
    }
}
