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
                u16::from(state.floor()),
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
