// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    ice_climbers::{self as game, IceClimbersState},
    nes_backend::pack_quicknes_state,
    workload::{NesSnapshot, NesTarget, NesWorkload},
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type IceClimbers = NesWorkload<game::IceClimbers, Traced<JsMachine>>;

impl BrowserGame for IceClimbers {
    fn root_snapshot(
        target: &mut NesTarget<game::IceClimbers, Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<NesSnapshot<IceClimbersState>, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        let state = snapshot.observation.state;
        if state.terminal() || state.in_transition() {
            return Err("Ice Climbers cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn ice_climbers_observation(ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Ice Climbers observations need 2 KiB of work RAM"));
    }
    serde_json::to_string(&IceClimbers::memory_view(ram).map_err(js_error)?).map_err(js_error)
}
