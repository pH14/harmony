// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    pac_man::game::{DOTS, PacMan, PacManState},
    search::draw::DrawMixture,
    workload::Arcade,
};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    mode: u8,
    level: u8,
    x: u16,
    y: u16,
    health: u8,
    loading: bool,
    dots: u8,
    percent: u8,
    power: u16,
    score: u32,
    direction: u8,
    cleared: u8,
}

impl Arcade for PacMan {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &PacManState) -> View {
        let state = *state;
        let (x, y) = state.screen();
        View {
            mode: state.mode,
            level: state.level_group(),
            x,
            y,
            health: state.lives,
            loading: !state.on_maze(),
            dots: state.dots,
            percent: u8::try_from(u16::from(state.dots_eaten()) * 100 / u16::from(DOTS))
                .unwrap_or(100),
            power: state.power_frames,
            score: state.score,
            direction: state.direction,
            cleared: state.mazes_cleared(),
        }
    }

    fn motion(state: &PacManState) -> [u16; 4] {
        let state = *state;
        if state.in_game() {
            let (x, y) = state.screen();
            [u16::from(state.level_group()), x, y, state.pose()]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &PacManState) -> u8 {
        state.mazes_cleared()
    }
}
