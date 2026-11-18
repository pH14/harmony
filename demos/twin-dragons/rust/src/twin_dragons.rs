// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{ArcadeGame, trail::Traced, twin_dragons::pose},
    nes_backend::pack_quicknes_state,
    twin_dragons::{self as game, TwinDragonsState},
    workload::{Game as _, NesSnapshot, NesTarget, NesWorkload},
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type TwinDragons = NesWorkload<game::TwinDragons, Traced<JsMachine>>;

impl BrowserGame for TwinDragons {
    fn root_snapshot(
        target: &mut NesTarget<game::TwinDragons, Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<NesSnapshot<TwinDragonsState>, Box<dyn Error>> {
        target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)
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
    Ok(pose(game::TwinDragons.decode(None, ram)))
}
