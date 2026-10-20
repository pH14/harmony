// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    famidash::{
        campaign::{FamidashCampaignRun, FamidashGame},
        levels::{GROUPS, Side, Stage},
        target::{
            FamidashMechanicalState, FamidashObservations, FamidashSnapshot, FamidashTarget,
            decode_state,
        },
    },
    nes_backend::{NesBackend, NesMemory},
    search::draw::DrawMixture,
};
use machine::nes::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, HIDDEN, Machines};

const MAIN_MAP_TOPS: [i32; 43] = [
    622, 664, 679, 663, 532, 519, 679, 630, 659, 662, 480, 249, 614, 415, 661, 422, 643, 599, 487,
    622, 583, 337, 686, 654, 500, 478, 598, 707, 483, 598, 702, 421, 583, 558, 363, 430, 606, 494,
    542, 436, 588, 480, 662,
];
const B_MAP_TOPS: [i32; 31] = [
    702, 638, 505, 615, 615, 552, 279, 480, 615, 519, 525, 638, 581, 699, 638, 542, 554, 565, 456,
    638, 480, 485, 659, 599, 270, 632, 517, 78, 665, 638, 480,
];
const C_MAP_TOPS: [i32; 25] = [
    518, 458, 692, 512, 62, 446, 599, 370, 403, 645, 286, 574, 574, 670, 663, 670, 375, 599, 480,
    644, 599, 558, 645, 279, 711,
];
const D_MAP_TOPS: [i32; 10] = [574, 574, 583, 670, 99, 480, 289, 679, 546, 580];

#[must_use]
pub fn group_stage(group: usize) -> Stage {
    let group = GROUPS.get(group).unwrap_or(&GROUPS[0]);
    Stage {
        side: group.side,
        level: group.first,
        goal: group.last,
    }
}

fn pose(state: FamidashMechanicalState) -> u16 {
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

fn cleared(state: FamidashMechanicalState) -> u8 {
    state.level + u8::from(state.complete())
}

#[must_use]
pub fn view(state: FamidashMechanicalState) -> View {
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
        cleared: cleared(state),
    }
}

impl<M: NesBackend<Vec<u8>> + NesMemory + 'static, const GROUP: usize> ArcadeGame
    for FamidashGame<M, GROUP>
{
    type Machine = M;
    type View = View;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        let stage = group_stage(GROUP);
        Self::with_targets(
            rom,
            identity,
            identity_sha256,
            stage,
            Box::new(move || {
                FamidashTarget::from_power_on(machines().map_err(|error| error.to_string())?, stage)
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn run() -> FamidashCampaignRun {
        FamidashCampaignRun
    }

    fn mixture() -> DrawMixture {
        DrawMixture::AlphabetOnly
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        match decode_state(wram, group_stage(GROUP)) {
            Ok(state) => match state.gameplay {
                Some(gameplay) => {
                    let (x, y) = map_point(state.side, state.level, gameplay.x, gameplay.y);
                    [u16::from(state.level), x, y, pose(state)]
                }
                None => [HIDDEN, 0, 0, 0],
            },
            Err(_) => [HIDDEN, 0, 0, 0],
        }
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode_state(memory, group_stage(GROUP)).map_or(0, cleared)
    }

    fn memory_view(memory: &[u8]) -> Result<View, Box<dyn Error>> {
        Ok(view(decode_state(memory, group_stage(GROUP))?))
    }

    fn view(observation: &FamidashObservations) -> View {
        view(observation.decoded)
    }

    fn frame_count(observation: &FamidashObservations) -> u64 {
        observation.frame_count
    }

    fn cleared(observation: &FamidashObservations) -> u8 {
        cleared(observation.decoded)
    }

    fn emulator_state(snapshot: &FamidashSnapshot) -> &[u8] {
        &snapshot.emulator_state
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
    fn every_group_starts_at_its_first_level_and_ends_at_its_last() {
        for (index, group) in GROUPS.iter().enumerate() {
            let stage = group_stage(index);
            assert_eq!(
                (stage.side, stage.level, stage.goal),
                (group.side, group.first, group.last)
            );
            assert_eq!(Stage::through_group(group.side, group.first), Ok(stage));
        }
        assert_eq!(group_stage(GROUPS.len()), group_stage(0));
    }

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
