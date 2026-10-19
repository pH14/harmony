// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use nes_games::{
    arcade::{ArcadeGame, trail::Traced},
    mm2::{
        campaign::{Mm2Game, admission_is_viable},
        target::{Mm2Snapshot, Mm2Target},
    },
    nes_backend::pack_quicknes_state,
};
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error};

pub type MegaMan = Mm2Game<Traced<JsMachine>>;

impl BrowserGame for MegaMan {
    fn root_snapshot(
        target: &mut Mm2Target<Traced<JsMachine>>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<Mm2Snapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().playable() || !admission_is_viable(target, &snapshot)? {
            return Err("Mega Man 2 cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn mega_man_observation(ram: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&MegaMan::memory_view(ram).map_err(js_error)?).map_err(js_error)
}
