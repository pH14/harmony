// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    ice_climbers::{
        campaign::IceClimbersGame,
        target::{IceClimbersObservations, IceClimbersSnapshot, IceClimbersTarget, decode_state},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error, restore_ram};

pub type IceClimbers = IceClimbersGame<Traced<JsMachine>>;

impl BrowserGame for IceClimbers {
    fn root_snapshot(
        _target: &mut IceClimbersTarget<Traced<JsMachine>>,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<IceClimbersSnapshot, Box<dyn Error>> {
        let decoded = decode_state(&restore_ram(core, raw)?)?;
        if decoded.terminal() || decoded.in_transition() {
            return Err("Ice Climbers cannot play on from this state".into());
        }
        Ok(IceClimbersSnapshot {
            emulator_state: pack_quicknes_state(raw)?,
            observation: IceClimbersObservations {
                frame_count: frames,
                decoded,
                terminal: decoded.terminal(),
            },
            failed: false,
        })
    }
}

#[wasm_bindgen]
pub fn ice_climbers_observation(ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Ice Climbers observations need 2 KiB of work RAM"));
    }
    serde_json::to_string(&IceClimbers::memory_view(ram).map_err(js_error)?).map_err(js_error)
}
