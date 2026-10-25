// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    ice_climbers::{
        campaign::{IceClimbersCampaignRun, IceClimbersGame},
        target::{
            IceClimbersMechanicalState, IceClimbersObservations, IceClimbersSnapshot,
            IceClimbersTarget, MOUNTAINS, decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

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

const OAM: usize = 0x200;
const PLAYER_X: usize = 0x64;
const PLAYER_Y: usize = 0x66;
const HORIZONTAL_FLIP: u8 = 0x40;

fn pose(wram: &[u8], airborne: bool) -> u16 {
    let (x, y) = (wram[PLAYER_X], wram[PLAYER_Y]);
    let flipped = wram[OAM..OAM + 0x100]
        .chunks_exact(4)
        .find(|sprite| sprite[0].abs_diff(y) <= 1 && (x..=x.saturating_add(8)).contains(&sprite[3]))
        .is_some_and(|sprite| sprite[2] & HORIZONTAL_FLIP != 0);
    u16::from(airborne) * 2 + u16::from(flipped)
}

fn cleared(state: IceClimbersMechanicalState) -> u8 {
    state.level.min(MOUNTAINS)
}

fn view(state: IceClimbersMechanicalState) -> View {
    View {
        x: u16::from(state.x),
        y: state.map_y(),
        health: if state.dead() { 0 } else { state.lives },
        level: state.level.min(MOUNTAINS - 1),
        floor: state.floor(),
        goal: state.victory(),
        loading: state.in_transition() || state.dead(),
        cleared: cleared(state),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for IceClimbersGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                IceClimbersTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> IceClimbersCampaignRun {
        IceClimbersCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if !state.in_transition() && !state.dead() => [
                u16::from(state.level),
                u16::from(state.x),
                state.map_y(),
                pose(wram, state.airborne),
            ],
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &IceClimbersObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &IceClimbersObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &IceClimbersObservations) -> u8 {
        cleared(observation.decoded)
    }

    fn snapshot_frame_count(snapshot: &IceClimbersSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &IceClimbersSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wram(sprite: [u8; 4]) -> [u8; 2048] {
        let mut wram = [0xf8; 2048];
        wram[PLAYER_X] = 0x38;
        wram[PLAYER_Y] = 0xc0;
        wram[OAM..OAM + 4].copy_from_slice(&sprite);
        wram
    }

    #[test]
    fn the_pose_is_the_airborne_flag_and_the_sprite_flip() {
        let facing = wram([0xc0, 0x5c, 0x00, 0x38]);
        assert_eq!(pose(&facing, false), 0);
        assert_eq!(pose(&facing, true), 2);
        let mirrored = wram([0xc0, 0x5c, 0x40, 0x40]);
        assert_eq!(pose(&mirrored, false), 1);
        assert_eq!(pose(&mirrored, true), 3);
    }

    #[test]
    fn a_sprite_away_from_the_climber_does_not_set_the_flip() {
        let enemy = wram([0x40, 0x5c, 0x40, 0x90]);
        assert_eq!(pose(&enemy, false), 0);
    }
}
