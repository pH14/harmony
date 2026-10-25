// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    joust::{
        campaign::JoustGame,
        target::{JoustSnapshot, JoustTarget},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Joust = JoustGame<Traced<JsMachine>>;

impl BrowserGame for Joust {
    fn root_snapshot(
        target: &mut JoustTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<JoustSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().playing() {
            return Err("Joust cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn joust_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Joust::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
