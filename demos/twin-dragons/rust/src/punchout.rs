// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    nes_backend::pack_quicknes_state,
    punchout::{self as game, PunchOutState},
    workload::{NesSnapshot, NesTarget, NesWorkload},
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type PunchOut = NesWorkload<game::PunchOut, Traced<JsMachine>>;

impl BrowserGame for PunchOut {
    fn root_snapshot(
        target: &mut NesTarget<game::PunchOut, Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<NesSnapshot<PunchOutState>, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        let state = snapshot.observation.state;
        if !state.fighting() || state.terminal() {
            return Err("Punch-Out!! cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn punchout_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&PunchOut::memory_view(memory).map_err(js_error)?).map_err(js_error)
}
