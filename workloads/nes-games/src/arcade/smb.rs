// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::NesBackend,
    search::draw::DrawMixture,
    smb::{
        campaign::{
            SNAPSHOT_CHECKPOINT_FORMAT, SmbButtonVocabulary, SmbCampaignRun, SmbGame,
            SmbTerminalPredicate, quicknes_identity,
        },
        target::{
            ROOM_IDENTITY_BYTES, SmbObservations, SmbSnapshot, SmbTarget, smb_is_playing,
            smb_is_victory, smb_mechanical_state_from_wram, smb_player_position, smb_world_cleared,
        },
    },
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const LEVEL_COUNT: u16 = 32;
const AREA_SLOTS: u16 = 128;
const MAP_HEIGHT: u16 = 224;
const SPRITE_TOP: u16 = 9;
const FACING_LEFT: u8 = 2;
const SIDE_PIPE_ENTRY: u8 = 2;
const VERTICAL_PIPE_ENTRY: u8 = 3;
const MAIN_AREAS: [u8; 32] = [
    37, 64, 38, 96, 40, 1, 39, 98, 36, 53, 32, 99, 34, 65, 44, 97, 42, 49, 38, 98, 46, 35, 45, 96,
    51, 1, 39, 100, 48, 50, 33, 101,
];

fn wram(memory: &[u8]) -> Result<&[u8; WRAM_SIZE], Box<dyn Error>> {
    Ok(memory
        .get(..WRAM_SIZE)
        .ok_or("Super Mario Bros. needs 2 KiB of work RAM")?
        .try_into()?)
}

fn area(wram: &[u8; WRAM_SIZE]) -> u8 {
    let [kind, offset] = ROOM_IDENTITY_BYTES.map(|address| wram[address]);
    kind.wrapping_mul(32).wrapping_add(offset & 31)
}

fn level_id(wram: &[u8; WRAM_SIZE]) -> u16 {
    let state = smb_mechanical_state_from_wram(wram);
    u16::from(state.world) * 4 + u16::from(state.level)
}

fn in_area(wram: &[u8; WRAM_SIZE]) -> bool {
    smb_is_playing(wram)
        && !matches!(
            smb_mechanical_state_from_wram(wram).player_engine_state,
            SIDE_PIPE_ENTRY | VERTICAL_PIPE_ENTRY
        )
}

fn map_room(wram: &[u8; WRAM_SIZE]) -> u16 {
    let level = level_id(wram);
    let area = area(wram);
    let main = MAIN_AREAS.get(usize::from(level)).copied();
    if in_area(wram) && main.is_some_and(|main| main != area) {
        LEVEL_COUNT + level * AREA_SLOTS + u16::from(area)
    } else {
        level
    }
}

fn player_y(wram: &[u8; WRAM_SIZE]) -> u16 {
    let position = smb_player_position(wram);
    match position.vertical_page {
        0 => 0,
        1 => (u16::from(position.y) + SPRITE_TOP).min(MAP_HEIGHT - 1),
        _ => MAP_HEIGHT - 1,
    }
}

fn cleared(wram: &[u8; WRAM_SIZE]) -> u8 {
    if smb_is_victory(wram) {
        return LEVEL_COUNT as u8;
    }
    let state = smb_mechanical_state_from_wram(wram);
    let done = (state.flag_active && smb_is_playing(wram)) || smb_world_cleared(wram);
    u8::try_from(level_id(wram)).unwrap_or(u8::MAX) + u8::from(done)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct View {
    x: u32,
    y: u16,
    health: u8,
    level: u16,
    world: u8,
    stage: u8,
    goal: bool,
    loading: bool,
    cleared: u8,
}

fn view(wram: &[u8; WRAM_SIZE]) -> View {
    let state = smb_mechanical_state_from_wram(wram);
    View {
        x: smb_player_position(wram).x,
        y: player_y(wram),
        health: u8::from(!state.dead),
        level: map_room(wram),
        world: state.world,
        stage: state.level,
        goal: state.flag_active,
        loading: !in_area(wram),
        cleared: cleared(wram),
    }
}

impl<M: NesBackend<Vec<u8>> + 'static> ArcadeGame for SmbGame<M> {
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_targets(
            rom,
            quicknes_identity(identity, identity_sha256),
            SNAPSHOT_CHECKPOINT_FORMAT,
            Box::new(move || {
                SmbTarget::from_machine(machines().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> SmbCampaignRun {
        SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::NesPressable,
            terminal: Some(SmbTerminalPredicate::GameVictory),
        }
    }

    fn mixture() -> DrawMixture {
        DrawMixture::EnergySplice { scale: 6 }
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        if !in_area(wram) || smb_mechanical_state_from_wram(wram).dead {
            return [HIDDEN, 0, 0, 0];
        }
        let position = smb_player_position(wram);
        [
            map_room(wram),
            u16::try_from(position.x).unwrap_or(u16::MAX),
            player_y(wram),
            u16::from(position.facing == FACING_LEFT),
        ]
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        wram(memory).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(wram(memory)?))
    }

    fn view(observation: &SmbObservations) -> View {
        view(wram(&observation.wram).unwrap_or(&[0; WRAM_SIZE]))
    }

    fn frame_count(observation: &SmbObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &SmbObservations) -> u8 {
        wram(&observation.wram).map_or(0, cleared)
    }

    fn snapshot_frame_count(snapshot: &SmbSnapshot) -> u64 {
        snapshot.frame_count()
    }

    fn emulator_state(snapshot: &SmbSnapshot) -> &[u8] {
        snapshot.emulator_state()
    }
}

#[must_use]
pub fn main_area(level: u8) -> Option<u8> {
    MAIN_AREAS.get(usize::from(level)).copied()
}
