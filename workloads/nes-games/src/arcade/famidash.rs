// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    famidash::{
        game::{Famidash, FamidashState},
        levels::{Side, Stage},
        lucky_draw::{LuckyDraw, LuckyDrawState},
    },
    search::draw::DrawMixture,
    workload::Arcade,
};
use serde::{Deserialize, Serialize};

pub use crate::famidash::levels::group_stage;

use super::HIDDEN;

const MAIN_MAP_TOPS: [i32; 43] = [
    662, 679, 679, 663, 598, 642, 709, 582, 660, 678, 480, 249, 612, 272, 647, 422, 643, 606, 558,
    622, 583, 343, 686, 654, 500, 478, 598, 707, 487, 598, 702, 397, 686, 558, 361, 430, 606, 494,
    542, 433, 603, 480, 663,
];
const B_MAP_TOPS: [i32; 31] = [
    694, 638, 505, 601, 615, 552, 279, 480, 614, 519, 711, 633, 581, 702, 638, 542, 554, 561, 452,
    638, 480, 485, 663, 599, 270, 673, 503, 78, 667, 638, 480,
];
const C_MAP_TOPS: [i32; 25] = [
    518, 456, 692, 512, 62, 446, 599, 535, 409, 643, 276, 574, 574, 670, 663, 670, 399, 599, 480,
    621, 590, 558, 648, 279, 711,
];
const D_MAP_TOPS: [i32; 10] = [574, 480, 583, 670, 99, 480, 289, 679, 483, 574];

fn pose(state: FamidashState) -> u16 {
    state.gameplay.map_or(0, |gameplay| {
        u16::from(gameplay.gamemode.min(7)) * 2 + u16::from(gameplay.gravity)
    })
}

fn map_point(side: Side, level: u8, x: i32, y: i32) -> (u16, u16) {
    (
        u16::try_from(x.max(0)).unwrap_or(u16::MAX),
        u16::try_from((y - map_top(side, level)).max(0)).unwrap_or(u16::MAX),
    )
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    gamestate: u8,
    percent: u8,
    gamemode: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
}

#[must_use]
pub fn view(state: FamidashState) -> View {
    let gameplay = state.gameplay.unwrap_or_default();
    let (x, y) = map_point(state.side, state.level, gameplay.x, gameplay.y);
    View {
        x,
        y,
        health: u8::from(state.gameplay.is_some() || state.complete()),
        level: state.level,
        gamestate: state.game_state,
        percent: state.percent(),
        gamemode: gameplay.gamemode,
        goal: state.finished(),
        loading: state.gameplay.is_none(),
        cleared: state.cleared(),
    }
}

impl<const GROUP: usize> Arcade for Famidash<GROUP> {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::AlphabetOnly
    }

    fn view(state: &FamidashState) -> View {
        view(*state)
    }

    fn motion(state: &FamidashState) -> [u16; 4] {
        match state.gameplay {
            Some(gameplay) => {
                let (x, y) = map_point(state.side, state.level, gameplay.x, gameplay.y);
                [u16::from(state.level), x, y, pose(*state)]
            }
            None => [HIDDEN, 0, 0, 0],
        }
    }

    fn cleared(state: &FamidashState) -> u8 {
        state.cleared()
    }

    fn at_level(level: u8) -> Self {
        Self::new(Stage {
            level,
            ..group_stage(GROUP)
        })
    }
}

impl Arcade for LuckyDraw {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::AlphabetOnly
    }

    fn view(state: &LuckyDrawState) -> View {
        view(state.famidash)
    }

    fn motion(state: &LuckyDrawState) -> [u16; 4] {
        <Famidash<3>>::motion(&state.famidash)
    }

    fn cleared(state: &LuckyDrawState) -> u8 {
        state.famidash.cleared()
    }
}

#[must_use]
pub fn map_top(side: Side, level: u8) -> i32 {
    let tops: &[i32] = match side {
        Side::Main => &MAIN_MAP_TOPS,
        Side::B => &B_MAP_TOPS,
        Side::C => &C_MAP_TOPS,
        Side::D => &D_MAP_TOPS,
    };
    tops.get(usize::from(level)).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_has_a_map_top() {
        for side in Side::ALL {
            assert!((0..side.level_count()).all(|level| map_top(side, level) > 0));
            assert_eq!(map_top(side, side.level_count()), 0);
        }
        assert_eq!(MAIN_MAP_TOPS.len(), usize::from(Side::Main.level_count()));
        assert_eq!(B_MAP_TOPS.len(), usize::from(Side::B.level_count()));
        assert_eq!(C_MAP_TOPS.len(), usize::from(Side::C.level_count()));
        assert_eq!(D_MAP_TOPS.len(), usize::from(Side::D.level_count()));
    }
}
