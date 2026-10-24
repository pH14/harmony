// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    nes_backend::pack_quicknes_state,
    zelda::{
        campaign::ZeldaGame,
        target::{MODE_PLAY, ZeldaSnapshot, ZeldaTarget},
    },
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Zelda = ZeldaGame<Traced<JsMachine>>;

impl BrowserGame for Zelda {
    fn root_snapshot(
        target: &mut ZeldaTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<ZeldaSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if snapshot.state().mode != MODE_PLAY {
            return Err("Zelda cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn zelda_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Zelda::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
