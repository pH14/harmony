// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    nes_backend::pack_quicknes_state,
    workload::{NesSnapshot, NesTarget, NesWorkload},
    zelda::game::{self as game, MODE_PLAY, ZeldaState},
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type Zelda = NesWorkload<game::Zelda, Traced<JsMachine>>;

impl BrowserGame for Zelda {
    fn root_snapshot(
        target: &mut NesTarget<game::Zelda, Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<NesSnapshot<ZeldaState>, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if snapshot.observation.state.mode != MODE_PLAY {
            return Err("Zelda cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn zelda_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&Zelda::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
