// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    joust::{
        campaign::{JoustCampaignRun, JoustGame},
        target::{
            JoustMechanicalState, JoustObservations, JoustSnapshot, JoustTarget, decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const LEVEL_COUNT: u8 = 49;
const SCREEN_TOP_CROP: u8 = 7;

fn level(state: JoustMechanicalState) -> u8 {
    state.waves_cleared().min(LEVEL_COUNT - 1)
}

fn screen_y(state: JoustMechanicalState) -> u16 {
    u16::from(state.y.saturating_sub(SCREEN_TOP_CROP))
}

fn loading(state: JoustMechanicalState) -> bool {
    !state.in_game || state.wave == 0
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
    wave: u8,
    lives: u8,
    score: u32,
    defeated: u8,
    enemies: u8,
    riders: u8,
    eggs: u8,
    left: bool,
    velocity_x: i8,
    velocity_y: i8,
}

fn view(state: JoustMechanicalState) -> View {
    View {
        x: u16::from(state.x),
        y: screen_y(state),
        health: if state.playing() {
            state.lives.saturating_add(1)
        } else {
            0
        },
        level: level(state),
        goal: state.victory(),
        loading: loading(state),
        cleared: state.waves_cleared(),
        wave: state.wave,
        lives: state.lives,
        score: state.score,
        defeated: state.defeated,
        enemies: state.enemies,
        riders: state.riders,
        eggs: state.eggs,
        left: state.facing_left,
        velocity_x: state.velocity_x,
        velocity_y: state.velocity_y,
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for JoustGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                JoustTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> JoustCampaignRun {
        JoustCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.playing() => [
                u16::from(level(state)),
                u16::from(state.x),
                screen_y(state),
                u16::from(state.facing_left),
            ],
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, JoustMechanicalState::waves_cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &JoustObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &JoustObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &JoustObservations) -> u8 {
        observation.decoded.waves_cleared()
    }

    fn snapshot_frame_count(snapshot: &JoustSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &JoustSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
