// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, smb, trail::Traced},
    nes_backend::pack_quicknes_state,
    smb::{
        campaign::SmbGame,
        target::{SmbSnapshot, SmbTarget},
    },
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Smb = SmbGame<Traced<JsMachine>>;

impl BrowserGame for Smb {
    fn root_snapshot(
        target: &mut SmbTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<SmbSnapshot, Box<dyn Error>> {
        target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)
    }
}

#[wasm_bindgen]
pub fn smb_observation(ram: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Smb::memory_view(ram).map_err(js_error)?).map_err(js_error)
}

#[wasm_bindgen]
pub fn smb_main_area(level: u8) -> Result<u8, JsValue> {
    smb::main_area(level).ok_or_else(|| js_error("unknown Super Mario Bros. level"))
}
