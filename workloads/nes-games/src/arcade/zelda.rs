// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::NesBackend,
    search::draw::DrawMixture,
    zelda::{
        campaign::{ZeldaCampaignRun, ZeldaGame},
        target::{
            MODE_PLAY, ZeldaMechanicalState, ZeldaObservations, ZeldaSnapshot, ZeldaTarget,
            decode_state,
        },
    },
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    mode: u8,
    level: u8,
    room: u8,
    x: u16,
    y: u16,
    health: u16,
    containers: u8,
    rupees: u8,
    sword: u8,
    items: u16,
    triforce: u8,
    cleared: u8,
}

const CARTRIDGE_BYTES: usize = 0x2000;
const SCREEN_WIDTH: i32 = 256;
const SCREEN_HEIGHT: i32 = 168;
const PLAY_TOP: i32 = 53;
const LINK_DIRECTION: usize = 0x98;
const FACING_LEFT: u8 = 0x02;

fn position(state: ZeldaMechanicalState) -> (u16, u16) {
    let column = i32::from(state.room & 15);
    let row = i32::from(state.room >> 4);
    let x = column * SCREEN_WIDTH + i32::from(state.x);
    let y = row * SCREEN_HEIGHT + (i32::from(state.y) - PLAY_TOP).clamp(0, SCREEN_HEIGHT - 1);
    (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0))
}

fn view(state: ZeldaMechanicalState) -> View {
    let (x, y) = position(state);
    View {
        mode: state.mode,
        level: state.level,
        room: state.room,
        x,
        y,
        health: state.health(),
        containers: state.containers(),
        rupees: state.rupees,
        sword: state.sword,
        items: state.items,
        triforce: state.triforce,
        cleared: state.triforce_pieces(),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for ZeldaGame<M> {
    type Machine = M;
    type View = View;
    const MEMORY_BYTES: usize = WRAM_SIZE + CARTRIDGE_BYTES;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            Box::new(move || {
                ZeldaTarget::from_power_on(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> ZeldaCampaignRun {
        ZeldaCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram) {
            Ok(state) if state.mode == MODE_PLAY => {
                let (x, y) = position(state);
                [
                    u16::from(state.level),
                    x,
                    y,
                    u16::from(wram[LINK_DIRECTION] & FACING_LEFT != 0),
                ]
            }
            _ => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory).map_or(0, ZeldaMechanicalState::triforce_pieces)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory)?))
    }

    fn view(observation: &ZeldaObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &ZeldaObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &ZeldaObservations) -> u8 {
        observation.decoded.triforce_pieces()
    }

    fn snapshot_frame_count(snapshot: &ZeldaSnapshot) -> u64 {
        snapshot.observation.frame_count
    }

    fn emulator_state(snapshot: &ZeldaSnapshot) -> &[u8] {
        &snapshot.emulator_state
    }
}
