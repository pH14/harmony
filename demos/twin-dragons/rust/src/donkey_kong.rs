// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    donkey_kong::{
        campaign::DonkeyKongGame,
        target::{DonkeyKongSnapshot, DonkeyKongTarget},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type DonkeyKong = DonkeyKongGame<Traced<JsMachine>>;

impl BrowserGame for DonkeyKong {
    fn root_snapshot(
        target: &mut DonkeyKongTarget<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<DonkeyKongSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().in_play() {
            return Err("Donkey Kong is not in play at this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn donkey_kong_observation(ram: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&DonkeyKong::memory_view(ram).map_err(js_error)?).map_err(js_error)
}
