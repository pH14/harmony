// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    castlevania::{
        campaign::CastlevaniaGame,
        target::{CastlevaniaSnapshot, CastlevaniaTarget},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Castlevania = CastlevaniaGame<Traced<JsMachine>>;

impl BrowserGame for Castlevania {
    fn root_snapshot(
        target: &mut CastlevaniaTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<CastlevaniaSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().playing() || snapshot.state().terminal() {
            return Err("Castlevania cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn castlevania_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Castlevania::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
