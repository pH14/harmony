// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    castlevania::{
        campaign::{CastlevaniaCampaignRun, CastlevaniaGame},
        progress::stages_cleared,
        target::{
            CastlevaniaMechanicalState, CastlevaniaObservations, CastlevaniaSnapshot,
            CastlevaniaTarget, FINAL_STAGE, decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const MAP_ROW_OFFSET: u16 = 40;
const FACING: usize = 0x15;

const ENTRY: [(u16, u8); 18] = [
    (0, 0),
    (0, 1),
    (0, 0),
    (256, 0),
    (512, 0),
    (512, 0),
    (512, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (1024, 0),
    (768, 0),
    (1280, 0),
    (512, 0),
    (512, 0),
];

fn map_x(state: CastlevaniaMechanicalState) -> Option<u16> {
    if !(1..=FINAL_STAGE).contains(&state.stage) {
        return None;
    }
    let (origin, section) = ENTRY[usize::from(state.stage - 1)];
    (state.section == section && state.x >= origin).then(|| state.x - origin)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    level: u8,
    stage: u8,
    section: u8,
    mode: u8,
    x: u16,
    y: u16,
    health: u8,
    lives: u8,
    hearts: u8,
    whip: u8,
    subweapon: u8,
    boss_health: u8,
    cleared: u8,
}

fn view(state: CastlevaniaMechanicalState) -> View {
    let x = map_x(state);
    View {
        level: x.map_or(u8::MAX, |_| state.stage - 1),
        stage: state.stage,
        section: state.section,
        mode: state.mode,
        x: x.unwrap_or(state.x),
        y: u16::from(state.y).saturating_sub(MAP_ROW_OFFSET),
        health: state.health,
        lives: state.lives,
        hearts: state.hearts,
        whip: state.whip,
        subweapon: state.subweapon,
        boss_health: state.boss_health,
        cleared: stages_cleared(state),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for CastlevaniaGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                CastlevaniaTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> CastlevaniaCampaignRun {
        CastlevaniaCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.playing() => match map_x(state) {
                Some(x) => [
                    u16::from(state.stage - 1),
                    x,
                    u16::from(state.y).saturating_sub(MAP_ROW_OFFSET),
                    u16::from(wram[FACING] & 1 == 0),
                ],
                None => [HIDDEN, 0, 0, 0],
            },
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, stages_cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &CastlevaniaObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &CastlevaniaObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &CastlevaniaObservations) -> u8 {
        stages_cleared(observation.decoded)
    }

    fn snapshot_frame_count(snapshot: &CastlevaniaSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &CastlevaniaSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
