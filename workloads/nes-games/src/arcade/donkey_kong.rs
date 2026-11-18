// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    donkey_kong::game::{DonkeyKong, DonkeyKongState},
    search::draw::DrawMixture,
    workload::Arcade,
};

const HITBOX_INSET: u8 = 5;
const FACING_LEFT: u8 = 0x40;
const AIRBORNE: u8 = 4;
const LADDER: u8 = 2;

fn corner(state: DonkeyKongState) -> (u16, u16) {
    (
        u16::from(state.x.saturating_sub(HITBOX_INSET)),
        u16::from(state.y.saturating_sub(HITBOX_INSET)),
    )
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    stage: u8,
    round: u8,
    lives: u8,
    bonus: u8,
    score: u32,
    rivets: u8,
    goal: bool,
    climbing: bool,
    airborne: bool,
    loading: bool,
    cleared: u8,
}

impl Arcade for DonkeyKong {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &DonkeyKongState) -> View {
        let state = *state;
        let (x, y) = corner(state);
        View {
            x,
            y,
            health: u8::from(!state.dead()),
            level: state.room(),
            stage: state.stage,
            round: state.round,
            lives: state.lives,
            bonus: state.bonus,
            score: state.score,
            rivets: state.rivets,
            goal: state.round >= 1,
            climbing: state.status == LADDER,
            airborne: state.status == AIRBORNE,
            loading: state.in_transition(),
            cleared: state.stages_cleared(),
        }
    }

    fn motion(state: &DonkeyKongState) -> [u16; 4] {
        let state = *state;
        if state.in_play() {
            let (x, y) = corner(state);
            [
                u16::from(state.room()),
                x,
                y,
                u16::from(state.facing & FACING_LEFT != 0),
            ]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &DonkeyKongState) -> u8 {
        state.stages_cleared()
    }
}
