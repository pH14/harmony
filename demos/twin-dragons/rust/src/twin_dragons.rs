// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    nes_backend::pack_quicknes_state,
    search::draw::DrawMixture,
    twin_dragons::{
        campaign::{TwinDragonsCampaignRun, TwinDragonsGame},
        target::{
            TwinDragonsMechanicalState, TwinDragonsObservations, TwinDragonsSnapshot,
            TwinDragonsTarget, decode_state,
        },
    },
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, HIDDEN, JsMachine, js_error, restore_ram};

pub type TwinDragons = TwinDragonsGame<JsMachine>;

const ANIMATION: usize = 0x71;
const ANIMATION_FRAME: usize = 0x72;
const FACING: usize = 0x74;

fn pose(wram: &[u8]) -> u16 {
    let pose = match (wram[ANIMATION], wram[ANIMATION_FRAME]) {
        (1, frame) => 1 + u16::from(frame.min(3)),
        (2, _) => 5,
        (3, _) => 6,
        _ => 0,
    };
    pose * 2 + u16::from(wram[FACING] != 0)
}

#[derive(Clone, Copy, Serialize)]
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

impl BrowserGame for TwinDragons {
    type View = View;

    fn create(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                TwinDragonsTarget::from_power_on(
                    JsMachine::new::<Self>(core).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> TwinDragonsCampaignRun {
        TwinDragonsCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::AlphabetOnly
    }

    fn motion(memory: &[u8]) -> [u16; 4] {
        match decode_state(memory) {
            Ok(state) => match state.gameplay {
                Some(gameplay) => [
                    u16::from(state.level),
                    gameplay.x.max(0) as u16,
                    gameplay.y.max(0) as u16,
                    pose(memory),
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

    fn root_snapshot(
        _target: &mut TwinDragonsTarget<JsMachine>,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<TwinDragonsSnapshot, Box<dyn Error>> {
        let decoded = decode_state(&restore_ram(core, raw)?)?;
        Ok(TwinDragonsSnapshot {
            emulator_state: pack_quicknes_state(raw)?,
            observation: TwinDragonsObservations {
                frame_count: frames,
                decoded,
                terminal: decoded.terminal(),
            },
            failed: false,
        })
    }
}

#[wasm_bindgen]
pub fn twin_dragons_observation(ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Twin Dragons observations need 2 KiB of work RAM"));
    }
    serde_json::to_string(&view(decode_state(ram).map_err(js_error)?)).map_err(js_error)
}

#[wasm_bindgen]
pub fn twin_dragons_pose(ram: &[u8]) -> Result<u16, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Twin Dragons poses need 2 KiB of work RAM"));
    }
    Ok(pose(ram))
}
