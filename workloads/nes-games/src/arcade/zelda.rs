// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    search::draw::DrawMixture,
    workload::Arcade,
    zelda::game::{MODE_PLAY, Zelda, ZeldaState},
};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    mode: u8,
    level: u8,
    room: u8,
    x: u16,
    y: u16,
    health: u16,
    containers: u8,
    rupees: u8,
    sword: u8,
    items: u16,
    triforce: u8,
    cleared: u8,
}

const CARTRIDGE_BYTES: usize = 0x2000;
const SCREEN_WIDTH: i32 = 256;
const SCREEN_HEIGHT: i32 = 168;
const PLAY_TOP: i32 = 53;

fn position(state: ZeldaState) -> (u16, u16) {
    let column = i32::from(state.room & 15);
    let row = i32::from(state.room >> 4);
    let x = column * SCREEN_WIDTH + i32::from(state.x);
    let y = row * SCREEN_HEIGHT + (i32::from(state.y) - PLAY_TOP).clamp(0, SCREEN_HEIGHT - 1);
    (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0))
}

impl Arcade for Zelda {
    type View = View;
    const MEMORY_BYTES: usize = WRAM_SIZE + CARTRIDGE_BYTES;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &ZeldaState) -> View {
        let state = *state;
        let (x, y) = position(state);
        View {
            mode: state.mode,
            level: state.level,
            room: state.room,
            x,
            y,
            health: state.health(),
            containers: state.containers(),
            rupees: state.rupees,
            sword: state.sword,
            items: state.items,
            triforce: state.triforce,
            cleared: state.triforce_pieces(),
        }
    }

    fn motion(state: &ZeldaState) -> [u16; 4] {
        if state.mode == MODE_PLAY {
            let (x, y) = position(*state);
            [u16::from(state.level), x, y, u16::from(state.facing_left)]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &ZeldaState) -> u8 {
        state.triforce_pieces()
    }
}
