// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    donkey_kong::{
        campaign::{DonkeyKongCampaignRun, DonkeyKongGame},
        target::{
            DonkeyKongMechanicalState, DonkeyKongObservations, DonkeyKongSnapshot,
            DonkeyKongTarget, decode_state,
        },
    },
    nes_backend::NesBackend,
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const HITBOX_INSET: u8 = 5;
const FACING_LEFT: u8 = 0x40;
const AIRBORNE: u8 = 4;
const LADDER: u8 = 2;

fn corner(state: DonkeyKongMechanicalState) -> (u16, u16) {
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

fn view(state: DonkeyKongMechanicalState) -> View {
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

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for DonkeyKongGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                DonkeyKongTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> DonkeyKongCampaignRun {
        DonkeyKongCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.in_play() => {
                let (x, y) = corner(state);
                [
                    u16::from(state.room()),
                    x,
                    y,
                    u16::from(state.facing & FACING_LEFT != 0),
                ]
            }
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, DonkeyKongMechanicalState::stages_cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &DonkeyKongObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &DonkeyKongObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &DonkeyKongObservations) -> u8 {
        observation.decoded.stages_cleared()
    }

    fn snapshot_frame_count(snapshot: &DonkeyKongSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &DonkeyKongSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
