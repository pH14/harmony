// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::NesBackend,
    pac_man::{
        campaign::{PacManCampaignRun, PacManGame},
        target::{
            DOTS, PacManMechanicalState, PacManObservations, PacManSnapshot, PacManTarget,
            decode_state,
        },
    },
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    mode: u8,
    level: u8,
    x: u16,
    y: u16,
    health: u8,
    loading: bool,
    dots: u8,
    percent: u8,
    power: u16,
    score: u32,
    direction: u8,
    cleared: u8,
}

fn view(state: PacManMechanicalState) -> View {
    let (x, y) = state.screen();
    View {
        mode: state.mode,
        level: state.level_group(),
        x,
        y,
        health: state.lives,
        loading: !state.on_maze(),
        dots: state.dots,
        percent: u8::try_from(u16::from(state.dots_eaten()) * 100 / u16::from(DOTS)).unwrap_or(100),
        power: state.power_frames,
        score: state.score,
        direction: state.direction,
        cleared: state.mazes_cleared(),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for PacManGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                PacManTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> PacManCampaignRun {
        PacManCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.in_game() => {
                let (x, y) = state.screen();
                [u16::from(state.level_group()), x, y, state.pose()]
            }
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, PacManMechanicalState::mazes_cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &PacManObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &PacManObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &PacManObservations) -> u8 {
        observation.decoded.mazes_cleared()
    }

    fn snapshot_frame_count(snapshot: &PacManSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &PacManSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
