// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::NesBackend,
    punchout::{
        campaign::{PunchOutCampaignRun, PunchOutGame},
        target::{
            FULL_HEALTH, PunchOutMechanicalState, PunchOutObservations, PunchOutSnapshot,
            PunchOutTarget, decode_state,
        },
    },
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

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
}

fn view(state: PunchOutMechanicalState) -> View {
    View {
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

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for PunchOutGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                PunchOutTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> PunchOutCampaignRun {
        PunchOutCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.fighting() => [
                u16::from(state.opponent),
                u16::from(FULL_HEALTH.saturating_sub(state.opponent_health)),
                u16::from(state.mac_health),
                u16::from(state.opponent_action),
            ],
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, PunchOutMechanicalState::opponents_defeated)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &PunchOutObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &PunchOutObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &PunchOutObservations) -> u8 {
        observation.decoded.opponents_defeated()
    }

    fn snapshot_frame_count(snapshot: &PunchOutSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &PunchOutSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
