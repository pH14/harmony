// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    nes_backend::pack_quicknes_state,
    punchout::{
        campaign::PunchOutGame,
        target::{PunchOutSnapshot, PunchOutTarget},
    },
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type PunchOut = PunchOutGame<Traced<JsMachine>>;

impl BrowserGame for PunchOut {
    fn root_snapshot(
        target: &mut PunchOutTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<PunchOutSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().fighting() || snapshot.state().terminal() {
            return Err("Punch-Out!! cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn punchout_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&PunchOut::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
