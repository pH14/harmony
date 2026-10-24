// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{
            GAMEPLAY_BANK, Mm2MechanicalState, Mm2Observations, Mm2Snapshot, PLAYER_STATE_STANDING,
            decode_state, facing_right,
        },
    },
    nes_backend::{NesBackend, NesMemory},
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

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

fn level(state: Mm2MechanicalState) -> u8 {
    ORDER
        .iter()
        .position(|&stage| stage == state.stage)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or(0)
}

fn position(state: Mm2MechanicalState) -> (u16, u16) {
    let (col, row) = cell(state.stage, state.screen);
    let top = row * SCREEN_HEIGHT;
    (
        col * 256 + u16::from(state.x),
        (top + u16::from(state.y.saturating_sub(CROP_TOP))).min(top + SCREEN_HEIGHT - 1),
    )
}

fn dead(state: Mm2MechanicalState) -> bool {
    state.is_dead() || state.is_dying()
}

fn in_play(state: Mm2MechanicalState) -> bool {
    state.stage < STAGE_COUNT && state.playable()
}

fn goal(state: Mm2MechanicalState) -> bool {
    state.bank == GAMEPLAY_BANK && state.stage < STAGE_COUNT && state.stage_cleared()
}

fn cleared(state: Mm2MechanicalState) -> u8 {
    if state.stage == STAGE_COUNT - 1 && goal(state) {
        STAGE_COUNT
    } else {
        level(state) + u8::from(goal(state))
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

fn view(state: Mm2MechanicalState) -> View {
    let (x, y) = position(state);
    let start = STAGES[usize::from(state.stage.min(STAGE_COUNT - 1))].0;
    View {
        x,
        y,
        health: if dead(state) { 0 } else { state.health },
        level: level(state),
        goal: goal(state),
        on_ground: state.player_state == PLAYER_STATE_STANDING,
        loading: !in_play(state),
        cleared: cleared(state),
        climb: u16::from(state.screen.saturating_sub(start)) * 256 + u16::from(state.x),
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
        match state(wram) {
            Ok(state) if in_play(state) && !dead(state) => {
                let (x, y) = position(state);
                [
                    u16::from(level(state)),
                    x,
                    y,
                    u16::from(!facing_right(wram)),
                ]
            }
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        state(memory).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(state(memory)?))
    }

    fn view(observation: &Mm2Observations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &Mm2Observations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &Mm2Observations) -> u8 {
        cleared(observation.decoded)
    }

    fn snapshot_frame_count(snapshot: &Mm2Snapshot) -> u64 {
        snapshot.frame_count()
    }

    fn emulator_state(snapshot: &Mm2Snapshot) -> &[u8] {
        snapshot.emulator_state()
    }
}
