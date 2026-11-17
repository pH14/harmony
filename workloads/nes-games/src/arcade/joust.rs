// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    joust::{Joust, JoustState},
    search::draw::DrawMixture,
    workload::Arcade,
};

const LEVEL_COUNT: u8 = 49;
const SCREEN_TOP_CROP: u8 = 7;

fn level(state: JoustState) -> u8 {
    state.waves_cleared().min(LEVEL_COUNT - 1)
}

fn screen_y(state: JoustState) -> u16 {
    u16::from(state.y.saturating_sub(SCREEN_TOP_CROP))
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
    wave: u8,
    lives: u8,
    score: u32,
    defeated: u8,
    enemies: u8,
    riders: u8,
    eggs: u8,
    left: bool,
    velocity_x: i8,
    velocity_y: i8,
}

impl Arcade for Joust {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &JoustState) -> View {
        let state = *state;
        View {
            x: u16::from(state.x),
            y: screen_y(state),
            health: if state.playing() {
                state.lives.saturating_add(1)
            } else {
                0
            },
            level: level(state),
            goal: state.victory(),
            loading: !state.in_game || state.wave == 0,
            cleared: state.waves_cleared(),
            wave: state.wave,
            lives: state.lives,
            score: state.score,
            defeated: state.defeated,
            enemies: state.enemies,
            riders: state.riders,
            eggs: state.eggs,
            left: state.facing_left,
            velocity_x: state.velocity_x,
            velocity_y: state.velocity_y,
        }
    }

    fn motion(state: &JoustState) -> [u16; 4] {
        if state.playing() {
            [
                u16::from(level(*state)),
                u16::from(state.x),
                screen_y(*state),
                u16::from(state.facing_left),
            ]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &JoustState) -> u8 {
        state.waves_cleared()
    }
}
