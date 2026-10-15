// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    metroid::{
        campaign::MetroidGame,
        target::{MetroidSnapshot, MetroidTarget},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Metroid = MetroidGame<Traced<JsMachine>>;

impl BrowserGame for Metroid {
    fn root_snapshot(
        target: &mut MetroidTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<MetroidSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().in_play() || target.is_dead() {
            return Err("Metroid cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn metroid_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Metroid::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
