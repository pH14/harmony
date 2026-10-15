// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{ArcadeGame, trail::Traced, twin_dragons::pose},
    nes_backend::pack_quicknes_state,
    twin_dragons::{
        campaign::TwinDragonsGame,
        target::{TwinDragonsObservations, TwinDragonsSnapshot, TwinDragonsTarget, decode_state},
    },
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error, restore_ram};

pub type TwinDragons = TwinDragonsGame<Traced<JsMachine>>;

impl BrowserGame for TwinDragons {
    fn root_snapshot(
        _target: &mut TwinDragonsTarget<Traced<JsMachine>>,
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
    serde_json::to_string(&TwinDragons::memory_view(ram).map_err(js_error)?).map_err(js_error)
}

#[wasm_bindgen]
pub fn twin_dragons_pose(ram: &[u8]) -> Result<u16, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Twin Dragons poses need 2 KiB of work RAM"));
    }
    Ok(pose(ram))
}
