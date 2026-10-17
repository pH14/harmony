// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{
        ArcadeGame, famidash,
        feed::{Action, actions},
        trail::Traced,
    },
    famidash::{
        campaign::FamidashGame,
        levels::Menu,
        target::{
            FamidashObservations, FamidashSnapshot, FamidashTarget, decode_state, level_select_tape,
        },
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error, restore_ram};

pub type Famidash = FamidashGame<Traced<JsMachine>>;

impl BrowserGame for Famidash {
    fn root_snapshot(
        target: &mut FamidashTarget<Traced<JsMachine>>,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<FamidashSnapshot, Box<dyn Error>> {
        let decoded = decode_state(&restore_ram(core, raw)?, target.stage())?;
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
    serde_json::to_string(&Famidash::memory_view(ram).map_err(js_error)?).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_level_select_tape() -> Result<String, JsValue> {
    let tape: Vec<Action> = actions(&level_select_tape(Menu::MainLevels));
    serde_json::to_string(&tape).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_map_top(level: u8) -> i32 {
    famidash::map_top(level)
}
