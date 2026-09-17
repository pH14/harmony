// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, path::Path};

use machine::{
    MachineError,
    gambatte::{GambatteMachine, VideoFrame},
    gb::{A, B, ButtonChord, DOWN, LEFT, RIGHT, UP},
};
use searcher::target::{ExitKind, Target};

pub use machine::gb::WRAM_SIZE;
use serde::{Deserialize, Serialize};

use crate::map::{Overworld, STEP_LEFT, STEP_RIGHT, STEP_UP, step_delta, step_towards};

pub const WRAM_BASE: u16 = 0xc000;

pub const SPRITE_STATE_DATA_1: u16 = 0xc100;
pub const PLAYER_FACING_DIRECTION: u16 = 0xc109;
pub const SPRITE_STATE_DATA_2: u16 = 0xc200;
pub const SPRITE_STRUCT_BYTES: u16 = 0x10;
pub const SPRITE_STRUCT_COUNT: u16 = 16;
pub const SPRITE_MAP_Y: u16 = 4;
pub const SPRITE_MAP_X: u16 = 5;
pub const SPRITE_COORDINATE_BIAS: u8 = 4;
pub const OVERWORLD_MAP: u16 = 0xc6e8;
pub const TOP_MENU_ITEM_Y: u16 = 0xcc24;
pub const TOP_MENU_ITEM_X: u16 = 0xcc25;
pub const CURRENT_MENU_ITEM: u16 = 0xcc26;
pub const MAX_MENU_ITEM: u16 = 0xcc28;
pub const MENU_WATCHED_KEYS: u16 = 0xcc29;
pub const BATTLE_AND_START_SAVED_MENU_ITEM: u16 = 0xcc2d;
pub const LIST_SCROLL_OFFSET: u16 = 0xcc36;
pub const JOY_IGNORE: u16 = 0xcd6b;
pub const ENEMY_MON_HP: u16 = 0xcfe6;
pub const ENEMY_MON_MAX_HP: u16 = 0xcff4;
pub const BATTLE_RESULT: u16 = 0xcf0b;
pub const BATTLE_MON_LEVEL: u16 = 0xd022;
pub const BATTLE_MON_HP: u16 = 0xd015;
pub const BATTLE_MON_MOVES: u16 = 0xd01c;
pub const BATTLE_MON_MAX_HP: u16 = 0xd023;
pub const IS_IN_BATTLE: u16 = 0xd057;
pub const CURRENT_OPPONENT: u16 = 0xd059;
pub const BATTLE_TYPE: u16 = 0xd05a;
pub const TEXT_BOX_ID: u16 = 0xd125;
pub const PARTY_COUNT: u16 = 0xd163;
pub const PARTY_SPECIES: u16 = 0xd164;
pub const PARTY_MONS: u16 = 0xd16b;
pub const PARTY_MON_BYTES: u16 = 44;
pub const PARTY_MON_HP: u16 = 1;
pub const PARTY_MON_LEVEL: u16 = 33;
pub const PARTY_MON_MAX_HP: u16 = 34;
pub const PARTY_SLOTS: u8 = 6;
pub const NUM_BAG_ITEMS: u16 = 0xd31d;
pub const BAG_ITEMS: u16 = 0xd31e;
pub const BAG_SLOTS: u8 = 20;
pub const PLAYER_MONEY: u16 = 0xd347;
pub const OBTAINED_BADGES: u16 = 0xd356;
pub const CUR_MAP: u16 = 0xd35e;
pub const Y_COORD: u16 = 0xd361;
pub const X_COORD: u16 = 0xd362;
pub const CUR_MAP_TILESET: u16 = 0xd367;
pub const CUR_MAP_HEIGHT: u16 = 0xd368;
pub const CUR_MAP_WIDTH: u16 = 0xd369;
pub const CUR_MAP_CONNECTIONS: u16 = 0xd370;
pub const NORTH_CONNECTION_MAP: u16 = 0xd371;
pub const SOUTH_CONNECTION_MAP: u16 = 0xd37c;
pub const WEST_CONNECTION_MAP: u16 = 0xd387;
pub const EAST_CONNECTION_MAP: u16 = 0xd392;
pub const NUMBER_OF_WARPS: u16 = 0xd3ae;
pub const WARP_ENTRIES: u16 = 0xd3af;
pub const WARP_ENTRY_BYTES: u16 = 4;
pub const WARP_DESTINATION_MAP: u16 = 3;
pub const LAST_MAP: u8 = 0xff;
pub const MAX_WARPS: u8 = 32;
pub const NUM_SIGNS: u16 = 0xd4b0;
pub const SIGN_COORDS: u16 = 0xd4b1;
pub const MAX_SIGNS: u8 = 16;
pub const NUM_SPRITES: u16 = 0xd4e1;
pub const TILESET_BANK: u16 = 0xd52b;
pub const TILESET_BLOCKS_POINTER: u16 = 0xd52c;
pub const TILESET_TALKING_OVER_TILES: u16 = 0xd532;
pub const TALKING_OVER_TILE_COUNT: u16 = 3;
pub const TILESET_COLLISION_POINTER: u16 = 0xd530;
pub const WALK_BIKE_SURF_STATE: u16 = 0xd700;
pub const STATUS_FLAGS_5: u16 = 0xd730;
pub const EVENT_FLAGS: u16 = 0xd747;
pub const EVENT_FLAG_BYTES: usize = 320;

pub const BOULDER_BADGE: u8 = 1 << 0;

pub const EVENT_GOT_STARTER: u16 = 34;
pub const EVENT_GOT_OAKS_PARCEL: u16 = 57;
pub const EVENT_OAK_GOT_PARCEL: u16 = 56;
pub const EVENT_GOT_POKEDEX: u16 = 37;

pub const MAP_PEWTER_CITY: u8 = 2;
pub const MAP_VIRIDIAN_FOREST: u8 = 51;
pub const MAP_PEWTER_GYM: u8 = 54;

pub const FLAG_GOT_STARTER: u8 = 1 << 0;
pub const FLAG_GOT_PARCEL: u8 = 1 << 1;
pub const FLAG_DELIVERED_PARCEL: u8 = 1 << 2;
pub const FLAG_GOT_POKEDEX: u8 = 1 << 3;
pub const FLAG_ENTERED_FOREST: u8 = 1 << 4;
pub const FLAG_ENTERED_PEWTER: u8 = 1 << 5;
pub const FLAG_ENTERED_GYM: u8 = 1 << 6;
pub const ROUTE_FLAGS: u8 = 7;
pub const MILESTONE_BITS: usize = ROUTE_FLAGS as usize + 1;

pub const BATTLE_MENU_CURSOR_ROW: u8 = 0x0e;
pub const BATTLE_MENU_LEFT_COLUMN: u8 = 0x09;
pub const BATTLE_MENU_RIGHT_COLUMN: u8 = 0x0f;
pub const BATTLE_MENU_FIGHT: u8 = 0;
pub const BATTLE_MENU_ITEM: u8 = 1;
pub const BATTLE_MENU_PARTY: u8 = 2;
pub const BATTLE_MENU_RUN: u8 = 3;
pub const BATTLE_MOVE_SLOTS: u8 = 4;
pub const MOVE_MENU_CURSOR_ROW: u8 = 0x0c;
pub const MOVE_MENU_COLUMN: u8 = 0x05;
pub const MOVE_MENU_FIRST_ROW: u8 = 1;
pub const TWO_OPTION_MENU: u8 = 0x14;

pub const WALK_STEP_FRAMES: u32 = 48;
pub const TURN_FRAMES: u32 = 2;
pub const PRESS_FRAMES: u32 = 8;
pub const RELEASE_FRAMES: u32 = 8;
pub const SETTLE_FRAMES: u32 = 12;
pub const MAP_LOAD_FRAMES: u32 = 150;
pub const BATTLE_STAGES: u8 = 8;
pub const MENU_WAIT_FRAMES: u32 = 600;
pub const TEXT_TAPS: u32 = 32;
pub const WALK_FRAME_BUDGET: u64 = 1_500;
pub const INTERACT_FRAME_BUDGET: u64 = 600;
pub const BATTLE_MOVE_FRAME_BUDGET: u64 = 1_200;
pub const USE_ITEM_FRAME_BUDGET: u64 = 900;
pub const SWITCH_FRAME_BUDGET: u64 = 900;
pub const ADVANCE_FRAME_BUDGET: u64 = 900;
pub const MAX_ACTION_FRAME_BUDGET: u64 = WALK_FRAME_BUDGET + MAP_LOAD_FRAMES as u64;
pub const NEARBY_TILES: u8 = 6;
pub const RANDOM_WALK_TILES: [u8; 3] = [1, 2, 4];

#[must_use]
pub fn byte(wram: &[u8], address: u16) -> u8 {
    wram.get(usize::from(address.wrapping_sub(WRAM_BASE)))
        .copied()
        .unwrap_or(0)
}

#[must_use]
pub fn word(wram: &[u8], address: u16) -> u16 {
    u16::from(byte(wram, address)) | (u16::from(byte(wram, address.wrapping_add(1))) << 8)
}

#[must_use]
pub fn big_endian_word(wram: &[u8], address: u16) -> u16 {
    (u16::from(byte(wram, address)) << 8) | u16::from(byte(wram, address.wrapping_add(1)))
}

#[must_use]
pub fn event_flag(wram: &[u8], flag: u16) -> bool {
    byte(wram, EVENT_FLAGS + flag / 8) & (1 << (flag % 8)) != 0
}

#[must_use]
pub fn binary_coded_decimal(wram: &[u8], address: u16, bytes: u16) -> u32 {
    (0..bytes).fold(0_u32, |total, offset| {
        let value = byte(wram, address + offset);
        total * 100 + u32::from(value >> 4) * 10 + u32::from(value & 0x0f)
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PartyMon {
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BagSlot {
    pub item: u8,
    pub count: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BlueState {
    pub badges: u8,
    pub route_flags: u8,
    pub map: u8,
    pub x: u8,
    pub y: u8,
    pub facing: u8,
    pub tileset: u8,
    pub party_count: u8,
    pub party: [PartyMon; PARTY_SLOTS as usize],
    pub bag_count: u8,
    pub bag: [BagSlot; BAG_SLOTS as usize],
    pub money: u32,
    pub events_set: u16,
    pub event_digest: u64,
    pub in_battle: u8,
    pub battle_type: u8,
    pub opponent: u8,
    pub enemy_hp: u16,
    pub enemy_max_hp: u16,
    pub text_box: u8,
    pub menu_item: u8,
    pub max_menu_item: u8,
    pub menu_column: u8,
    pub warps: u8,
    pub signs: u8,
    pub sprites: u8,
}

impl BlueState {
    #[must_use]
    pub fn has_badge(self) -> bool {
        self.badges & BOULDER_BADGE != 0
    }

    #[must_use]
    pub fn is_victory(self) -> bool {
        self.has_badge()
    }

    #[must_use]
    pub fn whited_out(self) -> bool {
        self.party_count > 0
            && self
                .party
                .iter()
                .take(usize::from(self.party_count))
                .all(|mon| mon.hp == 0 && mon.max_hp > 0)
    }

    #[must_use]
    pub fn party_hp(self) -> u32 {
        self.party.iter().map(|mon| u32::from(mon.hp)).sum()
    }

    #[must_use]
    pub fn party_levels(self) -> u32 {
        self.party.iter().map(|mon| u32::from(mon.level)).sum()
    }

    #[must_use]
    pub fn in_battle(self) -> bool {
        self.in_battle != 0
    }

    #[must_use]
    pub fn battle_stage(self) -> u8 {
        if !self.in_battle() || self.enemy_max_hp == 0 {
            return 0;
        }
        let left = u32::from(self.enemy_hp.min(self.enemy_max_hp));
        let parts = left * u32::from(BATTLE_STAGES) / u32::from(self.enemy_max_hp);
        (u32::from(BATTLE_STAGES) + 1 - parts)
            .min(u32::from(BATTLE_STAGES))
            .try_into()
            .unwrap_or(BATTLE_STAGES)
    }

    #[must_use]
    pub fn milestone_flags(self) -> u8 {
        self.route_flags
            | if self.has_badge() {
                1 << ROUTE_FLAGS
            } else {
                0
            }
    }
}

#[must_use]
pub fn decode_state(wram: &[u8], latched: u8) -> BlueState {
    let map = byte(wram, CUR_MAP);
    let mut party = [PartyMon::default(); PARTY_SLOTS as usize];
    let party_count = byte(wram, PARTY_COUNT).min(PARTY_SLOTS);
    for (slot, mon) in party.iter_mut().enumerate().take(usize::from(party_count)) {
        let base = PARTY_MONS + PARTY_MON_BYTES * u16::try_from(slot).unwrap_or(0);
        *mon = PartyMon {
            species: byte(wram, base),
            level: byte(wram, base + PARTY_MON_LEVEL),
            hp: big_endian_word(wram, base + PARTY_MON_HP),
            max_hp: big_endian_word(wram, base + PARTY_MON_MAX_HP),
        };
    }
    let mut bag = [BagSlot::default(); BAG_SLOTS as usize];
    let bag_count = byte(wram, NUM_BAG_ITEMS).min(BAG_SLOTS);
    for (slot, entry) in bag.iter_mut().enumerate().take(usize::from(bag_count)) {
        let base = BAG_ITEMS + 2 * u16::try_from(slot).unwrap_or(0);
        *entry = BagSlot {
            item: byte(wram, base),
            count: byte(wram, base + 1),
        };
    }
    let (events_set, event_digest) = event_block(wram);
    BlueState {
        badges: byte(wram, OBTAINED_BADGES),
        route_flags: latched | route_flags(wram, map),
        map,
        x: byte(wram, X_COORD),
        y: byte(wram, Y_COORD),
        facing: byte(wram, PLAYER_FACING_DIRECTION),
        tileset: byte(wram, CUR_MAP_TILESET),
        party_count,
        party,
        bag_count,
        bag,
        money: binary_coded_decimal(wram, PLAYER_MONEY, 3),
        events_set,
        event_digest,
        in_battle: byte(wram, IS_IN_BATTLE),
        battle_type: byte(wram, BATTLE_TYPE),
        opponent: byte(wram, CURRENT_OPPONENT),
        enemy_hp: big_endian_word(wram, ENEMY_MON_HP),
        enemy_max_hp: big_endian_word(wram, ENEMY_MON_MAX_HP),
        text_box: byte(wram, TEXT_BOX_ID),
        menu_item: byte(wram, CURRENT_MENU_ITEM),
        max_menu_item: byte(wram, MAX_MENU_ITEM),
        menu_column: byte(wram, TOP_MENU_ITEM_X),
        warps: byte(wram, NUMBER_OF_WARPS).min(MAX_WARPS),
        signs: byte(wram, NUM_SIGNS).min(MAX_SIGNS),
        sprites: byte(wram, NUM_SPRITES),
    }
}

fn route_flags(wram: &[u8], map: u8) -> u8 {
    let mut flags = 0;
    for (flag, event) in [
        (FLAG_GOT_STARTER, EVENT_GOT_STARTER),
        (FLAG_GOT_PARCEL, EVENT_GOT_OAKS_PARCEL),
        (FLAG_DELIVERED_PARCEL, EVENT_OAK_GOT_PARCEL),
        (FLAG_GOT_POKEDEX, EVENT_GOT_POKEDEX),
    ] {
        if event_flag(wram, event) {
            flags |= flag;
        }
    }
    for (flag, visited) in [
        (FLAG_ENTERED_FOREST, MAP_VIRIDIAN_FOREST),
        (FLAG_ENTERED_PEWTER, MAP_PEWTER_CITY),
        (FLAG_ENTERED_GYM, MAP_PEWTER_GYM),
    ] {
        if map == visited {
            flags |= flag;
        }
    }
    flags
}

fn event_block(wram: &[u8]) -> (u16, u64) {
    let mut set = 0_u16;
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    for offset in 0..EVENT_FLAG_BYTES {
        let value = byte(wram, EVENT_FLAGS + u16::try_from(offset).unwrap_or(0));
        set = set.saturating_add(u16::try_from(value.count_ones()).unwrap_or(0));
        digest = (digest ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (set, digest)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ActionKind {
    WalkTo,
    Interact,
    BattleMove,
    UseItem,
    Switch,
    Advance,
}

pub const ACTION_KINDS: [ActionKind; 6] = [
    ActionKind::WalkTo,
    ActionKind::Interact,
    ActionKind::BattleMove,
    ActionKind::UseItem,
    ActionKind::Switch,
    ActionKind::Advance,
];

impl ActionKind {
    #[must_use]
    pub fn frame_budget(self) -> u64 {
        self.macro_frame_budget() + u64::from(MAP_LOAD_FRAMES)
    }

    #[must_use]
    pub fn macro_frame_budget(self) -> u64 {
        match self {
            Self::WalkTo => WALK_FRAME_BUDGET,
            Self::Interact => INTERACT_FRAME_BUDGET,
            Self::BattleMove => BATTLE_MOVE_FRAME_BUDGET,
            Self::UseItem => USE_ITEM_FRAME_BUDGET,
            Self::Switch => SWITCH_FRAME_BUDGET,
            Self::Advance => ADVANCE_FRAME_BUDGET,
        }
    }

    #[must_use]
    pub fn in_context(self, in_battle: bool) -> Self {
        match (self, in_battle) {
            (Self::WalkTo, true) => Self::BattleMove,
            (Self::Interact, true) => Self::Advance,
            (Self::BattleMove, false) => Self::WalkTo,
            (Self::UseItem | Self::Switch, false) => Self::Interact,
            _ => self,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::WalkTo => "walk_to",
            Self::Interact => "interact",
            Self::BattleMove => "battle_move",
            Self::UseItem => "use_item",
            Self::Switch => "switch",
            Self::Advance => "advance",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BlueAction {
    pub kind: ActionKind,
    pub index: u8,
}

impl BlueAction {
    #[must_use]
    pub fn new(kind: ActionKind, index: u8) -> Self {
        Self { kind, index }
    }
}

#[must_use]
pub fn action_cost(action: &BlueAction) -> u64 {
    action.kind.frame_budget()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Destination {
    pub x: u8,
    pub y: u8,
    pub face: Option<(u8, u8)>,
    pub exit: Option<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Alphabet {
    pub destinations: Vec<Destination>,
    pub moves: u8,
    pub items: u8,
    pub party: u8,
    pub in_battle: bool,
}

impl Alphabet {
    #[must_use]
    pub fn size(&self) -> usize {
        if self.in_battle {
            usize::from(self.moves) + usize::from(self.items) + usize::from(self.party) + 1
        } else {
            self.destinations.len() + 2
        }
    }

    #[must_use]
    pub fn actions(&self) -> Vec<BlueAction> {
        let mut actions = Vec::new();
        if self.in_battle {
            for slot in 0..self.moves {
                actions.push(BlueAction::new(ActionKind::BattleMove, slot));
            }
            for slot in 0..self.items {
                actions.push(BlueAction::new(ActionKind::UseItem, slot));
            }
            for slot in 0..self.party {
                actions.push(BlueAction::new(ActionKind::Switch, slot));
            }
            actions.push(BlueAction::new(ActionKind::Advance, 0));
        } else {
            for index in 0..self.destinations.len() {
                actions.push(BlueAction::new(
                    ActionKind::WalkTo,
                    u8::try_from(index).unwrap_or(u8::MAX),
                ));
            }
            actions.push(BlueAction::new(ActionKind::Interact, 0));
            actions.push(BlueAction::new(ActionKind::Advance, 0));
        }
        actions
    }
}

#[must_use]
pub fn alphabet(wram: &[u8], rom: &[u8], state: BlueState) -> Alphabet {
    if state.in_battle() {
        return Alphabet {
            destinations: Vec::new(),
            moves: BATTLE_MOVE_SLOTS,
            items: state.bag_count.min(4),
            party: state.party_count,
            in_battle: true,
        };
    }
    let mut destinations = Vec::new();
    if let Some(overworld) = Overworld::decode(wram, rom) {
        let here = (state.x, state.y);
        for index in 0..u16::from(state.warps) {
            let base = WARP_ENTRIES + WARP_ENTRY_BYTES * index;
            let (y, x) = (byte(wram, base), byte(wram, base + 1));
            destinations.push(Destination {
                x,
                y,
                face: None,
                exit: edge_step(&overworld, (x, y)),
            });
        }
        for anchor in interactables(wram, state) {
            destinations.extend(approach_tiles(&overworld, here, anchor));
        }
        destinations.extend(edge_exits(&overworld, here));
        for distance in RANDOM_WALK_TILES {
            for step in crate::map::STEPS {
                let (dx, dy) = step_delta(step);
                let x = i64::from(here.0) + i64::from(dx) * i64::from(distance);
                let y = i64::from(here.1) + i64::from(dy) * i64::from(distance);
                let (Ok(x), Ok(y)) = (u8::try_from(x.max(0)), u8::try_from(y.max(0))) else {
                    continue;
                };
                if overworld.walkable(x, y) {
                    destinations.push(Destination {
                        x,
                        y,
                        face: None,
                        exit: None,
                    });
                }
            }
        }
    }
    Alphabet {
        destinations,
        moves: 0,
        items: 0,
        party: 0,
        in_battle: false,
    }
}

fn interactables(wram: &[u8], state: BlueState) -> Vec<(u8, u8)> {
    let mut anchors = Vec::new();
    for index in 1..SPRITE_STRUCT_COUNT {
        if u16::from(state.sprites) < index {
            break;
        }
        let base = SPRITE_STATE_DATA_2 + SPRITE_STRUCT_BYTES * index;
        let y = byte(wram, base + SPRITE_MAP_Y);
        let x = byte(wram, base + SPRITE_MAP_X);
        if y < SPRITE_COORDINATE_BIAS || x < SPRITE_COORDINATE_BIAS {
            continue;
        }
        anchors.push((x - SPRITE_COORDINATE_BIAS, y - SPRITE_COORDINATE_BIAS));
    }
    for index in 0..u16::from(state.signs) {
        let base = SIGN_COORDS + 2 * index;
        anchors.push((byte(wram, base + 1), byte(wram, base)));
    }
    anchors.retain(|(x, y)| {
        x.abs_diff(state.x) <= NEARBY_TILES && y.abs_diff(state.y) <= NEARBY_TILES
    });
    anchors
}

fn edge_step(overworld: &Overworld, tile: (u8, u8)) -> Option<u8> {
    let (width, height) = (overworld.width(), overworld.height());
    if usize::from(tile.1) + 1 == height {
        Some(crate::map::STEP_DOWN)
    } else if tile.1 == 0 {
        Some(crate::map::STEP_UP)
    } else if usize::from(tile.0) + 1 == width {
        Some(crate::map::STEP_RIGHT)
    } else if tile.0 == 0 {
        Some(crate::map::STEP_LEFT)
    } else {
        None
    }
}

fn edge_exits(overworld: &Overworld, from: (u8, u8)) -> Vec<Destination> {
    let distance = overworld.distances(from);
    let (width, height) = (overworld.width(), overworld.height());
    let index = |x: usize, y: usize| y * width + x;
    crate::map::STEPS
        .into_iter()
        .filter_map(|step| {
            let tiles: Vec<(usize, usize)> = match step {
                crate::map::STEP_UP => (0..width).map(|x| (x, 0)).collect(),
                crate::map::STEP_DOWN => (0..width).map(|x| (x, height - 1)).collect(),
                crate::map::STEP_LEFT => (0..height).map(|y| (0, y)).collect(),
                _ => (0..height).map(|y| (width - 1, y)).collect(),
            };
            tiles
                .into_iter()
                .filter(|(x, y)| {
                    distance[index(*x, *y)] != u32::MAX
                        && overworld.walkable(
                            u8::try_from(*x).unwrap_or(u8::MAX),
                            u8::try_from(*y).unwrap_or(u8::MAX),
                        )
                })
                .min_by_key(|(x, y)| distance[index(*x, *y)])
                .and_then(|(x, y)| {
                    Some(Destination {
                        x: u8::try_from(x).ok()?,
                        y: u8::try_from(y).ok()?,
                        face: None,
                        exit: Some(step),
                    })
                })
        })
        .collect()
}

fn approach_tiles(overworld: &Overworld, from: (u8, u8), anchor: (u8, u8)) -> Vec<Destination> {
    if anchor == from {
        return Vec::new();
    }
    crate::map::STEPS
        .into_iter()
        .filter_map(|step| {
            let near = overworld.neighbour(anchor.0, anchor.1, step)?;
            if overworld.walkable(near.0, near.1) {
                return Some((near, anchor));
            }
            if !overworld.talks_over(near.0, near.1) {
                return None;
            }
            let far = overworld.neighbour(near.0, near.1, step)?;
            overworld.walkable(far.0, far.1).then_some((far, near))
        })
        .filter(|(tile, _)| overworld.route(from, *tile).is_some())
        .map(|(tile, face)| Destination {
            x: tile.0,
            y: tile.1,
            face: Some(face),
            exit: None,
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueObservation {
    pub frame_count: u64,
    pub state: BlueState,
    pub alphabet_size: u16,
    pub dead: bool,
    pub links: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueSnapshot {
    emulator_state: Vec<u8>,
    observation: BlueObservation,
    failed: bool,
}

impl BlueSnapshot {
    #[must_use]
    pub fn state(&self) -> BlueState {
        self.observation.state
    }

    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.len()
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_tests(state: BlueState) -> Self {
        Self {
            emulator_state: Vec::new(),
            observation: BlueObservation {
                frame_count: 0,
                state,
                alphabet_size: 0,
                dead: false,
                links: Vec::new(),
            },
            failed: false,
        }
    }
}

pub struct BlueTarget {
    machine: GambatteMachine,
    rom: Vec<u8>,
    genesis: Vec<u8>,
    genesis_observation: BlueObservation,
    wram: [u8; WRAM_SIZE],
    observation: BlueObservation,
    action_observations: Vec<BlueObservation>,
    failed: bool,
    execution_work: u64,
    action_frames: u64,
    budget: u64,
    latched_flags: u8,
    action_whiteout: bool,
}

impl std::fmt::Debug for BlueTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlueTarget")
            .field("state", &self.observation.state)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl BlueTarget {
    pub fn from_rom_bytes_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let machine = GambatteMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        Self::from_machine(machine, rom, prefix)
    }

    pub fn from_rom_bytes_capturing(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let mut machine = GambatteMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        machine.set_video_capture(true);
        machine.set_audio_capture(true);
        Self::from_machine(machine, rom, prefix)
    }

    fn from_machine(
        mut machine: GambatteMachine,
        rom: &[u8],
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        machine.set_wram_capture(false);
        machine.run_chords(prefix)?;
        let wram = machine.read_wram()?;
        let snap = machine.snapshot()?;
        let genesis = machine.take_snapshot(snap)?;
        let state = decode_state(&wram, 0);
        let observation = BlueObservation {
            frame_count: 0,
            state,
            alphabet_size: u16::try_from(alphabet(&wram, rom, state).size()).unwrap_or(u16::MAX),
            dead: state.whited_out(),
            links: crate::graph::map_links(&wram).1,
        };
        Ok(Self {
            machine,
            rom: rom.to_vec(),
            genesis,
            genesis_observation: observation.clone(),
            wram,
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            execution_work: 0,
            action_frames: 0,
            budget: 0,
            action_whiteout: false,
            latched_flags: state.route_flags,
        })
    }

    pub fn drain_frames(&mut self) -> Vec<VideoFrame> {
        self.machine.take_video_frames()
    }

    pub fn drain_audio(&mut self) -> Vec<i16> {
        self.machine.take_audio_samples()
    }

    #[must_use]
    pub fn state(&self) -> BlueState {
        self.observation.state
    }

    #[must_use]
    pub fn alphabet(&self) -> Alphabet {
        alphabet(&self.wram, &self.rom, self.observation.state)
    }

    #[must_use]
    pub fn map_links(&self) -> (u8, Vec<u8>) {
        crate::graph::map_links(&self.wram)
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.dead
    }

    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.observation.state.is_victory()
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[BlueObservation] {
        &self.action_observations
    }

    #[must_use]
    pub fn work_ram(&self) -> &[u8; WRAM_SIZE] {
        &self.wram
    }

    fn peek(&self, address: u16) -> u8 {
        self.machine
            .read(u64::from(address), 1)
            .ok()
            .and_then(|bytes| bytes.first().copied())
            .unwrap_or(0)
    }

    fn hold(&mut self, buttons: u8, frames: u32) -> Result<(), MachineError> {
        for _ in 0..frames {
            if self.action_frames >= self.budget {
                return Ok(());
            }
            self.machine.hold_frame(buttons)?;
            self.action_frames += 1;
        }
        Ok(())
    }

    fn tap(&mut self, buttons: u8) -> Result<(), MachineError> {
        self.hold(buttons, PRESS_FRAMES)?;
        self.hold(0, RELEASE_FRAMES)?;
        if self.party_wiped() {
            self.action_whiteout = true;
        }
        Ok(())
    }

    fn party_word(&self, slot: u8, field: u16) -> u16 {
        let base = PARTY_MONS + PARTY_MON_BYTES * u16::from(slot) + field;
        (u16::from(self.peek(base)) << 8) | u16::from(self.peek(base + 1))
    }

    fn party_wiped(&self) -> bool {
        let count = self.peek(PARTY_COUNT).min(PARTY_SLOTS);
        count > 0
            && (0..count).all(|slot| {
                self.party_word(slot, PARTY_MON_HP) == 0
                    && self.party_word(slot, PARTY_MON_MAX_HP) > 0
            })
    }

    fn spent(&self) -> bool {
        self.action_frames >= self.budget
    }
}

pub const FIRST_TRAINER_OPPONENT: u8 = 200;

#[must_use]
pub fn step_button(step: u8) -> u8 {
    match step {
        STEP_UP => UP,
        STEP_LEFT => LEFT,
        STEP_RIGHT => RIGHT,
        _ => DOWN,
    }
}

impl BlueTarget {
    fn settle(&mut self) -> Result<(), MachineError> {
        self.hold(0, SETTLE_FRAMES)
    }

    fn battle_menu_open(&self) -> bool {
        self.peek(TOP_MENU_ITEM_Y) == BATTLE_MENU_CURSOR_ROW
            && matches!(
                self.peek(TOP_MENU_ITEM_X),
                BATTLE_MENU_LEFT_COLUMN | BATTLE_MENU_RIGHT_COLUMN
            )
            && self.peek(MAX_MENU_ITEM) == 1
            && self.peek(IS_IN_BATTLE) != 0
    }

    fn wait_for_battle_menu(&mut self) -> Result<bool, MachineError> {
        while !self.spent() {
            if self.peek(IS_IN_BATTLE) == 0 {
                return Ok(false);
            }
            if self.battle_menu_open() {
                return Ok(true);
            }
            if self.replace_fainted()? {
                continue;
            }
            self.tap(B)?;
        }
        Ok(false)
    }

    fn battle_mon_hp(&self) -> u16 {
        (u16::from(self.peek(BATTLE_MON_HP)) << 8) | u16::from(self.peek(BATTLE_MON_HP + 1))
    }

    fn replacement_wanted(&self) -> bool {
        if self.peek(IS_IN_BATTLE) == 0 || self.battle_mon_hp() != 0 {
            return false;
        }
        let count = self.peek(PARTY_COUNT).min(PARTY_SLOTS);
        let mut fainted = false;
        let mut live = false;
        for slot in 0..count {
            if self.party_word(slot, PARTY_MON_HP) == 0 {
                fainted = true;
            } else {
                live = true;
            }
        }
        fainted && live
    }

    fn replace_fainted(&mut self) -> Result<bool, MachineError> {
        if !self.replacement_wanted() {
            return Ok(false);
        }
        if self.peek(TEXT_BOX_ID) == TWO_OPTION_MENU {
            self.tap(A)?;
            return Ok(true);
        }
        let count = self.peek(PARTY_COUNT).min(PARTY_SLOTS);
        let Some(slot) = (0..count).find(|slot| self.party_word(*slot, PARTY_MON_HP) > 0) else {
            return Ok(false);
        };
        self.move_cursor_to(slot, PARTY_SLOTS)?;
        self.tap(A)?;
        self.settle()?;
        self.tap(A)?;
        Ok(true)
    }

    fn choose_battle_menu(&mut self, entry: u8) -> Result<bool, MachineError> {
        if !self.wait_for_battle_menu()? {
            return Ok(false);
        }
        let column = if entry >= BATTLE_MENU_PARTY {
            BATTLE_MENU_RIGHT_COLUMN
        } else {
            BATTLE_MENU_LEFT_COLUMN
        };
        let row = entry % 2;
        for _ in 0..BATTLE_MOVE_SLOTS {
            if self.spent() || self.peek(TOP_MENU_ITEM_X) == column {
                break;
            }
            self.tap(if column == BATTLE_MENU_RIGHT_COLUMN {
                RIGHT
            } else {
                LEFT
            })?;
        }
        for _ in 0..BATTLE_MOVE_SLOTS {
            if self.spent() || self.peek(CURRENT_MENU_ITEM) == row {
                break;
            }
            self.tap(if row == 1 { DOWN } else { UP })?;
        }
        if self.peek(TOP_MENU_ITEM_X) != column || self.peek(CURRENT_MENU_ITEM) != row {
            return Ok(false);
        }
        self.tap(A)?;
        self.settle()?;
        Ok(true)
    }

    fn known_moves(&self) -> u8 {
        (0..BATTLE_MOVE_SLOTS)
            .filter(|slot| self.peek(BATTLE_MON_MOVES + u16::from(*slot)) != 0)
            .count()
            .try_into()
            .unwrap_or(BATTLE_MOVE_SLOTS)
            .max(1)
    }

    fn wait_for_move_menu(&mut self) -> Result<bool, MachineError> {
        while !self.spent() {
            if self.peek(IS_IN_BATTLE) == 0 {
                return Ok(false);
            }
            if self.peek(TOP_MENU_ITEM_Y) == MOVE_MENU_CURSOR_ROW
                && self.peek(TOP_MENU_ITEM_X) == MOVE_MENU_COLUMN
            {
                return Ok(true);
            }
            self.hold(0, 1)?;
        }
        Ok(false)
    }

    fn move_cursor_to(&mut self, wanted: u8, limit: u8) -> Result<(), MachineError> {
        for _ in 0..limit {
            let here = self.peek(CURRENT_MENU_ITEM);
            if self.spent() || here == wanted {
                break;
            }
            self.tap(if here < wanted { DOWN } else { UP })?;
        }
        Ok(())
    }

    fn run_out_the_turn(&mut self) -> Result<(), MachineError> {
        self.settle()?;
        while !self.spent() {
            if self.peek(IS_IN_BATTLE) == 0 || self.battle_menu_open() {
                break;
            }
            if self.replace_fainted()? {
                continue;
            }
            self.tap(B)?;
        }
        self.settle()
    }

    fn walk_to(&mut self, index: u8, alphabet: &Alphabet) -> Result<(), MachineError> {
        if alphabet.destinations.is_empty() {
            return Ok(());
        }
        let destination = alphabet.destinations[usize::from(index) % alphabet.destinations.len()];
        let Some(overworld) = Overworld::decode(&self.wram, &self.rom) else {
            return Ok(());
        };
        let start = (byte(&self.wram, X_COORD), byte(&self.wram, Y_COORD));
        let Some(route) = overworld.route(start, (destination.x, destination.y)) else {
            return Ok(());
        };
        let map = self.peek(CUR_MAP);
        let mut here = start;
        for step in route {
            if self.spent() {
                break;
            }
            let button = step_button(step);
            let mut moved = false;
            let mut waited = 0;
            while waited < WALK_STEP_FRAMES && !self.spent() {
                self.hold(button, 2)?;
                waited += 2;
                let now = (self.peek(X_COORD), self.peek(Y_COORD));
                if now != here {
                    here = now;
                    moved = true;
                    break;
                }
            }
            self.hold(0, 1)?;
            if !moved || self.peek(IS_IN_BATTLE) != 0 || self.peek(CUR_MAP) != map {
                return self.settle();
            }
        }
        if let Some(anchor) = destination.face
            && self.peek(IS_IN_BATTLE) == 0
            && let Some(step) = step_towards(here, anchor)
        {
            self.hold(step_button(step), TURN_FRAMES)?;
        }
        if let Some(step) = destination.exit
            && self.peek(IS_IN_BATTLE) == 0
            && self.peek(CUR_MAP) == map
        {
            let button = step_button(step);
            let mut waited = 0;
            while waited < WALK_STEP_FRAMES && !self.spent() {
                self.hold(button, 2)?;
                waited += 2;
                if self.peek(CUR_MAP) != map {
                    break;
                }
            }
            self.hold(0, 1)?;
        }
        self.settle()
    }

    fn interact(&mut self) -> Result<(), MachineError> {
        let map = self.peek(CUR_MAP);
        for _ in 0..TEXT_TAPS {
            if self.spent() || self.peek(IS_IN_BATTLE) != 0 || self.peek(CUR_MAP) != map {
                break;
            }
            self.tap(A)?;
            if self.peek(TEXT_BOX_ID) == TWO_OPTION_MENU {
                break;
            }
        }
        self.settle()
    }

    fn advance(&mut self, alphabet: &Alphabet) -> Result<(), MachineError> {
        if alphabet.in_battle && self.peek(CURRENT_OPPONENT) < FIRST_TRAINER_OPPONENT {
            if self.choose_battle_menu(BATTLE_MENU_RUN)? {
                return self.run_out_the_turn();
            }
            return self.settle();
        }
        for _ in 0..TEXT_TAPS {
            if self.spent() {
                break;
            }
            self.tap(B)?;
        }
        self.settle()
    }

    fn battle_move(&mut self, index: u8, alphabet: &Alphabet) -> Result<(), MachineError> {
        if !alphabet.in_battle {
            return Ok(());
        }
        if !self.choose_battle_menu(BATTLE_MENU_FIGHT)? {
            return self.settle();
        }
        if !self.wait_for_move_menu()? {
            return self.run_out_the_turn();
        }
        let slots = self.known_moves();
        self.move_cursor_to(index % slots + MOVE_MENU_FIRST_ROW, BATTLE_MOVE_SLOTS)?;
        self.tap(A)?;
        self.run_out_the_turn()
    }

    fn use_item(&mut self, index: u8, alphabet: &Alphabet) -> Result<(), MachineError> {
        if !alphabet.in_battle || alphabet.items == 0 {
            return Ok(());
        }
        if !self.choose_battle_menu(BATTLE_MENU_ITEM)? {
            return self.settle();
        }
        let wanted = index % self.peek(NUM_BAG_ITEMS).max(1);
        for _ in 0..BAG_SLOTS {
            let here = self
                .peek(CURRENT_MENU_ITEM)
                .saturating_add(self.peek(LIST_SCROLL_OFFSET));
            if self.spent() || here == wanted {
                break;
            }
            self.tap(if here < wanted { DOWN } else { UP })?;
        }
        self.tap(A)?;
        self.settle()?;
        self.tap(A)?;
        self.run_out_the_turn()
    }

    fn switch(&mut self, index: u8, alphabet: &Alphabet) -> Result<(), MachineError> {
        if !alphabet.in_battle || alphabet.party <= 1 {
            return Ok(());
        }
        if !self.choose_battle_menu(BATTLE_MENU_PARTY)? {
            return self.settle();
        }
        self.move_cursor_to(index % alphabet.party, PARTY_SLOTS)?;
        self.tap(A)?;
        self.settle()?;
        self.tap(A)?;
        self.run_out_the_turn()
    }

    fn run_macro(&mut self, action: &BlueAction) -> Result<(), MachineError> {
        self.machine.begin_action()?;
        self.action_frames = 0;
        self.action_whiteout = false;
        let ceiling = action.kind.frame_budget();
        self.budget = action.kind.macro_frame_budget();
        let alphabet = self.alphabet();
        let map = self.peek(CUR_MAP);
        match action.kind.in_context(alphabet.in_battle) {
            ActionKind::WalkTo => self.walk_to(action.index, &alphabet),
            ActionKind::Interact => self.interact(),
            ActionKind::BattleMove => self.battle_move(action.index, &alphabet),
            ActionKind::UseItem => self.use_item(action.index, &alphabet),
            ActionKind::Switch => self.switch(action.index, &alphabet),
            ActionKind::Advance => self.advance(&alphabet),
        }?;
        if self.peek(CUR_MAP) != map {
            self.budget = ceiling;
            self.hold(0, MAP_LOAD_FRAMES)?;
        }
        Ok(())
    }

    fn observe_now(&mut self) -> Result<BlueObservation, MachineError> {
        self.wram = self.machine.read_wram()?;
        let state = decode_state(&self.wram, self.latched_flags);
        self.latched_flags = state.route_flags;
        Ok(BlueObservation {
            frame_count: self
                .observation
                .frame_count
                .saturating_add(self.action_frames),
            state,
            alphabet_size: u16::try_from(alphabet(&self.wram, &self.rom, state).size())
                .unwrap_or(u16::MAX),
            dead: state.whited_out() || self.action_whiteout,
            links: crate::graph::map_links(&self.wram).1,
        })
    }
}

impl Target for BlueTarget {
    type Action = BlueAction;
    type Observations = BlueObservation;
    type Snapshot = BlueSnapshot;

    fn reset(&mut self) {
        let genesis = std::mem::take(&mut self.genesis);
        self.failed = self.machine.restore_bytes(&genesis).is_err();
        self.genesis = genesis;
        self.failed |= match self.machine.read_wram() {
            Ok(wram) => {
                self.wram = wram;
                false
            }
            Err(_) => true,
        };
        self.observation = self.genesis_observation.clone();
        self.latched_flags = self.observation.state.route_flags;
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_dead() {
            return;
        }
        if self.run_macro(action).is_err() {
            self.failed = true;
            return;
        }
        self.execution_work = self.execution_work.saturating_add(self.action_frames);
        match self.observe_now() {
            Ok(observation) => {
                self.observation = observation.clone();
                self.action_observations = vec![observation];
            }
            Err(_) => self.failed = true,
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.state;
        (u64::from(state.milestone_flags()) << 32)
            | (u64::from(state.map) << 16)
            | (u64::from(state.x) << 8)
            | u64::from(state.y)
    }

    fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        if self.failed {
            return None;
        }
        let snap = match self.machine.snapshot() {
            Ok(snap) => snap,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        let Ok(emulator_state) = self.machine.take_snapshot(snap) else {
            self.failed = true;
            return None;
        };
        Some(BlueSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        self.machine
            .restore_bytes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        self.wram = self
            .machine
            .read_wram()
            .map_err(|error| error.to_string())?;
        self.observation = snapshot.observation.clone();
        self.latched_flags = self.observation.state.route_flags;
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wram_with(pairs: &[(u16, u8)]) -> Vec<u8> {
        let mut wram = vec![0_u8; WRAM_SIZE];
        for (address, value) in pairs {
            wram[usize::from(*address - WRAM_BASE)] = *value;
        }
        wram
    }

    #[test]
    fn the_route_flags_latch_and_never_clear() {
        let inside = wram_with(&[(CUR_MAP, MAP_VIRIDIAN_FOREST)]);
        let outside = wram_with(&[(CUR_MAP, MAP_PEWTER_CITY)]);
        let first = decode_state(&inside, 0);
        assert_eq!(first.route_flags, FLAG_ENTERED_FOREST);
        let second = decode_state(&outside, first.route_flags);
        assert_eq!(
            second.route_flags,
            FLAG_ENTERED_FOREST | FLAG_ENTERED_PEWTER
        );
    }

    #[test]
    fn an_event_flag_is_read_from_its_own_bit() {
        let wram = wram_with(&[(
            EVENT_FLAGS + EVENT_GOT_STARTER / 8,
            1 << (EVENT_GOT_STARTER % 8),
        )]);
        assert!(event_flag(&wram, EVENT_GOT_STARTER));
        assert!(!event_flag(&wram, EVENT_GOT_POKEDEX));
        assert_eq!(decode_state(&wram, 0).route_flags, FLAG_GOT_STARTER);
    }

    #[test]
    fn a_whiteout_needs_a_party_and_every_member_fainted() {
        let empty = decode_state(&wram_with(&[]), 0);
        assert!(!empty.whited_out());
        let fainted = decode_state(
            &wram_with(&[
                (PARTY_COUNT, 1),
                (PARTY_MONS, 7),
                (PARTY_MONS + PARTY_MON_MAX_HP + 1, 25),
            ]),
            0,
        );
        assert!(fainted.whited_out());
        let unwritten = decode_state(&wram_with(&[(PARTY_COUNT, 1), (PARTY_MONS, 7)]), 0);
        assert!(!unwritten.whited_out());
        let alive = decode_state(
            &wram_with(&[
                (PARTY_COUNT, 1),
                (PARTY_MONS, 7),
                (PARTY_MONS + PARTY_MON_HP + 1, 9),
            ]),
            0,
        );
        assert!(!alive.whited_out());
        assert_eq!(alive.party[0].hp, 9);
    }

    #[test]
    fn money_reads_as_three_binary_coded_decimal_bytes() {
        let wram = wram_with(&[
            (PLAYER_MONEY, 0x01),
            (PLAYER_MONEY + 1, 0x23),
            (PLAYER_MONEY + 2, 0x45),
        ]);
        assert_eq!(decode_state(&wram, 0).money, 12_345);
    }

    #[test]
    fn the_badge_is_the_objective_and_orders_above_every_route_flag() {
        let badged = decode_state(&wram_with(&[(OBTAINED_BADGES, BOULDER_BADGE)]), 0);
        assert!(badged.is_victory());
        assert!(badged.milestone_flags() > decode_state(&wram_with(&[]), 0x7f).milestone_flags());
    }

    #[test]
    fn an_action_declares_the_frames_it_may_spend() {
        for kind in ACTION_KINDS {
            assert!(kind.frame_budget() <= MAX_ACTION_FRAME_BUDGET);
            assert_eq!(action_cost(&BlueAction::new(kind, 0)), kind.frame_budget());
        }
    }

    #[test]
    fn a_battle_alphabet_offers_moves_items_party_and_one_way_out() {
        let wram = wram_with(&[(IS_IN_BATTLE, 1), (PARTY_COUNT, 2), (NUM_BAG_ITEMS, 3)]);
        let state = decode_state(&wram, 0);
        let alphabet = alphabet(&wram, &[], state);
        assert!(alphabet.in_battle);
        assert_eq!(alphabet.size(), 4 + 3 + 2 + 1);
        let actions = alphabet.actions();
        assert_eq!(actions.len(), alphabet.size());
        assert_eq!(
            actions.last().map(|action| action.kind),
            Some(ActionKind::Advance)
        );
    }
}
