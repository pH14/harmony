// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use super::HIDDEN;
use crate::{
    ice_climbers::game::{IceClimbers, IceClimbersState, MOUNTAINS},
    search::draw::DrawMixture,
    workload::Arcade,
};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u16,
    y: u16,
    health: u8,
    level: u8,
    floor: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
}

impl Arcade for IceClimbers {
    type View = View;

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn view(state: &IceClimbersState) -> View {
        let state = *state;
        View {
            x: u16::from(state.x),
            y: state.map_y(),
            health: if state.dead() { 0 } else { state.lives },
            level: state.level.min(MOUNTAINS - 1),
            floor: state.floor,
            goal: state.victory(),
            loading: state.in_transition() || state.dead(),
            cleared: Self::cleared(&state),
        }
    }

    fn motion(state: &IceClimbersState) -> [u16; 4] {
        let state = *state;
        if state.in_transition() || state.dead() {
            [HIDDEN, 0, 0, 0]
        } else {
            [
                u16::from(state.level),
                u16::from(state.x),
                state.map_y(),
                u16::from(state.airborne) * 2 + u16::from(state.flipped),
            ]
        }
    }

    fn cleared(state: &IceClimbersState) -> u8 {
        state.level.min(MOUNTAINS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::Game;
    use machine::nes::WRAM_SIZE;

    const OAM: usize = 0x200;
    const PHASE: usize = 0x5a;
    const LIVES: usize = 0x20;
    const AIRBORNE: usize = 0xe0;

    fn wram(sprite: [u8; 4], airborne: bool) -> [u8; WRAM_SIZE] {
        let mut wram = [0xf8; WRAM_SIZE];
        wram[0x64] = 0x38;
        wram[0x66] = 0xc0;
        wram[PHASE] = 1;
        wram[LIVES] = 3;
        wram[AIRBORNE] = u8::from(airborne);
        wram[OAM..OAM + 4].copy_from_slice(&sprite);
        wram
    }

    fn pose(wram: &[u8; WRAM_SIZE]) -> u16 {
        IceClimbers::motion(&IceClimbers::default().decode(None, wram))[3]
    }

    #[test]
    fn the_pose_is_the_airborne_flag_and_the_sprite_flip() {
        let facing = [0xc0, 0x5c, 0x00, 0x38];
        assert_eq!(pose(&wram(facing, false)), 0);
        assert_eq!(pose(&wram(facing, true)), 2);
        let mirrored = [0xc0, 0x5c, 0x40, 0x40];
        assert_eq!(pose(&wram(mirrored, false)), 1);
        assert_eq!(pose(&wram(mirrored, true)), 3);
    }

    #[test]
    fn a_sprite_away_from_the_climber_does_not_set_the_flip() {
        assert_eq!(pose(&wram([0x40, 0x5c, 0x40, 0x90], false)), 0);
    }

    #[test]
    fn a_transition_or_a_lost_life_hides_the_climber() {
        let mut memory = wram([0xc0, 0x5c, 0x00, 0x38], false);
        memory[PHASE] = 0;
        assert_eq!(
            IceClimbers::motion(&IceClimbers::default().decode(None, &memory))[0],
            HIDDEN
        );
        let mut memory = wram([0xc0, 0x5c, 0x00, 0x38], false);
        memory[LIVES] = 2;
        assert_eq!(
            IceClimbers::motion(&IceClimbers::default().decode(None, &memory))[0],
            HIDDEN
        );
    }
}
