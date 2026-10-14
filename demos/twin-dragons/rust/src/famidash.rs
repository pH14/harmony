// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    famidash::{
        campaign::{FamidashCampaignRun, FamidashGame},
        target::{
            FamidashMechanicalState, FamidashObservations, FamidashSnapshot, FamidashTarget,
            decode_state, level_select_tape,
        },
    },
    nes_backend::pack_quicknes_state,
    search::draw::DrawMixture,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::campaign::{Action, BrowserGame, HIDDEN, JsMachine, actions, js_error, restore_ram};

pub type Famidash = FamidashGame<JsMachine>;

const MAP_TOP: i32 = 240;

fn pose(state: FamidashMechanicalState) -> u16 {
    state.gameplay.map_or(0, |gameplay| {
        u16::from(gameplay.gamemode.min(7)) * 2 + u16::from(gameplay.gravity)
    })
}

fn map_point(x: i32, y: i32) -> (u16, u16) {
    (
        u16::try_from(x.max(0)).unwrap_or(u16::MAX),
        u16::try_from((y - MAP_TOP).max(0)).unwrap_or(u16::MAX),
    )
}

#[derive(Clone, Copy, Serialize)]
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
    let (x, y) = map_point(gameplay.x, gameplay.y);
    View {
        x,
        y,
        health: u8::from(state.gameplay.is_some() || state.complete()),
        level: state.level,
        gamestate: state.game_state,
        percent: state.percent(),
        gamemode: gameplay.gamemode,
        goal: state.complete(),
        loading: state.gameplay.is_none(),
        cleared: cleared(state),
    }
}

impl BrowserGame for Famidash {
    type View = View;

    fn create(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                FamidashTarget::from_power_on(
                    JsMachine::new::<Self>(core).map_err(|error| error.to_string())?,
                )
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

    fn motion(memory: &[u8]) -> [u16; 4] {
        match decode_state(memory) {
            Ok(state) => match state.gameplay {
                Some(gameplay) => {
                    let (x, y) = map_point(gameplay.x, gameplay.y);
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

    fn root_snapshot(
        _target: &mut FamidashTarget<JsMachine>,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<FamidashSnapshot, Box<dyn Error>> {
        let decoded = decode_state(&restore_ram(core, raw)?)?;
        Ok(FamidashSnapshot {
            emulator_state: pack_quicknes_state(raw)?,
            observation: FamidashObservations {
                frame_count: frames,
                decoded,
                terminal: decoded.terminal(),
            },
            failed: false,
        })
    }
}

#[wasm_bindgen]
pub fn famidash_observation(ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Famidash observations need 2 KiB of work RAM"));
    }
    serde_json::to_string(&view(decode_state(ram).map_err(js_error)?)).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_level_select_tape() -> Result<String, JsValue> {
    let tape: Vec<Action> = actions(&level_select_tape());
    serde_json::to_string(&tape).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_map_top() -> i32 {
    MAP_TOP
}
