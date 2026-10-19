// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::NesBackend,
    search::draw::DrawMixture,
    twin_dragons::{
        campaign::{TwinDragonsCampaignRun, TwinDragonsGame},
        target::{
            TwinDragonsMechanicalState, TwinDragonsObservations, TwinDragonsSnapshot,
            TwinDragonsTarget, decode_state,
        },
    },
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const ANIMATION: usize = 0x71;
const ANIMATION_FRAME: usize = 0x72;
const FACING: usize = 0x74;

#[must_use]
pub fn pose(wram: &[u8]) -> u16 {
    let pose = match (wram[ANIMATION], wram[ANIMATION_FRAME]) {
        (1, frame) => 1 + u16::from(frame.min(3)),
        (2, _) => 5,
        (3, _) => 6,
        _ => 0,
    };
    pose * 2 + u16::from(wram[FACING] != 0)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: i16,
    y: i16,
    health: u8,
    level: u8,
    next_level: u8,
    gamestate: u8,
    goal: bool,
    on_ground: bool,
    facing: u8,
    loading: bool,
    cleared: u8,
}

fn view(state: TwinDragonsMechanicalState) -> View {
    let gameplay = state.gameplay.unwrap_or_default();
    View {
        x: gameplay.x,
        y: gameplay.y,
        health: gameplay.health,
        level: state.level,
        next_level: state.next_level,
        gamestate: state.game_state,
        goal: state.goal,
        on_ground: gameplay.on_ground,
        facing: gameplay.facing,
        loading: !state.playing() || !state.loaded,
        cleared: state.levels_cleared(),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for TwinDragonsGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                TwinDragonsTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> TwinDragonsCampaignRun {
        TwinDragonsCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) => match state.gameplay {
                Some(gameplay) => [
                    u16::from(state.level),
                    gameplay.x.max(0) as u16,
                    gameplay.y.max(0) as u16,
                    pose(wram),
                ],
                None => [HIDDEN, 0, 0, 0],
            },
            Err(_) => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, |state| state.levels_cleared())
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &TwinDragonsObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &TwinDragonsObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &TwinDragonsObservations) -> u8 {
        observation.decoded.levels_cleared()
    }

    fn emulator_state(snapshot: &TwinDragonsSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
