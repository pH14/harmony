// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use nes_games::{
    metroid::{
        campaign::{MetroidCampaignRun, MetroidGame},
        target::{
            BCD_BORROW_HEALTH, GenesisDepth, MetroidMechanicalState, MetroidObservations,
            MetroidSnapshot, MetroidTarget, decode_state, faces_left, power_on_walk,
        },
    },
    nes_backend::pack_quicknes_state,
    search::draw::DrawMixture,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::campaign::{BrowserGame, HIDDEN, JsMachine, js_error};

pub type Metroid = MetroidGame<JsMachine>;

const MEMORY_BYTES: usize = WRAM_SIZE + 0x2000;
const SCREEN_WIDTH: i32 = 256;
const SCREEN_HEIGHT: i32 = 240;
const AREAS: [(i32, i32, i32, i32); 5] = [
    (1, 1, 30, 19),
    (13, 9, 18, 16),
    (1, 16, 12, 15),
    (14, 23, 17, 8),
    (1, 3, 10, 9),
];
const AREA_OF: [u8; 5] = [0, 1, 2, 4, 3];
const DOOR_IDLE: u8 = 0;
const DOOR_SCROLLED: u8 = 5;
const POSE_LANDED: u8 = 1;
const POSE_UNKNOWN: u8 = 0xff;
const MAX_TANKS: u8 = 6;

fn level(state: MetroidMechanicalState) -> u8 {
    AREA_OF
        .get(usize::from(state.area & 15))
        .copied()
        .unwrap_or(0)
}

fn position(state: MetroidMechanicalState) -> (u16, u16) {
    let (x0, y0, cols, rows) = AREAS[usize::from(level(state))];
    let x = ((i32::from(state.map_x) - x0) * SCREEN_WIDTH + i32::from(state.x))
        .clamp(0, cols * SCREEN_WIDTH - 1);
    let y = ((i32::from(state.map_y) - y0) * SCREEN_HEIGHT + i32::from(state.y))
        .clamp(0, rows * SCREEN_HEIGHT - 1);
    (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0))
}

fn dead(state: MetroidMechanicalState) -> bool {
    state.in_play() && (state.health == 0 || state.health >= BCD_BORROW_HEALTH)
}

fn loading(state: MetroidMechanicalState) -> bool {
    !state.in_play() || (state.door != DOOR_IDLE && state.door != DOOR_SCROLLED)
}

fn cleared(state: MetroidMechanicalState) -> u8 {
    u8::try_from(state.equipment.count_ones()).unwrap_or(0)
        + state.energy_tanks.min(MAX_TANKS)
        + state.bosses
        + u8::from(state.ending)
}

#[derive(Clone, Copy, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    goal: bool,
    on_ground: bool,
    loading: bool,
    cleared: u8,
}

fn view(state: MetroidMechanicalState) -> View {
    let (x, y) = position(state);
    View {
        x,
        y,
        health: if dead(state) {
            0
        } else {
            u8::try_from(state.health.div_ceil(10)).unwrap_or(u8::MAX)
        },
        level: level(state),
        goal: state.ending,
        on_ground: state.pose <= POSE_LANDED || state.pose == POSE_UNKNOWN,
        loading: loading(state),
        cleared: cleared(state),
    }
}

fn state(memory: &[u8]) -> Result<MetroidMechanicalState, Box<dyn Error>> {
    if memory.len() != MEMORY_BYTES {
        return Err("Metroid needs 2 KiB of work RAM and 8 KiB of cartridge RAM".into());
    }
    let (wram, cartridge) = memory.split_at(WRAM_SIZE);
    Ok(decode_state(wram, cartridge)?)
}

impl BrowserGame for Metroid {
    type View = View;

    const MEMORY_BYTES: usize = MEMORY_BYTES;

    fn create(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Self {
        Self::with_machines(
            rom,
            identity,
            identity_sha256,
            power_on_walk(),
            GenesisDepth::NewGame,
            Box::new(move || JsMachine::new::<Self>(core)),
        )
    }

    fn run() -> MetroidCampaignRun {
        MetroidCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(memory: &[u8]) -> [u16; 4] {
        match state(memory) {
            Ok(state) if !loading(state) && !dead(state) => {
                let (x, y) = position(state);
                [u16::from(level(state)), x, y, u16::from(faces_left(memory))]
            }
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        state(memory).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(state(memory)?))
    }

    fn view(observation: &MetroidObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &MetroidObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &MetroidObservations) -> u8 {
        cleared(observation.decoded)
    }

    fn emulator_state(snapshot: &MetroidSnapshot) -> &[u8] {
        snapshot.emulator_state()
    }

    fn root_snapshot(
        target: &mut MetroidTarget<JsMachine>,
        _core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<MetroidSnapshot, Box<dyn Error>> {
        let snapshot = target.snapshot_of_state(&pack_quicknes_state(raw)?, frames)?;
        if !snapshot.state().in_play() || target.is_dead() {
            return Err("Metroid cannot play on from this state".into());
        }
        Ok(snapshot)
    }
}

#[wasm_bindgen]
pub fn metroid_observation(memory: &[u8]) -> Result<String, JsValue> {
    serde_json::to_string(&view(state(memory).map_err(js_error)?)).map_err(js_error)
}
