// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    castlevania::{
        archive::bosses_down,
        campaign::{CastlevaniaCampaignRun, CastlevaniaGame},
        target::{
            CastlevaniaMechanicalState, CastlevaniaObservations, CastlevaniaSnapshot,
            CastlevaniaTarget, decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    stage: u8,
    section: u8,
    mode: u8,
    x: u16,
    y: u8,
    health: u8,
    lives: u8,
    hearts: u8,
    whip: u8,
    subweapon: u8,
    boss_health: u8,
    cleared: u8,
}

fn view(state: CastlevaniaMechanicalState) -> View {
    View {
        stage: state.stage,
        section: state.section,
        mode: state.mode,
        x: state.x,
        y: state.y,
        health: state.health,
        lives: state.lives,
        hearts: state.hearts,
        whip: state.whip,
        subweapon: state.subweapon,
        boss_health: state.boss_health,
        cleared: bosses_down(state),
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
            Ok(state) if state.playing() => [
                u16::from(state.stage) << 8 | u16::from(state.section),
                state.x,
                u16::from(state.y),
                0,
            ],
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, bosses_down)
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
        bosses_down(observation.decoded)
    }

    fn emulator_state(snapshot: &CastlevaniaSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
