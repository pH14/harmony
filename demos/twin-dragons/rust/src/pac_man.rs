// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    nes_backend::pack_quicknes_state,
    pac_man::{
        campaign::PacManGame,
        target::{PacManSnapshot, PacManTarget},
    },
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type PacMan = PacManGame<Traced<JsMachine>>;

impl BrowserGame for PacMan {
    fn root_snapshot(
        target: &mut PacManTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<PacManSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().playing() {
            return Err("Pac-Man cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn pac_man_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&PacMan::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
