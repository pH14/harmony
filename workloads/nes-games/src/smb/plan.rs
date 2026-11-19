// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    search::plan::{self, StageProgress},
    smb::target::{ROOM_IDENTITY_BYTES, WRAM_SIZE, smb_area_is_loading, smb_camera_pixels},
};

const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const WARP_ZONE_CONTROL_OFFSET: usize = 0x06d6;
const PLAYER_STATE_OFFSET: usize = 0x001d;
const PLAYER_CLIMBING_STATE: u8 = 3;
const ENEMY_FLAG_OFFSET: usize = 0x000f;
const ENEMY_ID_OFFSET: usize = 0x0016;
const ENEMY_SLOTS: usize = 5;
const VINE_ENEMY_ID: u8 = 0x2f;
const VINE_START_Y_OFFSET: usize = 0x039d;
const AREA_POINTER_OFFSET: usize = 0x0750;
const VINE_AREA_POINTER: u8 = 0x2f;
const PLAYER_X_PAGE_OFFSET: usize = 0x006d;
const PLAYER_X_LOW_OFFSET: usize = 0x0086;
const EVEN_PAGE_BLOCK_BUFFER: usize = 0x0500;
const ODD_PAGE_BLOCK_BUFFER: usize = 0x05d0;
const HIDDEN_COIN_METATILE: u8 = 0x5f;

const FOUR_TWO: (u8, u8) = (3, 1);
const FOUR_TWO_VINE_ROOM: [u8; 2] = [2, 1];
const FOUR_TWO_VINE_AREA_ROOM: [u8; 2] = [1, 15];
const VINE_STEPS: [(usize, usize); 4] = [(63, 6), (64, 5), (65, 6), (66, 7)];
const VINE_STEPS_FROM_PIXEL: u32 = 960;
const VINE_STEPS_READABLE_FROM_CAMERA: u32 = 784;
const VINE_BRICK_LAST_CAMERA: u32 = 1040;
const VINE_AT_THE_STEPS: u16 = 1;
const VINE_SPAWNED: u16 = 6;
const VINE_CLIMBING: u16 = 7;
pub const VINE_AREA: u16 = 8;

pub const FIRST_WARP_ZONE: u8 = 4;
pub const VINE_WARP_ZONE: u8 = 6;

const PROGRESS_BAND: u16 = 64;

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum SmbRoute {
    #[default]
    AllLevels,
    Warps,
    WarpsWithoutVine,
}

impl SmbRoute {
    #[must_use]
    pub fn identifier(self) -> &'static str {
        match self {
            Self::AllLevels => "all_levels",
            Self::Warps => "warps_1_2_to_4_vine_area_4_2_to_8",
            Self::WarpsWithoutVine => "warps_1_2_to_4_2_to_8_without_a_vine_stage",
        }
    }

    pub fn from_identifier(identifier: &str) -> Result<Self, String> {
        [Self::AllLevels, Self::Warps, Self::WarpsWithoutVine]
            .into_iter()
            .find(|route| route.identifier() == identifier)
            .ok_or_else(|| format!("unknown Super Mario Bros. route {identifier}"))
    }

    #[must_use]
    pub fn plan(self) -> &'static [Stage] {
        match self {
            Self::AllLevels => ALL_LEVELS,
            Self::Warps => WARPS,
            Self::WarpsWithoutVine => WARPS_WITHOUT_VINE,
        }
    }

    #[must_use]
    pub fn locate(self, state: &PlanState) -> StageProgress {
        plan::locate(self.plan(), state, None)
    }

    #[must_use]
    pub fn stage_name(self, stage: u8) -> &'static str {
        plan::stage_name(self.plan(), stage)
    }

    #[must_use]
    pub fn omits(self, world: u8, level: u8) -> bool {
        match self {
            Self::AllLevels => false,
            Self::Warps | Self::WarpsWithoutVine => !ROUTE_LEVELS.contains(&(world, level)),
        }
    }

    #[must_use]
    pub fn is_doomed(self, wram: &[u8; WRAM_SIZE]) -> bool {
        match self {
            Self::AllLevels => false,
            Self::Warps => {
                self.omits(wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET])
                    || vine_out_of_reach(wram)
            }
            Self::WarpsWithoutVine => {
                self.omits(wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET])
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Goal {
    Level,
    WarpZone(u8),
    Pipe,
    Vine,
    Victory,
    Ending,
}

#[derive(Clone, Copy, Debug)]
pub struct Stage {
    pub name: &'static str,
    pub world: u8,
    pub level: u8,
    pub goal: Goal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlanState {
    pub world: u8,
    pub level: u8,
    pub band: u16,
    pub warp_zone: u8,
    pub vine: u16,
    pub victory: bool,
}

const fn stage(name: &'static str, world: u8, level: u8, goal: Goal) -> Stage {
    Stage {
        name,
        world,
        level,
        goal,
    }
}

const ALL_LEVELS: &[Stage] = &[
    stage("1-1", 0, 0, Goal::Level),
    stage("1-2", 0, 1, Goal::Level),
    stage("1-3", 0, 2, Goal::Level),
    stage("1-4", 0, 3, Goal::Level),
    stage("2-1", 1, 0, Goal::Level),
    stage("2-2", 1, 1, Goal::Level),
    stage("2-3", 1, 2, Goal::Level),
    stage("2-4", 1, 3, Goal::Level),
    stage("3-1", 2, 0, Goal::Level),
    stage("3-2", 2, 1, Goal::Level),
    stage("3-3", 2, 2, Goal::Level),
    stage("3-4", 2, 3, Goal::Level),
    stage("4-1", 3, 0, Goal::Level),
    stage("4-2", 3, 1, Goal::Level),
    stage("4-3", 3, 2, Goal::Level),
    stage("4-4", 3, 3, Goal::Level),
    stage("5-1", 4, 0, Goal::Level),
    stage("5-2", 4, 1, Goal::Level),
    stage("5-3", 4, 2, Goal::Level),
    stage("5-4", 4, 3, Goal::Level),
    stage("6-1", 5, 0, Goal::Level),
    stage("6-2", 5, 1, Goal::Level),
    stage("6-3", 5, 2, Goal::Level),
    stage("6-4", 5, 3, Goal::Level),
    stage("7-1", 6, 0, Goal::Level),
    stage("7-2", 6, 1, Goal::Level),
    stage("7-3", 6, 2, Goal::Level),
    stage("7-4", 6, 3, Goal::Level),
    stage("8-1", 7, 0, Goal::Level),
    stage("8-2", 7, 1, Goal::Level),
    stage("8-3", 7, 2, Goal::Level),
    stage("8-4", 7, 3, Goal::Level),
];

const WARPS: &[Stage] = &[
    stage("1-1", 0, 0, Goal::Level),
    stage(
        "1-2 to the warp zone",
        0,
        1,
        Goal::WarpZone(FIRST_WARP_ZONE),
    ),
    stage("1-2 pipe to 4-1", 0, 1, Goal::Pipe),
    stage("4-1", 3, 0, Goal::Level),
    stage("4-2 to the vine", 3, 1, Goal::Vine),
    stage("4-2 to the warp zone", 3, 1, Goal::WarpZone(VINE_WARP_ZONE)),
    stage("4-2 pipe to 8-1", 3, 1, Goal::Pipe),
    stage("8-1", 7, 0, Goal::Level),
    stage("8-2", 7, 1, Goal::Level),
    stage("8-3", 7, 2, Goal::Level),
    stage("8-4", 7, 3, Goal::Victory),
    stage("ending", 7, 3, Goal::Ending),
];

const WARPS_WITHOUT_VINE: &[Stage] = &[
    stage("1-1", 0, 0, Goal::Level),
    stage(
        "1-2 to the warp zone",
        0,
        1,
        Goal::WarpZone(FIRST_WARP_ZONE),
    ),
    stage("1-2 pipe to 4-1", 0, 1, Goal::Pipe),
    stage("4-1", 3, 0, Goal::Level),
    stage("4-2 to the warp zone", 3, 1, Goal::WarpZone(VINE_WARP_ZONE)),
    stage("4-2 pipe to 8-1", 3, 1, Goal::Pipe),
    stage("8-1", 7, 0, Goal::Level),
    stage("8-2", 7, 1, Goal::Level),
    stage("8-3", 7, 2, Goal::Level),
    stage("8-4", 7, 3, Goal::Victory),
    stage("ending", 7, 3, Goal::Ending),
];

const ROUTE_LEVELS: [(u8, u8); 8] = [
    (0, 0),
    (0, 1),
    (3, 0),
    (3, 1),
    (7, 0),
    (7, 1),
    (7, 2),
    (7, 3),
];

impl plan::Stage for Stage {
    type State = PlanState;

    fn name(self) -> &'static str {
        self.name
    }

    fn objective(self, state: &PlanState, _carried: Option<u16>) -> u16 {
        match self.goal {
            Goal::Level | Goal::WarpZone(_) | Goal::Victory => state.band,
            Goal::Vine => state.vine,
            Goal::Pipe | Goal::Ending => 0,
        }
    }

    fn done(self, state: &PlanState, _objective: u16) -> bool {
        let here = (state.world, state.level);
        let stage = (self.world, self.level);
        let past = here > stage;
        match self.goal {
            Goal::Level | Goal::Pipe => past,
            Goal::WarpZone(zone) => past || (here == stage && state.warp_zone == zone),
            Goal::Vine => {
                past || (here == stage
                    && (state.vine >= VINE_AREA || state.warp_zone == VINE_WARP_ZONE))
            }
            Goal::Victory => state.victory,
            Goal::Ending => false,
        }
    }
}

#[must_use]
pub fn band(column: u16) -> u16 {
    column / PROGRESS_BAND
}

#[must_use]
pub fn warp_zone(wram: &[u8; WRAM_SIZE]) -> u8 {
    wram[WARP_ZONE_CONTROL_OFFSET]
}

fn room(wram: &[u8; WRAM_SIZE]) -> [u8; 2] {
    ROOM_IDENTITY_BYTES.map(|offset| wram[offset])
}

fn in_four_two(wram: &[u8; WRAM_SIZE]) -> bool {
    (wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET]) == FOUR_TWO
}

fn climbing(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[PLAYER_STATE_OFFSET] == PLAYER_CLIMBING_STATE
}

fn player_pixels(wram: &[u8; WRAM_SIZE]) -> u32 {
    u32::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u32::from(wram[PLAYER_X_LOW_OFFSET])
}

fn vine_spawned(wram: &[u8; WRAM_SIZE]) -> bool {
    (0..ENEMY_SLOTS).any(|slot| {
        wram[ENEMY_FLAG_OFFSET + slot] != 0 && wram[ENEMY_ID_OFFSET + slot] == VINE_ENEMY_ID
    }) || (player_pixels(wram) >= VINE_STEPS_FROM_PIXEL && wram[VINE_START_Y_OFFSET] != 0)
}

fn metatile(wram: &[u8; WRAM_SIZE], column: usize, row: usize) -> u8 {
    let buffer = if (column / 16) % 2 == 1 {
        ODD_PAGE_BLOCK_BUFFER
    } else {
        EVEN_PAGE_BLOCK_BUFFER
    };
    wram[buffer + row * 16 + column % 16]
}

fn vine_out_of_reach(wram: &[u8; WRAM_SIZE]) -> bool {
    in_four_two(wram)
        && room(wram) == FOUR_TWO_VINE_ROOM
        && !smb_area_is_loading(wram)
        && smb_camera_pixels(wram) > VINE_BRICK_LAST_CAMERA
        && !climbing(wram)
        && !(vine_spawned(wram) && wram[AREA_POINTER_OFFSET] == VINE_AREA_POINTER)
}

#[must_use]
pub fn vine_step(wram: &[u8; WRAM_SIZE]) -> u16 {
    if !in_four_two(wram) || smb_area_is_loading(wram) {
        return 0;
    }
    let here = room(wram);
    if here == FOUR_TWO_VINE_AREA_ROOM {
        return VINE_AREA;
    }
    if here != FOUR_TWO_VINE_ROOM {
        return 0;
    }
    if climbing(wram) {
        return VINE_CLIMBING;
    }
    if vine_spawned(wram) {
        return VINE_SPAWNED;
    }
    if player_pixels(wram) < VINE_STEPS_FROM_PIXEL {
        return 0;
    }
    VINE_AT_THE_STEPS + u16::try_from(revealed_vine_step_columns(wram).len()).unwrap_or(0)
}

#[must_use]
pub fn revealed_vine_step_columns(wram: &[u8; WRAM_SIZE]) -> Vec<u16> {
    if !(VINE_STEPS_READABLE_FROM_CAMERA..=VINE_BRICK_LAST_CAMERA)
        .contains(&smb_camera_pixels(wram))
    {
        return Vec::new();
    }
    VINE_STEPS
        .iter()
        .filter(|&&(column, row)| metatile(wram, column, row) != HIDDEN_COIN_METATILE)
        .filter_map(|&(column, _)| u16::try_from(column).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(world: u8, level: u8, band: u16) -> PlanState {
        PlanState {
            world,
            level,
            band,
            ..PlanState::default()
        }
    }

    #[test]
    fn all_levels_is_one_stage_per_level_with_the_band_as_objective() {
        let route = SmbRoute::AllLevels;
        assert_eq!(route.plan().len(), 32);
        assert_eq!(route.locate(&state(0, 0, 2)), (0, 2));
        assert_eq!(route.locate(&state(3, 0, 1)), (12, 1));
        assert_eq!(route.locate(&state(7, 3, 4)), (31, 4));
        assert!(!route.omits(4, 2));
    }

    #[test]
    fn the_warp_route_climbs_to_each_warp_zone_and_its_pipe() {
        let route = SmbRoute::Warps;
        assert_eq!(route.locate(&state(0, 0, 3)), (0, 3));
        assert_eq!(route.locate(&state(0, 1, 2)), (1, 2));
        let zone = PlanState {
            warp_zone: FIRST_WARP_ZONE,
            ..state(0, 1, 2)
        };
        assert_eq!(route.locate(&zone), (2, 0));
        assert_eq!(route.stage_name(2), "1-2 pipe to 4-1");
        assert_eq!(route.locate(&state(3, 0, 1)), (3, 1));
        let steps = PlanState {
            vine: 3,
            ..state(3, 1, 1)
        };
        assert_eq!(route.locate(&steps), (4, 3));
        let vine_area = PlanState {
            vine: VINE_AREA,
            ..state(3, 1, 1)
        };
        assert_eq!(route.locate(&vine_area), (5, 1));
        let vine_zone = PlanState {
            warp_zone: VINE_WARP_ZONE,
            ..vine_area
        };
        assert_eq!(route.locate(&vine_zone), (6, 0));
        assert_eq!(route.locate(&state(7, 0, 0)), (7, 0));
        assert_eq!(route.locate(&state(7, 3, 2)), (10, 2));
        let won = PlanState {
            victory: true,
            ..state(7, 3, 4)
        };
        assert_eq!(route.locate(&won), (11, 0));
    }

    #[test]
    fn the_warp_route_omits_every_level_it_skips() {
        let route = SmbRoute::Warps;
        for (world, level) in ROUTE_LEVELS {
            assert!(!route.omits(world, level));
        }
        assert!(route.omits(0, 2));
        assert!(route.omits(3, 2));
        assert!(route.omits(4, 0));
        assert!(route.omits(6, 3));
        assert_eq!(
            SmbRoute::from_identifier(route.identifier()),
            Ok(SmbRoute::Warps)
        );
    }

    fn four_two_at(camera: u32, player: u32) -> [u8; WRAM_SIZE] {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[WORLD_NUMBER_OFFSET] = FOUR_TWO.0;
        wram[LEVEL_NUMBER_OFFSET] = FOUR_TWO.1;
        wram[ROOM_IDENTITY_BYTES[0]] = FOUR_TWO_VINE_ROOM[0];
        wram[ROOM_IDENTITY_BYTES[1]] = FOUR_TWO_VINE_ROOM[1];
        wram[0x071a] = u8::try_from(camera / 256).unwrap();
        wram[0x071c] = u8::try_from(camera % 256).unwrap();
        wram[PLAYER_X_PAGE_OFFSET] = u8::try_from(player / 256).unwrap();
        wram[PLAYER_X_LOW_OFFSET] = u8::try_from(player % 256).unwrap();
        for (column, row) in VINE_STEPS {
            let buffer = if (column / 16) % 2 == 1 {
                ODD_PAGE_BLOCK_BUFFER
            } else {
                EVEN_PAGE_BLOCK_BUFFER
            };
            wram[buffer + row * 16 + column % 16] = HIDDEN_COIN_METATILE;
        }
        wram
    }

    #[test]
    fn the_vine_steps_count_revealed_blocks_then_the_vine_and_the_climb() {
        assert_eq!(vine_step(&four_two_at(700, 900)), 0);
        let mut wram = four_two_at(900, 1000);
        assert_eq!(vine_step(&wram), VINE_AT_THE_STEPS);
        wram[ODD_PAGE_BLOCK_BUFFER + 6 * 16 + 15] = 0xc4;
        wram[EVEN_PAGE_BLOCK_BUFFER + 7 * 16 + 2] = 0xc4;
        assert_eq!(vine_step(&wram), VINE_AT_THE_STEPS + 2);
        wram[ENEMY_FLAG_OFFSET + 2] = 1;
        wram[ENEMY_ID_OFFSET + 2] = VINE_ENEMY_ID;
        assert_eq!(vine_step(&wram), VINE_SPAWNED);
        let mut scrolled_off = four_two_at(1200, 1300);
        scrolled_off[VINE_START_Y_OFFSET] = 80;
        assert_eq!(vine_step(&scrolled_off), VINE_SPAWNED);
        let mut entering = four_two_at(0, 40);
        entering[VINE_START_Y_OFFSET] = 36;
        assert_eq!(vine_step(&entering), 0);
        wram[PLAYER_STATE_OFFSET] = PLAYER_CLIMBING_STATE;
        assert_eq!(vine_step(&wram), VINE_CLIMBING);
        wram[ROOM_IDENTITY_BYTES[0]] = 2;
        wram[ROOM_IDENTITY_BYTES[1]] = 2;
        assert_eq!(vine_step(&wram), 0);
        wram[ROOM_IDENTITY_BYTES[0]] = 1;
        wram[ROOM_IDENTITY_BYTES[1]] = 5;
        assert_eq!(vine_step(&wram), 0);
        wram[ROOM_IDENTITY_BYTES[0]] = FOUR_TWO_VINE_AREA_ROOM[0];
        wram[ROOM_IDENTITY_BYTES[1]] = FOUR_TWO_VINE_AREA_ROOM[1];
        assert_eq!(vine_step(&wram), VINE_AREA);
    }

    #[test]
    fn passing_the_vine_brick_before_the_vine_spawns_is_doomed_on_the_warp_route() {
        assert!(!SmbRoute::Warps.is_doomed(&four_two_at(1040, 1100)));
        let past = four_two_at(1041, 1100);
        assert!(SmbRoute::Warps.is_doomed(&past));
        assert!(!SmbRoute::AllLevels.is_doomed(&past));
        let mut climbing = past;
        climbing[PLAYER_STATE_OFFSET] = PLAYER_CLIMBING_STATE;
        assert!(!SmbRoute::Warps.is_doomed(&climbing));
        let mut spawned = past;
        spawned[VINE_START_Y_OFFSET] = 80;
        assert!(SmbRoute::Warps.is_doomed(&spawned));
        spawned[AREA_POINTER_OFFSET] = VINE_AREA_POINTER;
        assert!(!SmbRoute::Warps.is_doomed(&spawned));
        let mut five_one = [0_u8; WRAM_SIZE];
        five_one[WORLD_NUMBER_OFFSET] = 4;
        assert!(SmbRoute::Warps.is_doomed(&five_one));
    }

    #[test]
    fn the_route_without_a_vine_stage_climbs_4_2_by_band_to_the_vine_warp_zone() {
        let route = SmbRoute::WarpsWithoutVine;
        assert_eq!(
            SmbRoute::from_identifier(route.identifier()),
            Ok(SmbRoute::WarpsWithoutVine)
        );
        assert_eq!(route.plan().len(), SmbRoute::Warps.plan().len() - 1);
        assert!(route.plan().iter().all(|stage| stage.goal != Goal::Vine));
        let steps = PlanState {
            vine: 3,
            ..state(3, 1, 9)
        };
        assert_eq!(route.locate(&steps), (4, 9));
        let end_of_level = PlanState {
            warp_zone: VINE_WARP_ZONE - 1,
            ..state(3, 1, 40)
        };
        assert_eq!(route.locate(&end_of_level), (4, 40));
        let vine_zone = PlanState {
            warp_zone: VINE_WARP_ZONE,
            ..state(3, 1, 2)
        };
        assert_eq!(route.locate(&vine_zone), (5, 0));
        assert_eq!(route.stage_name(5), "4-2 pipe to 8-1");
        assert!(!route.is_doomed(&four_two_at(1041, 1100)));
        let mut five_one = [0_u8; WRAM_SIZE];
        five_one[WORLD_NUMBER_OFFSET] = 4;
        assert!(route.is_doomed(&five_one));
    }
}
