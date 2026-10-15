// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    famidash::{
        campaign::{FamidashCampaignRun, FamidashGame},
        target::{
            FamidashMechanicalState, FamidashObservations, FamidashSnapshot, FamidashTarget,
            decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const MAP_TOPS: [i32; 22] = [
    662, 664, 679, 630, 599, 615, 710, 584, 643, 678, 480, 250, 616, 272, 661, 422, 658, 604, 558,
    622, 583, 342,
];

fn pose(state: FamidashMechanicalState) -> u16 {
    state.gameplay.map_or(0, |gameplay| {
        u16::from(gameplay.gamemode.min(7)) * 2 + u16::from(gameplay.gravity)
    })
}

fn map_point(level: u8, x: i32, y: i32) -> (u16, u16) {
    (
        u16::try_from(x.max(0)).unwrap_or(u16::MAX),
        u16::try_from((y - map_top(level)).max(0)).unwrap_or(u16::MAX),
    )
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    gamestate: u8,
    percent: u8,
    gamemode: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
}

fn cleared(state: FamidashMechanicalState) -> u8 {
    state.level + u8::from(state.complete())
}

fn view(state: FamidashMechanicalState) -> View {
    let gameplay = state.gameplay.unwrap_or_default();
    let (x, y) = map_point(state.level, gameplay.x, gameplay.y);
    View {
        x,
        y,
        health: u8::from(state.gameplay.is_some() || state.complete()),
        level: state.level,
        gamestate: state.game_state,
        percent: state.percent(),
        gamemode: gameplay.gamemode,
        goal: state.finished(),
        loading: state.gameplay.is_none(),
        cleared: cleared(state),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for FamidashGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            0,
            Box::new(move || {
                FamidashTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> FamidashCampaignRun {
        FamidashCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::AlphabetOnly
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) => match state.gameplay {
                Some(gameplay) => {
                    let (x, y) = map_point(state.level, gameplay.x, gameplay.y);
                    [u16::from(state.level), x, y, pose(state)]
                }
                None => [HIDDEN, 0, 0, 0],
            },
            Err(_) => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &FamidashObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &FamidashObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &FamidashObservations) -> u8 {
        cleared(observation.decoded)
    }

    fn emulator_state(snapshot: &FamidashSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}

#[must_use]
pub fn map_top(level: u8) -> i32 {
    MAP_TOPS.get(usize::from(level)).copied().unwrap_or(0)
}
