// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    punchout::game::{FULL_HEALTH, PunchOut, PunchOutState},
    search::draw::DrawMixture,
    workload::Arcade,
};

const TRACK_WIDTH: u16 = 256;
const TRACK_HEIGHT: u16 = 16;
const TRACK_BOTTOM: u16 = 216;

fn track(lost: u8, span: u16) -> u16 {
    let scaled =
        (u32::from(lost) * u32::from(span) + u32::from(FULL_HEALTH) / 2) / u32::from(FULL_HEALTH);
    scaled.min(u32::from(span) - 1) as u16
}

fn placement(state: PunchOutState) -> (u16, u16) {
    (
        track(
            FULL_HEALTH.saturating_sub(state.opponent_health),
            TRACK_WIDTH,
        ),
        TRACK_BOTTOM - track(FULL_HEALTH.saturating_sub(state.mac_health), TRACK_HEIGHT),
    )
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    mode: u8,
    opponent: u8,
    screen: u8,
    round: u8,
    mac_health: u8,
    opponent_health: u8,
    hearts: u8,
    stars: u8,
    knockdowns: u8,
    opponent_action: u8,
    cleared: u8,
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    loading: bool,
}

impl Arcade for PunchOut {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &PunchOutState) -> View {
        let state = *state;
        let fighting = state.fighting();
        let (x, y) = if fighting { placement(state) } else { (0, 0) };
        View {
            x,
            y,
            health: if fighting { state.mac_health } else { 0 },
            level: state.opponent,
            loading: !fighting,
            mode: state.mode,
            opponent: state.opponent,
            screen: state.screen,
            round: state.round,
            mac_health: state.mac_health,
            opponent_health: state.opponent_health,
            hearts: state.hearts,
            stars: state.stars,
            knockdowns: state.round_knockdowns,
            opponent_action: state.opponent_action,
            cleared: state.opponents_defeated(),
        }
    }

    fn motion(state: &PunchOutState) -> [u16; 4] {
        let state = *state;
        if state.fighting() {
            let (x, y) = placement(state);
            [u16::from(state.opponent), x, y, 0]
        } else {
            [HIDDEN, 0, 0, 0]
        }
    }

    fn cleared(state: &PunchOutState) -> u8 {
        state.opponents_defeated()
    }
}
