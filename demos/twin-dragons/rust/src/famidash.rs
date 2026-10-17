// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    arcade::{
        famidash::{self as arcade, group_stage},
        feed::{Action, actions},
        trail::Traced,
    },
    famidash::{
        campaign::FamidashGame,
        levels::GROUPS,
        target::{
            FamidashObservations, FamidashSnapshot, FamidashTarget, decode_state,
            level_select_tape, start_level_tape,
        },
    },
    nes_backend::pack_quicknes_state,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, JsMachine, js_error, restore_ram};

pub type Famidash<const GROUP: usize> = FamidashGame<Traced<JsMachine>, GROUP>;

impl<const GROUP: usize> BrowserGame for Famidash<GROUP> {
    fn root_snapshot(
        target: &mut FamidashTarget<Traced<JsMachine>>,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<FamidashSnapshot, Box<dyn Error>> {
        let decoded = decode_state(&restore_ram(core, raw)?, target.stage())?;
        Ok(FamidashSnapshot {
            emulator_state: pack_quicknes_state(raw)?,
            observation: FamidashObservations {
                frame_count: frames,
                decoded,
                terminal: decoded.terminal(),
            },
            failed: false,
        })
    }
}

#[derive(Serialize)]
struct Level {
    id: u8,
    name: &'static str,
    top: i32,
}

#[derive(Serialize)]
struct Group {
    game: String,
    id: &'static str,
    name: &'static str,
    side: &'static str,
    rom: &'static str,
    rom_sha256: &'static str,
    level_count: u8,
    levels: Vec<Level>,
}

#[must_use]
pub fn game_id(group: usize) -> String {
    match GROUPS.get(group).map(|group| group.id) {
        Some("main") | None => "famidash".to_owned(),
        Some(id) => format!("famidash-{id}"),
    }
}

fn group(game: &str) -> Result<usize, JsValue> {
    (0..GROUPS.len())
        .find(|&group| game_id(group) == game)
        .ok_or_else(|| js_error(format!("{game} is not a Famidash level group")))
}

#[wasm_bindgen]
pub fn famidash_groups() -> Result<String, JsValue> {
    let groups: Vec<Group> = GROUPS
        .iter()
        .enumerate()
        .map(|(index, group)| Group {
            game: game_id(index),
            id: group.id,
            name: group.name,
            side: group.side.id(),
            rom: group.side.rom_file(),
            rom_sha256: group.side.rom_sha256(),
            level_count: group.side.level_count(),
            levels: (group.first..=group.last)
                .map(|id| Level {
                    id,
                    name: group.side.level_name(id).unwrap_or_default(),
                    top: arcade::map_top(group.side, id),
                })
                .collect(),
        })
        .collect();
    serde_json::to_string(&groups).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_observation(game: &str, ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Famidash observations need 2 KiB of work RAM"));
    }
    let state = decode_state(ram, group_stage(group(game)?)).map_err(js_error)?;
    serde_json::to_string(&arcade::view(state)).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_level_select_tape(game: &str) -> Result<String, JsValue> {
    let menu = GROUPS[group(game)?].menu;
    let tape: Vec<Action> = actions(&level_select_tape(menu));
    serde_json::to_string(&tape).map_err(js_error)
}

#[wasm_bindgen]
pub fn famidash_start_level_tape() -> Result<String, JsValue> {
    let tape: Vec<Action> = actions(&start_level_tape());
    serde_json::to_string(&tape).map_err(js_error)
}
