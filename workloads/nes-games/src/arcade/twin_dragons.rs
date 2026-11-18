// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    search::draw::DrawMixture,
    twin_dragons::game::{TwinDragons, TwinDragonsState},
    workload::Arcade,
};

pub fn pose(state: TwinDragonsState) -> u16 {
    let pose = match (state.animation, state.animation_frame) {
        (1, frame) => 1 + u16::from(frame.min(3)),
        (2, _) => 5,
        (3, _) => 6,
        _ => 0,
    };
    pose * 2 + u16::from(state.facing != 0)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: i16,
    y: i16,
    health: u8,
    level: u8,
    next_level: u8,
    gamestate: u8,
    goal: bool,
    on_ground: bool,
    facing: u8,
    loading: bool,
    cleared: u8,
}

impl Arcade for TwinDragons {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &TwinDragonsState) -> View {
        let state = *state;
        View {
            x: state.x,
            y: i16::from(state.y),
            health: state.health,
            level: state.level,
            next_level: state.next_level,
            gamestate: state.game_state,
            goal: state.at_goal,
            on_ground: state.on_ground,
            facing: state.facing,
            loading: !state.playing() || !state.loaded,
            cleared: state.levels_cleared(),
        }
    }

    fn motion(state: &TwinDragonsState) -> [u16; 4] {
        let state = *state;
        if state.in_play {
            [
                u16::from(state.level),
                u16::try_from(state.x.max(0)).unwrap_or(0),
                u16::from(state.y),
                pose(state),
            ]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &TwinDragonsState) -> u8 {
        state.levels_cleared()
    }
}
