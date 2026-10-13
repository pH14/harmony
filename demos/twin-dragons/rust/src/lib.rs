// SPDX-License-Identifier: AGPL-3.0-or-later

use searcher::search::{
    archive::{Archive, ArchiveCandidate, ArchiveKey},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::{BTreeMap, VecDeque}};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(
    inline_js = "export function capture(){return globalThis.harmonyEngine.capture()} export function memory(){return globalThis.harmonyEngine.memory()} "
)]
extern "C" {
    #[wasm_bindgen(catch)]
        fn capture() -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch)]
    fn memory() -> Result<Vec<u8>, JsValue>;
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Action {
    pub buttons: u8,
    pub frames: u8,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Game {
    TwinDragons,
    TobuTobuGirl,
    SuperMarioBros,
    MegaMan2,
    Metroid,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Observation {
    pub game: Game,
    pub x: u16,
    pub y: u16,
    pub health: u8,
    pub level: u16,
    pub next_level: u8,
    pub gamestate: u8,
    pub goal: bool,
    pub on_ground: bool,
    pub facing: u8,
    pub loading: bool,
    pub cleared: u8,
    pub scene: u8,
    pub climb: u16,
    pub time: u8,
    pub fuel: u8,
    #[serde(default)]
    pub clock: u16,
}

const LEVEL_COUNT: u8 = 3;
const PLAYING: u8 = 1;
const LOADING: u8 = 2;
const TOBU_IN_LEVEL: u8 = 4;
const TOBU_ENDING: u8 = 10;
const TOBU_LEVEL_COMPLETE: u8 = 2;
const TOBU_BAND: u16 = 1024;
const TOBU_MAP_HEIGHTS: [u16; 3] = [2000, 2896, 3792];
const TOBU_SPRITE_LEFT: u16 = 24;
const TOBU_SPRITE_CENTER: u16 = 155;
const SMB_LEVEL_COUNT: u8 = 32;
const SMB_AREA_SLOTS: u16 = 128;
const SMB_PLAY: u8 = 1;
const SMB_VICTORY: u8 = 2;
const SMB_PLAYING_TASK: u8 = 3;
const SMB_KILLED: u8 = 0x0b;
const SMB_ADVANCED_FLAG: u8 = 5;
const SMB_MAP_HEIGHT: u16 = 224;
const SMB_SPRITE_TOP: u16 = 9;
const SMB_BAND: u16 = 64;
const MM2_ORDER: [u8; 14] = [7, 5, 6, 1, 3, 0, 2, 4, 8, 9, 10, 11, 12, 13];
const MM2_LEVEL_COUNT: u8 = 14;
const MM2_SCREEN_HEIGHT: u16 = 224;
const MM2_CROP_TOP: u8 = 8;
const MM2_BAND: u16 = 128;
const MM2_FULL: u8 = 28;
const METROID_AREAS: [(u16, u16, u16, u16); 5] = [(1, 1, 30, 19), (13, 9, 18, 16), (1, 16, 12, 15), (14, 23, 17, 8), (1, 3, 10, 9)];
const METROID_AREA_OF: [u16; 5] = [0, 1, 2, 4, 3];
const METROID_SCREEN_HEIGHT: u16 = 240;
const METROID_CARTRIDGE: usize = 0x800;
const MM2_STAGES: [(u8, &[u8]); 14] = [
    (0, &[0x47, 0x40, 0x4a, 0x40, 0x20, 0x20, 0x00]),
    (0, &[0x49, 0x40, 0x28, 0x20, 0x00]),
    (0, &[0x44, 0x40, 0x20, 0x20, 0x20, 0x80, 0x80, 0x44, 0x40, 0x40, 0x40, 0x22, 0x20, 0x00]),
    (0, &[0x44, 0x40, 0x40, 0x40, 0x26, 0x24, 0x20, 0x00]),
    (
        0,
        &[
            0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x22, 0x20, 0x00,
        ],
    ),
    (0, &[0x46, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x24, 0x20, 0x00]),
    (0, &[0x49, 0x40, 0x28, 0x20, 0x00]),
    (
        0,
        &[
            0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x21, 0x20, 0x00,
        ],
    ),
    (24, &[0x88, 0x80, 0x80, 0x80, 0x80, 0x80, 0x04]),
    (22, &[0x45, 0x40, 0x40, 0x40, 0x45, 0x40, 0x40, 0x00]),
    (24, &[0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x22, 0x00]),
    (22, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x41, 0x40, 0x40, 0x40, 0x40, 0x23, 0x00]),
    (24, &[0x00]),
    (22, &[0x40, 0x40, 0x40, 0x25, 0x00]),
];
const SMB_MAIN_AREAS: [u8; 32] = [
    37, 64, 38, 96, 40, 1, 39, 98, 36, 53, 32, 99, 34, 65, 44, 97, 42, 49, 38, 98, 46, 35, 45, 96, 51, 1, 39, 100,
    48, 50, 33, 101,
];

impl Game {
    fn parse(id: &str) -> Result<Game, &'static str> {
        match id {
            "twin-dragons" => Ok(Game::TwinDragons),
            "tobu-tobu-girl" => Ok(Game::TobuTobuGirl),
            "super-mario-bros" => Ok(Game::SuperMarioBros),
            "mega-man-2" => Ok(Game::MegaMan2),
            "metroid" => Ok(Game::Metroid),
            _ => Err("unknown game"),
        }
    }
    fn ram_bytes(self) -> usize {
        match self {
            Game::TwinDragons | Game::SuperMarioBros | Game::MegaMan2 => 0x800,
            Game::TobuTobuGirl => 0x400,
            Game::Metroid => 0x2800,
        }
    }
    fn decode(self, ram: &[u8]) -> Result<Observation, &'static str> {
        match self {
            Game::TwinDragons => decode(ram),
            Game::TobuTobuGirl => decode_tobu(ram),
            Game::SuperMarioBros => decode_smb(ram),
            Game::MegaMan2 => decode_mm2(ram),
            Game::Metroid => decode_metroid(ram),
        }
    }
    fn admissible(self, o: Observation) -> bool {
        match self {
            Game::TwinDragons => o.health != 0 && o.gamestate != 0,
            Game::TobuTobuGirl => !o.loading || o.gamestate == TOBU_ENDING,
            Game::SuperMarioBros => (o.health != 0 && o.gamestate == SMB_PLAY && !o.goal) || o.won(),
            Game::MegaMan2 => (o.health != 0 && !o.loading && !o.goal) || o.won(),
            Game::Metroid => (o.health != 0 && !o.loading) || o.won(),
        }
    }
    fn level_complete(self, o: Observation) -> bool {
        match self {
            Game::TwinDragons => false,
            Game::TobuTobuGirl => o.gamestate == TOBU_IN_LEVEL && o.scene == TOBU_LEVEL_COMPLETE,
            Game::SuperMarioBros => o.health != 0 && !o.won() && (o.goal || o.gamestate == SMB_VICTORY),
            Game::MegaMan2 => o.health != 0 && !o.won() && o.goal,
            Game::Metroid => false,
        }
    }
    fn starts_at_level_one(self, o: Observation) -> bool {
        match self {
            Game::TwinDragons => o.gamestate == PLAYING && o.level == 0,
            Game::TobuTobuGirl | Game::SuperMarioBros | Game::MegaMan2 | Game::Metroid => !o.loading && o.level == 0,
        }
    }
    fn validate_actions(self, actions: &[Action]) -> Result<(), &'static str> {
        if actions.len() > 10000 {
            return Err("History too long");
        }
        let mut frames = 0_u32;
        for a in actions {
            let menu_buttons = matches!(self, Game::TobuTobuGirl | Game::MegaMan2);
            let blocked = match self {
                Game::Metroid => 8,
                _ if menu_buttons => 0,
                _ => 12,
            };
            if a.frames == 0
                || (!menu_buttons && a.frames > 120)
                || a.buttons & blocked != 0
                || a.buttons & 48 == 48
                || a.buttons & 192 == 192
            {
                return Err("Invalid controller action");
            }
            frames += u32::from(a.frames);
        }
        if frames > 200000 {
            return Err("History too long");
        }
        Ok(())
    }
}

impl ArchiveKey for Observation {
    type Place = (u16, bool, u16, u16);
    type Progress = (u8, u16);
    type Identity = (u16, u16);
    type Lineage = ();
    fn place(self) -> Self::Place {
        match self.game {
            Game::TwinDragons => (self.level, self.loading, self.x / 32, self.y / 32),
            Game::TobuTobuGirl => (self.level, self.loading, self.climb / 32, (self.x + TOBU_SPRITE_LEFT) / 32),
            Game::SuperMarioBros => (
                self.level,
                self.fuel != 0,
                u16::from(self.scene) * 512 + self.climb,
                (self.y / 16) << 8 | u16::from(self.time),
            ),
            Game::MegaMan2 => (self.level, self.loading, self.x / 32, (self.y / 32) << 8 | u16::from(self.time)),
            Game::Metroid => (self.level, self.loading, self.x / 32, self.y / 32),
        }
    }
    fn progress(self) -> Self::Progress {
        match self.game {
            Game::TwinDragons => (self.cleared, 0),
            Game::TobuTobuGirl => (self.cleared, self.climb / TOBU_BAND),
            Game::SuperMarioBros => (self.cleared, self.climb / SMB_BAND),
            Game::MegaMan2 => (self.cleared, (self.climb / MM2_BAND) * 32 + u16::from(self.time)),
            Game::Metroid => (self.cleared, 0),
        }
    }
    fn identity(self) -> Self::Identity {
        match self.game {
            Game::TwinDragons => (self.x / 16, self.y / 16),
            Game::TobuTobuGirl => (0, 0),
            Game::SuperMarioBros => (self.climb % 16, 0),
            Game::MegaMan2 | Game::Metroid => (self.x / 16 % 2, self.y / 16 % 2),
        }
    }
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }
    fn tier_rank_shift() -> u32 {
        1
    }
    fn preference_cmp(self, _: usize, other: Self) -> Ordering {
        match self.game {
            Game::TwinDragons => (self.cleared, self.health).cmp(&(other.cleared, other.health)),
            Game::TobuTobuGirl => (self.cleared, self.time, self.climb).cmp(&(other.cleared, other.time, other.climb)),
            Game::SuperMarioBros => (self.cleared, self.clock).cmp(&(other.cleared, other.clock)),
            Game::MegaMan2 => (self.cleared, self.health).cmp(&(other.cleared, other.health)),
            Game::Metroid => (self.cleared, self.fuel, self.clock).cmp(&(other.cleared, other.fuel, other.clock)),
        }
    }
    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_: &mut Self::Lineage, _: Self) {}
}

impl Observation {
    fn won(self) -> bool {
        match self.game {
            Game::TwinDragons => self.cleared >= LEVEL_COUNT,
            Game::TobuTobuGirl => self.gamestate == TOBU_ENDING,
            Game::SuperMarioBros => self.cleared >= SMB_LEVEL_COUNT,
            Game::MegaMan2 => self.cleared >= MM2_LEVEL_COUNT,
            Game::Metroid => self.goal,
        }
    }
}

pub fn decode(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x800 {
        return Err("emulator memory must contain 2 KiB work RAM");
    }
    let gamestate = ram[0x9e];
    let level = ram[0x42];
    let next_level = ram[0x35];
    let goal = ram[0x4e3] != 0;
    let settled = gamestate == PLAYING
        && (next_level == level.wrapping_add(1) || (level == LEVEL_COUNT - 1 && next_level == 0xff));
    let cleared = match gamestate {
        PLAYING if settled && goal => level.saturating_add(1),
        PLAYING | LOADING => level,
        _ => 0,
    };
    Ok(Observation {
        game: Game::TwinDragons,
        x: u16::from(ram[0x52]) * 256 + u16::from(ram[0x57]) + u16::from(ram[0x62]),
        y: u16::from(ram[0x63]),
        health: ram[0x75],
        level: u16::from(level),
        next_level,
        gamestate,
        goal,
        on_ground: ram[0x6b] != 0,
        facing: ram[0x74],
        loading: !settled,
        cleared,
        scene: 0,
        climb: 0,
        time: 0,
        fuel: 0,
        clock: 0,
    })
}

pub fn decode_tobu(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x400 {
        return Err("emulator memory must contain 1 KiB of work RAM bank 0");
    }
    let gamestate = ram[0xa5];
    let scene = ram[0xa6];
    let level = ram[0xa7].saturating_sub(1);
    let in_level = gamestate == TOBU_IN_LEVEL && scene == 0;
    let climb = u16::from(ram[0x233]) * u16::from(ram[0x237]) + u16::from(ram[0x230]);
    let cleared = if gamestate == TOBU_ENDING { LEVEL_COUNT } else { level };
    Ok(Observation {
        game: Game::TobuTobuGirl,
        x: u16::from(ram[0xc2]).saturating_sub(TOBU_SPRITE_LEFT),
        y: (TOBU_MAP_HEIGHTS[usize::from(level.min(2))] + u16::from(ram[0xc3]))
            .saturating_sub(TOBU_SPRITE_CENTER + climb),
        health: u8::from(in_level || gamestate == TOBU_ENDING),
        level: u16::from(level),
        next_level: 0,
        gamestate,
        goal: ram[0x235] != 0,
        on_ground: false,
        facing: 0,
        loading: !in_level,
        cleared,
        scene,
        climb,
        time: ram[0xb0],
        fuel: ram[0x23a],
        clock: 0,
    })
}

pub fn decode_smb(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x800 {
        return Err("emulator memory must contain 2 KiB work RAM");
    }
    let world = ram[0x75f];
    let level = if ram[0x746] == SMB_ADVANCED_FLAG {
        ram[0x75c].saturating_sub(1)
    } else {
        ram[0x75c]
    };
    let id = world.wrapping_mul(4).wrapping_add(level);
    let room = ram[0x74e].wrapping_mul(32).wrapping_add(ram[0x74f] & 31);
    let mode = ram[0x770];
    let loading = !(mode == SMB_PLAY && ram[0x772] == SMB_PLAYING_TASK);
    let in_area = !loading && SMB_MAIN_AREAS.get(usize::from(id)) != Some(&room);
    let camera = u16::from(ram[0x71a]) * 256 + u16::from(ram[0x71c]);
    let x = u16::from(ram[0x6d]) * 256 + u16::from(ram[0x86]);
    let y = match ram[0xb5] {
        0 => 0,
        1 => (u16::from(ram[0xce]) + SMB_SPRITE_TOP).min(SMB_MAP_HEIGHT - 1),
        _ => SMB_MAP_HEIGHT - 1,
    };
    let dead = ram[0x0e] == SMB_KILLED || ram[0xb5] >= 2;
    let goal = ram[0x746] != 0;
    let victory = mode == SMB_VICTORY && world == 7;
    let screen_x = if loading { 0 } else { (x.saturating_sub(camera) >> 4).min(15) };
    Ok(Observation {
        game: Game::SuperMarioBros,
        x,
        y,
        health: u8::from(!dead),
        level: if in_area {
            u16::from(SMB_LEVEL_COUNT) + u16::from(id) * SMB_AREA_SLOTS + u16::from(room)
        } else {
            u16::from(id)
        },
        next_level: 0,
        gamestate: mode,
        goal,
        on_ground: ram[0x1d] == 0,
        facing: ram[0x33],
        loading,
        cleared: if victory {
            SMB_LEVEL_COUNT
        } else {
            id + u8::from(goal || mode == SMB_VICTORY)
        },
        scene: room,
        climb: if loading { 0 } else { camera >> 4 } + screen_x,
        time: ram[0x7f8],
        fuel: u8::from(ram[0x6d9] == ram[0x6da]),
        clock: u16::from(ram[0x7f8]) * 100 + u16::from(ram[0x7f9]) * 10 + u16::from(ram[0x7fa]),
    })
}

fn mm2_cell(stage: u8, screen: u8) -> (u16, u16) {
    let (start, entries) = MM2_STAGES[usize::from(stage.min(13))];
    let mut placed = Vec::new();
    let (mut col, mut row, mut first) = (0_i32, 0_i32, i32::from(start));
    for &entry in entries {
        let count = i32::from(entry & 31) + 1;
        for k in 0..count {
            placed.push((first + k, col + k, row));
        }
        first += count;
        col += count - 1;
        if entry & 0x40 != 0 {
            row += 1;
        } else if entry & 0x80 != 0 {
            row -= 1;
        } else if entry & 0x20 != 0 {
            col += 1;
        }
    }
    let top = placed.iter().map(|p| p.2).min().unwrap_or(0);
    let found = placed
        .iter()
        .find(|p| p.0 == i32::from(screen))
        .or_else(|| placed.iter().find(|p| p.0 == i32::from(start)))
        .copied()
        .unwrap_or((0, 0, 0));
    (found.1 as u16, (found.2 - top) as u16)
}

pub fn decode_mm2(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x800 {
        return Err("emulator memory must contain 2 KiB work RAM");
    }
    let stage = ram[0x2a];
    let state = ram[0x2c];
    let bank = ram[0x29];
    let phase = ram[0xb1];
    let level = MM2_ORDER.iter().position(|&s| s == stage).unwrap_or(0) as u8;
    let (col, row) = mm2_cell(stage, ram[0x440]);
    let start = MM2_STAGES[usize::from(stage.min(13))].0;
    let dead = ram[0x6c0] == 0 || state == 0 || (state == 1 && ram[0x4a0] >= 0xe0);
    let in_play = bank == 0x0e && stage < 14 && (2..=10).contains(&state);
    let last_castle_boss = stage != 12 || (ram[0xbc] == 0xff && ram[0xb3] == 12);
    let complete = bank == 0x0e
        && stage < 14
        && if stage < 8 {
            phase >= 0xfe
        } else {
            phase == 0xff && last_castle_boss
        };
    let won = stage == 13 && complete;
    let boss_damage = if phase >= 0xfe {
        MM2_FULL
    } else if phase >= 2 && ram[0x6c1] > 0 {
        MM2_FULL.saturating_sub(ram[0x6c1])
    } else {
        0
    };
    Ok(Observation {
        game: Game::MegaMan2,
        x: col * 256 + u16::from(ram[0x460]),
        y: (row * MM2_SCREEN_HEIGHT + u16::from(ram[0x4a0].saturating_sub(MM2_CROP_TOP)))
            .min(row * MM2_SCREEN_HEIGHT + MM2_SCREEN_HEIGHT - 1),
        health: if dead { 0 } else { ram[0x6c0] },
        level: u16::from(level),
        next_level: 0,
        gamestate: phase,
        goal: complete,
        on_ground: state == 3,
        facing: u8::from(ram[0x420] & 0x40 != 0),
        loading: !in_play,
        cleared: if won { MM2_LEVEL_COUNT } else { level + u8::from(complete) },
        scene: ram[0x38],
        climb: u16::from(ram[0x440].saturating_sub(start)) * 256 + u16::from(ram[0x460]),
        time: boss_damage,
        fuel: 0,
        clock: 0,
    })
}

fn bcd(byte: u8) -> u16 {
    u16::from(byte >> 4) * 10 + u16::from(byte & 15)
}

fn metroid_cell(ram: &[u8]) -> (u8, u8) {
    let direction = ram[0x49];
    let vertical = direction < 2;
    let backward = direction == 0 || direction == 2;
    let scroll = ram[if vertical { 0xfc } else { 0xfd }];
    let in_selected = ram[0x30c] == u8::from(ram[0xff] & 3 != 0);
    let ahead = scroll == 0 || in_selected == backward;
    let step = |v: u8| {
        if ahead {
            v
        } else if backward {
            v.wrapping_add(1)
        } else {
            v.wrapping_sub(1)
        }
    };
    if vertical {
        (ram[0x50], step(ram[0x4f]))
    } else {
        (step(ram[0x50]), ram[0x4f])
    }
}

pub fn decode_metroid(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x2800 {
        return Err("emulator memory must contain 2 KiB work RAM and 8 KiB cartridge RAM");
    }
    let cart = |address: usize| ram[METROID_CARTRIDGE + address - 0x6000];
    let level = METROID_AREA_OF.get(usize::from(ram[0x74] & 15)).copied().unwrap_or(0);
    let (x0, y0, cols, rows) = METROID_AREAS[usize::from(level)];
    let (cx, cy) = metroid_cell(ram);
    let x = ((i32::from(cx) - i32::from(x0)) * 256 + i32::from(ram[0x30e])).clamp(0, i32::from(cols) * 256 - 1);
    let y = ((i32::from(cy) - i32::from(y0)) * i32::from(METROID_SCREEN_HEIGHT) + i32::from(ram[0x30d]))
        .clamp(0, i32::from(rows * METROID_SCREEN_HEIGHT) - 1);
    let health = bcd(ram[0x107]) * 100 + bcd(ram[0x106]);
    let playing = ram[0x1e] == 3;
    let dead = playing && (health == 0 || health >= 8000);
    let won = cart(0x6883) != 0;
    let door = ram[0x56];
    let cleared = cart(0x6878).count_ones() as u8
        + cart(0x6877).min(6)
        + (cart(0x687b) >> 7)
        + (cart(0x687c) >> 7)
        + u8::from(won);
    let pose = ram[0x300];
    Ok(Observation {
        game: Game::Metroid,
        x: x as u16,
        y: y as u16,
        health: if dead { 0 } else { health.div_ceil(10).min(255) as u8 },
        level,
        next_level: 0,
        gamestate: ram[0x1e],
        goal: won,
        on_ground: pose <= 1 || pose == 0xff,
        facing: u8::from(ram[0x4d] == 0),
        loading: !playing || (door != 0 && door != 5),
        cleared,
        scene: ram[0x74],
        climb: 0,
        time: 0,
        fuel: cart(0x6879),
        clock: health,
    })
}

const DIRECTIONS: [u8; 9] = [0, 0x80, 0x40, 0x10, 0x20, 0x90, 0xa0, 0x50, 0x60];
fn below(rng: &mut RomuDuoJrRand, n: usize) -> usize {
    rng.below(std::num::NonZeroUsize::new(n).unwrap())
}
fn draw(rng: &mut RomuDuoJrRand, previous: Option<Action>) -> Action {
    let buttons = match previous {
        None => DIRECTIONS[below(rng, 9)] | below(rng, 4) as u8,
        Some(p) => match below(rng, 4) {
            0 => {
                let dirs: Vec<_> = DIRECTIONS
                    .iter()
                    .copied()
                    .filter(|d| *d != p.buttons & 0xf0)
                    .collect();
                (p.buttons & 3) | dirs[below(rng, dirs.len())]
            }
            1 => p.buttons ^ 1,
            2 => p.buttons ^ 2,
            _ => p.buttons,
        },
    };
    let frames = if below(rng, 2) == 0 {
        2 + below(rng, 11)
    } else {
        6 + below(rng, 13)
    };
    Action {
        buttons,
        frames: frames as u8,
    }
}

#[derive(Serialize)]
struct Point {
    observation: Observation,
    retained: Option<usize>,
    frame: u32,
}
#[derive(Serialize)]
struct Batch {
    executions: u32,
    states: usize,
    deaths: u32,
    frames: u64,
    snapshot_bytes: usize,
    stopped: bool,
    won: bool,
    points: Vec<Point>,
}
#[derive(Serialize)]
struct State {
    id: usize,
    observation: Observation,
    actions: Vec<Action>,
    frames: u32,
}

#[wasm_bindgen]
pub struct Explorer {
    game: Game,
    archive: Archive<Action, Observation, (), ()>,
    snapshots: BTreeMap<usize, Box<[u8]>>,
    digests: BTreeMap<usize, u64>,
    rng: RomuDuoJrRand,
    executions: u32,
    deaths: u32,
    frames: u64,
    stopped: bool,
    won: bool,
    snapshot_bytes: usize,
    max_snapshot_bytes: usize,
    retired_snapshots: u32,
    prefix: Vec<Action>,
    pending: VecDeque<Job>,
    points: Vec<Point>,
    snapshot_size: usize,
}

struct Job {
    parent: usize,
    actions: Vec<Action>,
    frame: u32,
}

const MIN_SNAPSHOT_BUDGET: usize = 2 * 1024 * 1024;
const MIN_ACTIVE_ENTRIES: usize = 256;

fn snapshot_digest(bytes: &[u8]) -> u64 {
    let mut a: u32 = 0x811c_9dc5;
    let mut b: u32 = 0x01c9_3a75;
    for &byte in bytes {
        a = (a ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        b = (b ^ u32::from(byte))
            .wrapping_mul(0x0100_0193)
            .rotate_left(5);
    }
    (u64::from(a) << 32) | u64::from(b)
}

fn compress_snapshot(bytes: &[u8]) -> Box<[u8]> {
    miniz_oxide::deflate::compress_to_vec(bytes, 1).into_boxed_slice()
}

fn js_error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl Explorer {
    #[wasm_bindgen(constructor)]
    pub fn new(game: &str, seed: u32) -> Result<Explorer, JsValue> {
        Self::root(Game::parse(game).map_err(js_error)?, seed, Vec::new(), true)
    }

    pub fn from_history(game: &str, seed: u32, history: &str) -> Result<Explorer, JsValue> {
        let game = Game::parse(game).map_err(js_error)?;
        if history.len() > 1000000 {
            return Err(js_error("History file too large"));
        }
        let prefix: Vec<Action> = serde_json::from_str(history).map_err(js_error)?;
        game.validate_actions(&prefix).map_err(js_error)?;
        Self::root(game, seed, prefix, false)
    }

    pub fn state_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn snapshot_bytes(&self) -> usize {
        self.snapshot_bytes
    }

    fn root(game: Game, seed: u32, prefix: Vec<Action>, genesis: bool) -> Result<Explorer, JsValue> {
        let obs = game.decode(&memory()?).map_err(js_error)?;
        if !game.admissible(obs) || (genesis && !game.starts_at_level_one(obs)) {
            return Err(js_error("Game setup did not reach level one"));
        }
        let mut archive = Archive::new(|a: &Action| u64::from(a.frames));
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::<Action>::new(),
                    key: obs,
                    milestones: (),
                },
                (),
            )
            .map_err(js_error)?;
        let won = obs.won();
        let stopped = won
            || prefix.len() >= 10000
            || prefix.iter().map(|a| u32::from(a.frames)).sum::<u32>() >= 200000;
        let raw = capture()?;
        let root = compress_snapshot(&raw);
        let snapshot_bytes = root.len();
        Ok(Self {
            game,
            archive,
            rng: RomuDuoJrRand::with_seed(u64::from(seed)),
            executions: 0,
            deaths: 0,
            frames: 0,
            snapshots: BTreeMap::from([(0, root)]),
            digests: BTreeMap::from([(0, snapshot_digest(&raw))]),
            stopped,
            won,
            snapshot_bytes,
            max_snapshot_bytes: 128 * 1024 * 1024,
            retired_snapshots: 0,
            prefix,
            pending: VecDeque::new(),
            points: Vec::new(),
            snapshot_size: raw.len(),
        })
    }

    fn history(&self, id: usize) -> Result<Vec<Action>, JsValue> {
        let mut actions = self.prefix.clone();
        actions.extend(self.archive.entry_input(id).map_err(js_error)?.actions);
        Ok(actions)
    }

    pub fn set_snapshot_budget(&mut self, bytes: u32) {
        self.max_snapshot_bytes = (bytes as usize).max(MIN_SNAPSHOT_BUDGET);
    }

    pub fn retired_snapshots(&self) -> u32 {
        self.retired_snapshots
    }

    fn release_retired_snapshots(&mut self) {
        let active = &self.archive.active;
        let mut freed = 0;
        let mut released = 0;
        self.snapshots.retain(|&id, bytes| {
            let keep = id == 0 || active.get(id).copied().unwrap_or(false);
            if !keep {
                freed += bytes.len();
                released += 1;
            }
            keep
        });
        self.snapshot_bytes -= freed;
        self.retired_snapshots += released;
    }

    fn enforce_snapshot_budget(&mut self) {
        self.release_retired_snapshots();
        if self.snapshot_bytes > self.max_snapshot_bytes {
            let live = self.archive.active_count();
            self.archive.max_entries = live.saturating_sub(live / 64 + 1).max(MIN_ACTIVE_ENTRIES);
        }
    }

    pub fn plan(&mut self, jobs: u32) -> Result<Vec<u32>, JsValue> {
        let mut plan = Vec::new();
        for _ in 0..jobs {
            if self.stopped {
                break;
            }
            let (parent, selection) = self
                .archive
                .select_parent(&mut self.rng)
                .map_err(js_error)?;
            self.archive.record_selection(parent, &selection);
            let prefix = self.history(parent)?;
            let start: u32 = prefix.iter().map(|a| u32::from(a.frames)).sum();
            let mut frame = start;
            let mut previous = prefix.last().copied();
            let mut actions = Vec::new();
            let count = 1 + below(&mut self.rng, 24);
            for action_count in (prefix.len()..).take(count) {
                let mut action = draw(&mut self.rng, previous);
                if self.game == Game::Metroid && below(&mut self.rng, 8) == 0 {
                    action.buttons ^= 4;
                }
                if frame + u32::from(action.frames) > 200_000 || action_count >= 10_000 {
                    break;
                }
                frame += u32::from(action.frames);
                actions.push(action);
                previous = Some(action);
            }
            plan.push(u32::try_from(parent).map_err(js_error)?);
            plan.push(actions.len() as u32);
            plan.extend(
                actions
                    .iter()
                    .map(|a| (u32::from(a.buttons) << 8) | u32::from(a.frames)),
            );
            self.pending.push_back(Job {
                parent,
                actions,
                frame: start,
            });
        }
        Ok(plan)
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn merge(&mut self, steps: &[u8], transition: &[u32]) -> Result<(), JsValue> {
        if self.stopped {
            return Ok(());
        }
        let job = self
            .pending
            .pop_front()
            .ok_or_else(|| js_error("no rollout is pending"))?;
        let ram_bytes = self.game.ram_bytes();
        let stride = ram_bytes + self.snapshot_size;
        let transition: Vec<Action> = transition
            .iter()
            .map(|&packed| Action { buttons: (packed >> 8) as u8, frames: packed as u8 })
            .collect();
        let planned = steps.len() / stride - usize::from(!transition.is_empty());
        if !steps.len().is_multiple_of(stride)
            || steps.len() < stride * usize::from(!transition.is_empty())
            || planned > job.actions.len()
            || transition.iter().any(|a| a.frames == 0)
        {
            return Err(js_error("rollout result does not match its plan"));
        }
        let mut frame = job.frame;
        let mut current_parent = job.parent;
        let mut suffix = Vec::new();
        let mut productive = false;
        let mut ended = false;
        let mut taken: Vec<(Vec<Action>, &[u8])> = job.actions[..planned]
            .iter()
            .zip(steps.chunks_exact(stride))
            .map(|(action, step)| (vec![*action], step))
            .collect();
        if !transition.is_empty() {
            taken.push((transition.clone(), &steps[planned * stride..]));
        }
        let last = taken.len().saturating_sub(1);
        for (index, (actions, step)) in taken.into_iter().enumerate() {
            let (ram, raw) = step.split_at(ram_bytes);
            for action in &actions {
                self.frames += u64::from(action.frames);
                frame += u32::from(action.frames);
            }
            suffix.extend(actions);
            let obs = self.game.decode(ram).map_err(js_error)?;
            if !self.game.admissible(obs) {
                if self.game.level_complete(obs) && !transition.is_empty() && index + 1 == last {
                    continue;
                }
                self.deaths += 1;
                ended = true;
                break;
            }
            let retained_before = self.archive.live_entry_count();
            let id = self
                .archive
                .insert(
                    Some(current_parent),
                    u64::from(self.executions) + 1,
                    ArchiveCandidate {
                        suffix: suffix.clone(),
                        key: obs,
                        milestones: (),
                    },
                    (),
                )
                .map_err(js_error)?;
            if let Some(id) = id
                && let std::collections::btree_map::Entry::Vacant(entry) = self.snapshots.entry(id)
            {
                let compressed = compress_snapshot(raw);
                self.snapshot_bytes += compressed.len();
                entry.insert(compressed);
                self.digests.insert(id, snapshot_digest(raw));
            }
            productive |= self.archive.live_entry_count() > retained_before;
            if let Some(id) = id {
                current_parent = id;
                suffix.clear();
            }
            self.won |= obs.won();
            self.points.push(Point {
                observation: obs,
                retained: id,
                frame,
            });
            if obs.won() {
                ended = true;
                break;
            }
        }
        if !ended && transition.is_empty() && steps.len() / stride != job.actions.len() {
            return Err(js_error("rollout result ended early"));
        }
        self.archive.record_selection_outcome(job.parent, productive);
        self.enforce_snapshot_budget();
        self.executions += 1;
        self.stopped = self.won;
        if self.stopped {
            self.pending.clear();
        }
        Ok(())
    }

    pub fn take_batch(&mut self) -> Result<String, JsValue> {
        serde_json::to_string(&Batch {
            executions: self.executions,
            states: self.snapshots.len(),
            deaths: self.deaths,
            frames: self.frames,
            snapshot_bytes: self.snapshot_bytes,
            stopped: self.stopped,
            won: self.won,
            points: std::mem::take(&mut self.points),
        })
        .map_err(js_error)
    }

    pub fn state(&self, id: usize) -> Result<String, JsValue> {
        let observation = self
            .archive
            .entry_key(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let actions = self.history(id)?;
        let frames = actions.iter().map(|a| u32::from(a.frames)).sum();
        serde_json::to_string(&State {
            id,
            observation,
            actions,
            frames,
        })
        .map_err(js_error)
    }

    pub fn digest(&self, id: usize) -> Result<String, JsValue> {
        self.digests
            .get(&id)
            .map(|digest| format!("{digest:016x}"))
            .ok_or_else(|| js_error("digest unavailable"))
    }

    pub fn snapshot(&self, id: usize) -> Result<Vec<u8>, JsValue> {
        let compressed = self
            .snapshots
            .get(&id)
            .ok_or_else(|| js_error("snapshot unavailable"))?;
        miniz_oxide::inflate::decompress_to_vec_with_limit(compressed, 1048576)
            .map_err(|_| js_error("invalid compressed snapshot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_digest_matches_the_browser_reference() {
        assert_eq!(snapshot_digest(&[]), 0x811c_9dc5_01c9_3a75);
        assert_eq!(snapshot_digest(b"twin"), 0x9a87_04f3_35dd_7e16);
        assert_ne!(snapshot_digest(b"twin"), snapshot_digest(b"twio"));
    }
    #[test]
    fn recorded_next_level_is_admitted_with_its_real_input_suffix() {
        #[derive(Deserialize)]
        struct Tape {
            observation: Observation,
            actions: Vec<Action>,
        }
        let before: Tape =
            serde_json::from_str(include_str!("../../tests/fixtures/level-one.json")).unwrap();
        let after: Tape =
            serde_json::from_str(include_str!("../../tests/fixtures/level-two.json")).unwrap();
        assert_eq!((before.observation.level, before.observation.cleared), (0, 0));
        assert_eq!((after.observation.level, after.observation.cleared), (1, 1));
        assert_ne!(before.observation.place(), after.observation.place());
        assert!(Game::TwinDragons.admissible(before.observation));
        assert!(Game::TwinDragons.admissible(after.observation));
        assert!(after.actions.starts_with(&before.actions));
        let suffix = after.actions[before.actions.len()..].to_vec();
        assert!(!suffix.is_empty());
        let mut archive = Archive::new(|a: &Action| u64::from(a.frames));
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::<Action>::new(),
                    key: before.observation,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        let id = archive
            .insert(
                Some(root),
                1,
                ArchiveCandidate {
                    suffix: suffix.clone(),
                    key: after.observation,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        assert_ne!(root, id);
        assert_eq!(archive.entry_input(id).unwrap().actions, suffix);
    }

    #[test]
    fn human_histories_reject_invalid_inputs_and_bound_total_work() {
        assert!(
            Game::TwinDragons.validate_actions(&[Action {
                buttons: 128,
                frames: 1
            }])
            .is_ok()
        );
        for action in [
            Action {
                buttons: 8,
                frames: 1,
            },
            Action {
                buttons: 48,
                frames: 1,
            },
            Action {
                buttons: 192,
                frames: 1,
            },
            Action {
                buttons: 0,
                frames: 0,
            },
            Action {
                buttons: 0,
                frames: 121,
            },
        ] {
            assert!(Game::TwinDragons.validate_actions(&[action]).is_err());
        }
        assert!(
            Game::TwinDragons.validate_actions(&vec![
                Action {
                    buttons: 0,
                    frames: 120
                };
                1667
            ])
            .is_err()
        );
        assert!(
            Game::TwinDragons.validate_actions(&vec![
                Action {
                    buttons: 0,
                    frames: 1
                };
                10001
            ])
            .is_err()
        );
    }

    #[test]
    fn twin_dragons_addresses_decode_world_position_and_progress() {
        let mut ram = vec![0; 0x800];
        ram[0x52] = 1;
        ram[0x57] = 0x20;
        ram[0x62] = 0x40;
        ram[0x63] = 100;
        ram[0x75] = 2;
        ram[0x42] = 1;
        ram[0x35] = 2;
        ram[0x9e] = PLAYING;
        let state = decode(&ram).unwrap();
        assert!(!state.loading);
        assert_eq!(
            (state.x, state.y, state.health, state.level, state.next_level),
            (352, 100, 2, 1, 2)
        );
        assert_eq!(state.cleared, 1);
        assert!(!state.won());
        ram[0x4e3] = 1;
        assert_eq!(decode(&ram).unwrap().cleared, 2);
        ram[0x42] = 2;
        let unsettled = decode(&ram).unwrap();
        assert!(unsettled.loading && !unsettled.won());
        assert_eq!(unsettled.cleared, 2);
        ram[0x35] = 0xff;
        assert!(decode(&ram).unwrap().won());
        ram[0x9e] = LOADING;
        assert_eq!(decode(&ram).unwrap().cleared, 2);
        ram[0x9e] = 0;
        assert_eq!(decode(&ram).unwrap().cleared, 0);
        assert!(!Game::TwinDragons.admissible(decode(&ram).unwrap()));
        assert!(decode(&ram[..0x400]).is_err());
        assert!(decode(&vec![0; 0x2800]).is_err());
    }
    #[test]
    fn compressed_snapshot_owns_only_its_payload_and_round_trips() {
        let bytes: Vec<u8> = (0..20998).map(|i| (i % 256) as u8).collect();
        let compressed: Box<[u8]> = compress_snapshot(&bytes);
        assert!(compressed.len() < bytes.len() / 2);
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_with_limit(&compressed, 1048576).unwrap(),
            bytes
        );
    }
    #[test]
    fn heatmap_position_is_not_full_archive_identity() {
        let mut ram = vec![0; 0x800];
        ram[0x9e] = PLAYING;
        ram[0x35] = 1;
        let a = decode(&ram).unwrap();
        let b = Observation { health: 3, ..a };
        assert_eq!(a.place(), b.place());
        assert!(!Game::TwinDragons.admissible(a));
        assert!(Game::TwinDragons.admissible(b));
        let loading = Observation {
            gamestate: LOADING,
            loading: true,
            ..b
        };
        assert_ne!(loading.place(), b.place());
        assert_eq!(b.preference_cmp(0, a), Ordering::Greater);
        let mut rng = RomuDuoJrRand::with_seed(1);
        let mut prev = None;
        for _ in 0..10000 {
            let a = draw(&mut rng, prev);
            assert_eq!(a.buttons & 12, 0);
            assert_ne!(a.buttons & 0x30, 0x30);
            assert_ne!(a.buttons & 0xc0, 0xc0);
            assert!((2..=18).contains(&a.frames));
            prev = Some(a);
        }
    }
    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
    struct ProbeKey(u8);
    impl ArchiveKey for ProbeKey {
        type Place = u8;
        type Progress = u8;
        type Identity = ();
        type Lineage = ();
        fn place(self) -> u8 {
            self.0
        }
        fn progress(self) -> u8 {
            self.0
        }
        fn identity(self) {}
        fn tier_rank_shift() -> u32 {
            7
        }
        fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_: &mut Self::Lineage, _: Self) {}
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn wide_selector_matches_recorded_native_choices() {
        let mut archive = Archive::<u8, ProbeKey, (), ()>::new(|_| 1);
        for id in 0..3 {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![id],
                        key: ProbeKey(id),
                        milestones: (),
                    },
                    (),
                )
                .unwrap();
        }
        let mut rng = RomuDuoJrRand::with_seed(42);
        let choices: Vec<_> = (0..32)
            .map(|_| {
                let (id, draw) = archive.select_parent(&mut rng).unwrap();
                archive.record_selection(id, &draw);
                id
            })
            .collect();
        assert_eq!(
            choices,
            [
                2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 2, 2, 2,
                2, 2, 2, 2
            ]
        );
    }
}
