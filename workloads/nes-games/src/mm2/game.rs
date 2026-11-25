// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Value};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    mm2::switch::{
        SWITCH_FRAME_LIMIT, SWITCH_ODDS, SWITCH_PLANNED_FRAMES, WeaponSwitch, weapon_menu_row,
    },
    search::{
        assertion::{Assertion, Declarations, Number, Strategy},
        choice::{CERTAIN, ChoiceSite},
    },
    workload::{Game, Moves, Offer, Play, Step},
};
use machine::nes::{ButtonChord, MAX_HOLD_FRAMES};

const CAMERA_STATE: usize = 0x1b;
const CAMERA_STATE_SCROLLING: u8 = 0x80;
const LEVEL_ROOM: usize = 0x20;
const CURRENT_BANK: usize = 0x29;
const STAGE: usize = 0x2a;
const PLAYER_STATE: usize = 0x2c;
const WEAPONS_OBTAINED: usize = 0x9a;
const ITEMS_OBTAINED: usize = 0x9b;
const WEAPON_ENERGY: usize = 0x9c;
const WEAPON_ENERGY_BYTES: usize = 12;
const LIVES: usize = 0xa8;
const SELECTED_WEAPON: usize = 0xa9;
const BOSS_PHASE: usize = 0xb1;
const CURRENT_BOSS: usize = 0xb3;
const REFIGHTS: usize = 0xbc;
const MENU_MODE: usize = 0xf7;
const MENU_CURSOR: usize = 0xfd;
const MENU_PAGE: usize = 0xfe;
const ENEMY_INDEX_TABLE: usize = 0x100;
const ENEMY_HIT_TABLE: usize = 0x110;
const OBJECT_ID_TABLE: usize = 0x400;
const OBJECT_FLAG_TABLE: usize = 0x420;
const PLAYER_SCREEN: usize = 0x440;
const PLAYER_X: usize = 0x460;
const PLAYER_Y: usize = 0x4a0;
const PLAYER_HEALTH: usize = 0x6c0;
const BOSS_HEALTH: usize = 0x6c1;
const ENEMY_HEALTH_TABLE: usize = 0x6d0;

const ENEMY_SLOTS: usize = 16;
const OBJECT_SLOTS: usize = 0x20;
const OBJECT_ACTIVE: u8 = 0x80;
const PLAYER_FACING_RIGHT: u8 = 0x40;
const ENEMY_KILLED_ID: u8 = 0x06;
const ENEMY_HIT_CAP: u8 = 4;
const ENEMY_DAMAGE_CAP: u8 = 24;
const ITEM_OBJECT_FIRST: u8 = 0x38;
const ITEM_OBJECT_LAST: u8 = 0x3a;
const MENU_BANK: u8 = 0x0d;
pub(crate) const GAMEPLAY_BANK: u8 = 0x0e;
pub(crate) const MENU_ROWS: [u8; 2] = [8, 7];
pub(crate) const MENU_PAGE_ROWS: u8 = 8;
pub(crate) const MENU_CLOSED: u8 = 0xff;
const MENU_UNKNOWN: u8 = 0xfe;
const MENU_MODE_STAGE_SELECT: u8 = 0x90;
const WEAPONS: usize = 11;
const FULL_HEALTH: u8 = 28;
const FULL_BOSS_HEALTH: u8 = 28;
const BOSS_PHASE_NONE: u8 = 0x00;
const BOSS_PHASE_FIGHTING: u8 = 0x02;
const BOSS_PHASE_REFILL: u8 = 0x04;
const BOSS_PHASE_DEFEATED: u8 = 0xfe;
const BOSS_PHASE_CLEARED: u8 = 0xff;
const NO_REFIGHT_BOSS: u8 = 0xff;
const WILY_MACHINE: u8 = 12;
const PLAYER_STATE_DYING: u8 = 0x00;
const PLAYER_STATE_FALLEN: u8 = 0x01;
const PLAYER_STATE_HIT: u8 = 0x02;
pub(crate) const PLAYER_STATE_STANDING: u8 = 0x03;
const PLAYER_STATE_AIRBORNE: u8 = 0x06;
const PLAYER_STATE_LADDER: u8 = 0x09;
const PLAYER_STATE_LADDER_TOP: u8 = 0x0a;
const PLAYER_STATE_TELEPORTING: u8 = 0x0b;
const BELOW_PLAY_AREA_Y: u8 = 0xe0;
const FALL_STEP_LIMIT: u8 = 64;
const FALL_RUN_LIMIT: u16 = 256;
const DYING_FRAMES: u32 = 30;
const ROBOT_MASTERS: u8 = 8;
const FIRST_WILY_STAGE: u8 = 8;
const WILY5_STAGE: u8 = 12;
const LAST_WILY_STAGE: u8 = 13;
const FINAL_STAGE: u8 = 14;
const MAX_WEAPON_ENERGY: i64 = 255 * WEAPON_ENERGY_BYTES as i64;

pub const JOYPAD_START: u8 = 1 << 3;
const JOYPAD_UP: u8 = 1 << 4;
pub(crate) const JOYPAD_DOWN: u8 = 1 << 5;
const JOYPAD_LEFT: u8 = 1 << 6;
const JOYPAD_RIGHT: u8 = 1 << 7;

const TITLE_SETTLE_FRAMES: u32 = 600;
const MENU_SETTLE_FRAMES: u32 = 300;
const AWARD_WAIT_CHUNKS: u16 = 15;
const STAGE_SELECT_WALK_ROUNDS: u8 = 16;
const STAGE_START_WAIT_FRAMES: u16 = 1_200;
const WILY_STAGE_START_WAIT_FRAMES: u16 = 8_000;
const BOOT_PRESSES: usize = 3;
const PRESS_FRAMES: u8 = 4;
const CURSOR_SETTLE_FRAMES: u8 = 30;
const STAGE_ARRIVAL_WAIT_FRAMES: [u32; 8] = [669, 666, 670, 665, 668, 668, 669, 670];
const AWARD_SETTLE_FRAMES: u32 = 900;
const DRIVE_FRAMES: u64 = 1_800 + 900 + 16 * 338 + 68 + 4 + 8_000;

const FIRST_ITEM_WEAPON: u8 = 9;
const LAST_WEAPON: u8 = 11;
const CHORD_LABEL: u16 = 0;
const SWITCH_OPTIONS: usize = LAST_WEAPON as usize + 2;
pub const WEAPON_SITE: u16 = 0;
pub const SWITCHES: [(u16, Play<Mm2Move>); SWITCH_OPTIONS] = switches();

const ROBOT_MASTER_ORDER: [u8; 8] = [7, 5, 6, 1, 3, 0, 2, 4];
const CRASH: u8 = 7;

const ENTERED: [&str; 14] = [
    "heat_entered",
    "air_entered",
    "wood_entered",
    "bubble_entered",
    "quick_entered",
    "flash_entered",
    "metal_entered",
    "crash_entered",
    "wily1_entered",
    "wily2_entered",
    "wily3_entered",
    "wily4_entered",
    "wily5_entered",
    "wily6_entered",
];
const BOSS: [Option<&str>; 14] = [
    Some("heat_boss"),
    Some("air_boss"),
    Some("wood_boss"),
    Some("bubble_boss"),
    Some("quick_boss"),
    Some("flash_boss"),
    Some("metal_boss"),
    Some("crash_boss"),
    Some("wily1_boss"),
    Some("wily2_boss"),
    Some("wily3_boss"),
    Some("wily4_boss"),
    None,
    Some("wily6_boss"),
];
const DEFEATED: [&str; 14] = [
    "heat_defeated",
    "air_defeated",
    "wood_defeated",
    "bubble_defeated",
    "quick_defeated",
    "flash_defeated",
    "metal_defeated",
    "crash_defeated",
    "wily1_defeated",
    "wily2_defeated",
    "wily3_defeated",
    "wily4_defeated",
    "wily5_defeated",
    "wily6_defeated",
];
const REFIGHT: [&str; 8] = [
    "wily5_heat_refight",
    "wily5_air_refight",
    "wily5_wood_refight",
    "wily5_bubble_refight",
    "wily5_quick_refight",
    "wily5_flash_refight",
    "wily5_metal_refight",
    "wily5_crash_refight",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnemyFrame {
    pub health: [u8; ENEMY_SLOTS],
    pub flags: [u8; ENEMY_SLOTS],
    pub ids: [u8; ENEMY_SLOTS],
    pub index: [u8; ENEMY_SLOTS],
}

impl EnemyFrame {
    fn read(ram: &[u8]) -> Self {
        let table = |start: usize| std::array::from_fn(|slot| byte(ram, start + slot));
        let object = OBJECT_SLOTS - ENEMY_SLOTS;
        Self {
            health: table(ENEMY_HEALTH_TABLE),
            flags: table(OBJECT_FLAG_TABLE + object),
            ids: table(OBJECT_ID_TABLE + object),
            index: table(ENEMY_INDEX_TABLE),
        }
    }

    fn damage_to(&self, ram: &[u8]) -> u8 {
        let current = Self::read(ram);
        (0..ENEMY_SLOTS)
            .filter_map(|enemy| {
                let (before, after) = (self.health[enemy], current.health[enemy]);
                let active = self.flags[enemy] & OBJECT_ACTIVE != 0;
                let hit = byte(ram, ENEMY_HIT_TABLE + enemy) != 0;
                let same = self.ids[enemy] == current.ids[enemy]
                    && self.index[enemy] == current.index[enemy];
                let killed = after == 0 && current.ids[enemy] == ENEMY_KILLED_ID;
                (active && hit && (same || killed) && after < before)
                    .then(|| before.saturating_sub(after).min(ENEMY_HIT_CAP))
            })
            .fold(0, u8::saturating_add)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Transition {
    RobotMaster(u8),
    Castle(u8),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2State {
    pub stage: u8,
    pub screen: u8,
    pub room: u8,
    pub x: u8,
    pub y: u8,
    pub health: u8,
    pub weapon_energy: u16,
    pub weapon_energies: [u8; WEAPONS],
    pub platforms: u8,
    pub lives: u8,
    pub player_state: u8,
    pub weapon: u8,
    pub menu: u8,
    pub menu_mode: u8,
    pub weapons_obtained: u8,
    pub items_obtained: u8,
    pub facing_right: bool,
    pub boss_health: u8,
    pub boss_phase: u8,
    pub camera_state: u8,
    pub bank: u8,
    pub current_boss: u8,
    pub refights: u8,
    pub enemy_damage: u8,
    pub boss_intro_frames: u16,
    pub fall_run: u16,
    pub dying_run: u32,
    pub dead: bool,
    pub ending: bool,
    pub arrived: bool,
    pub driving: Option<Transition>,
    pub enemies: EnemyFrame,
}

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn menu_row(ram: &[u8], bank: u8) -> u8 {
    if bank != MENU_BANK {
        return MENU_CLOSED;
    }
    let (page, cursor) = (byte(ram, MENU_PAGE), byte(ram, MENU_CURSOR));
    match MENU_ROWS.get(usize::from(page)) {
        Some(rows) if cursor < *rows => page * MENU_PAGE_ROWS + cursor,
        _ => MENU_UNKNOWN,
    }
}

fn live_platforms(ram: &[u8]) -> u8 {
    (0..OBJECT_SLOTS)
        .filter(|slot| {
            (ITEM_OBJECT_FIRST..=ITEM_OBJECT_LAST).contains(&byte(ram, OBJECT_ID_TABLE + slot))
                && byte(ram, OBJECT_FLAG_TABLE + slot) & OBJECT_ACTIVE != 0
        })
        .fold(0, |count, _| count.saturating_add(1))
}

#[must_use]
pub fn next_stage_after(weapons_obtained: u8) -> u8 {
    ROBOT_MASTER_ORDER
        .into_iter()
        .find(|stage| weapons_obtained & (1 << stage) == 0)
        .unwrap_or(FIRST_WILY_STAGE)
}

#[must_use]
pub fn select_path(stage: u8) -> &'static [u8] {
    match stage {
        0 => &[JOYPAD_LEFT],
        1 => &[JOYPAD_UP],
        2 => &[JOYPAD_RIGHT],
        3 => &[JOYPAD_UP, JOYPAD_LEFT],
        4 => &[JOYPAD_UP, JOYPAD_RIGHT],
        5 => &[JOYPAD_DOWN],
        6 => &[JOYPAD_DOWN, JOYPAD_LEFT],
        7 => &[JOYPAD_DOWN, JOYPAD_RIGHT],
        _ => &[],
    }
}

fn arrival_limit(stage: u8) -> u16 {
    if stage >= FIRST_WILY_STAGE {
        WILY_STAGE_START_WAIT_FRAMES
    } else {
        STAGE_START_WAIT_FRAMES
    }
}

impl Mm2State {
    fn read(ram: &[u8], previous_stage: Option<u8>) -> Self {
        let raw_stage = byte(ram, STAGE);
        let player_state = byte(ram, PLAYER_STATE);
        let boss_phase = byte(ram, BOSS_PHASE);
        let stage = if previous_stage == Some(WILY5_STAGE)
            && raw_stage < FIRST_WILY_STAGE
            && player_state == PLAYER_STATE_TELEPORTING
            && boss_phase == BOSS_PHASE_NONE
        {
            WILY5_STAGE
        } else {
            raw_stage
        };
        let bank = byte(ram, CURRENT_BANK);
        Self {
            stage,
            screen: byte(ram, PLAYER_SCREEN),
            room: byte(ram, LEVEL_ROOM),
            x: byte(ram, PLAYER_X),
            y: byte(ram, PLAYER_Y),
            health: byte(ram, PLAYER_HEALTH),
            weapon_energy: (WEAPON_ENERGY..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
                .map(|address| u16::from(byte(ram, address)))
                .sum(),
            weapon_energies: std::array::from_fn(|index| byte(ram, WEAPON_ENERGY + index)),
            platforms: live_platforms(ram),
            lives: byte(ram, LIVES),
            player_state,
            weapon: byte(ram, SELECTED_WEAPON),
            menu: menu_row(ram, bank),
            menu_mode: byte(ram, MENU_MODE),
            weapons_obtained: byte(ram, WEAPONS_OBTAINED),
            items_obtained: byte(ram, ITEMS_OBTAINED),
            facing_right: byte(ram, OBJECT_FLAG_TABLE) & PLAYER_FACING_RIGHT != 0,
            boss_health: byte(ram, BOSS_HEALTH),
            boss_phase,
            camera_state: byte(ram, CAMERA_STATE),
            bank,
            current_boss: byte(ram, CURRENT_BOSS),
            refights: if stage == WILY5_STAGE {
                byte(ram, REFIGHTS)
            } else {
                0
            },
            enemies: EnemyFrame::read(ram),
            ..Self::default()
        }
    }

    fn fresh(ram: &[u8]) -> Self {
        let state = Self::read(ram, None);
        Self {
            dead: state.is_dead(),
            arrived: state.arrived_at(state.stage),
            ..state
        }
    }

    fn after(previous: &Self, ram: &[u8]) -> Self {
        let mut state = Self::read(ram, Some(previous.stage));
        let steady = state.camera_state != CAMERA_STATE_SCROLLING
            && previous.camera_state != CAMERA_STATE_SCROLLING
            && state.screen == previous.screen;
        let dropped = state.y.wrapping_sub(previous.y);
        state.fall_run = if steady && dropped > 0 && dropped <= FALL_STEP_LIMIT {
            previous.fall_run.saturating_add(u16::from(dropped))
        } else {
            0
        };
        let moved = state.screen != previous.screen || state.stage != previous.stage;
        state.enemy_damage = if moved { 0 } else { previous.enemy_damage }
            .saturating_add(previous.enemies.damage_to(ram))
            .min(ENEMY_DAMAGE_CAP);
        state.boss_intro_frames = if state.boss_intro() {
            previous.boss_intro_frames.saturating_add(1)
        } else {
            0
        };
        state.dying_run = if state.player_state == PLAYER_STATE_DYING {
            previous.dying_run.saturating_add(1)
        } else {
            0
        };
        let escaped = (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&previous.boss_phase)
            && state.screen != previous.screen;
        state.ending = previous.stage == LAST_WILY_STAGE && state.final_stage_cleared();
        state.arrived = previous.arrived && state.arrived_at(state.stage);
        state.dead = previous.dead
            || state.is_dead()
            || state.dying_run >= DYING_FRAMES
            || state.lives < previous.lives
            || escaped
            || state.fall_run > FALL_RUN_LIMIT;
        if !state.dead {
            state.driving = state.transition();
        }
        state
    }

    fn driven(transition: Transition, ram: &[u8]) -> Self {
        let fresh = Self::fresh(ram);
        let arrived = match transition {
            Transition::RobotMaster(stage) => {
                fresh.weapons_obtained & (1 << stage) != 0
                    && fresh.arrived_at(next_stage_after(fresh.weapons_obtained))
            }
            Transition::Castle(stage) => fresh.arrived_at(stage + 1),
        };
        if arrived {
            Self {
                dead: false,
                arrived: true,
                ..fresh
            }
        } else {
            Self {
                dead: false,
                arrived: false,
                driving: Some(transition),
                ..fresh
            }
        }
    }

    fn transition(&self) -> Option<Transition> {
        if self.stage < FIRST_WILY_STAGE && self.boss_phase >= BOSS_PHASE_DEFEATED {
            return Some(Transition::RobotMaster(self.stage));
        }
        ((FIRST_WILY_STAGE..LAST_WILY_STAGE).contains(&self.stage)
            && self.boss_phase == BOSS_PHASE_CLEARED
            && self.last_castle_boss())
        .then_some(Transition::Castle(self.stage))
    }

    #[must_use]
    pub fn robot_masters(&self) -> u8 {
        u8::try_from(self.weapons_obtained.count_ones()).unwrap_or(u8::MAX)
    }

    #[must_use]
    pub fn castles(&self) -> u8 {
        if (FIRST_WILY_STAGE..=LAST_WILY_STAGE).contains(&self.stage) {
            self.stage - FIRST_WILY_STAGE
        } else {
            0
        }
    }

    #[must_use]
    pub fn refights_cleared(&self) -> u8 {
        u8::try_from(self.refights.count_ones()).unwrap_or(u8::MAX)
    }

    fn in_castle(&self) -> bool {
        (FIRST_WILY_STAGE..=LAST_WILY_STAGE).contains(&self.stage)
    }

    fn last_castle_boss(&self) -> bool {
        self.stage != WILY5_STAGE || (self.refights == u8::MAX && self.current_boss == WILY_MACHINE)
    }

    #[must_use]
    pub fn stage_cleared(&self) -> bool {
        if self.stage < FIRST_WILY_STAGE {
            self.boss_phase >= BOSS_PHASE_DEFEATED
        } else {
            self.stage <= LAST_WILY_STAGE
                && self.boss_phase == BOSS_PHASE_CLEARED
                && self.last_castle_boss()
        }
    }

    #[must_use]
    pub fn is_dying(&self) -> bool {
        self.player_state == PLAYER_STATE_DYING
    }

    fn holds(&self, weapon: u8) -> bool {
        let energy = || {
            self.weapon_energies
                .get(usize::from(weapon) - 1)
                .is_some_and(|energy| *energy > 0)
        };
        match weapon {
            0 => true,
            1..=ROBOT_MASTERS => self.weapons_obtained & (1 << (weapon - 1)) != 0 && energy(),
            FIRST_ITEM_WEAPON..=LAST_WEAPON => {
                self.items_obtained & (1 << (weapon - FIRST_ITEM_WEAPON)) != 0 && energy()
            }
            _ => false,
        }
    }

    pub fn switch_targets(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=LAST_WEAPON).filter(|weapon| *weapon != self.weapon && self.holds(*weapon))
    }

    #[must_use]
    pub fn switch_offer(&self) -> u64 {
        if !WeaponSwitch::starts(self.menu, self.playable()) {
            return 0;
        }
        self.switch_targets()
            .fold(0, |offered, weapon| offered | 1 << switch_label(weapon))
    }

    #[must_use]
    pub fn castle_boss_defeated(&self) -> bool {
        self.in_castle() && self.boss_phase >= BOSS_PHASE_DEFEATED && self.last_castle_boss()
    }

    #[must_use]
    pub fn machine_shell_broken(&self) -> bool {
        self.stage == WILY5_STAGE
            && self.current_boss == WILY_MACHINE
            && (BOSS_PHASE_REFILL..BOSS_PHASE_DEFEATED).contains(&self.boss_phase)
    }

    #[must_use]
    pub fn refight_boss(&self) -> u8 {
        if self.stage == WILY5_STAGE
            && (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&self.boss_phase)
        {
            self.current_boss
        } else {
            NO_REFIGHT_BOSS
        }
    }

    #[must_use]
    pub fn posture(&self) -> u8 {
        match self.player_state {
            PLAYER_STATE_AIRBORNE | PLAYER_STATE_HIT => 1,
            PLAYER_STATE_LADDER | PLAYER_STATE_LADDER_TOP => 2,
            _ => 0,
        }
    }

    #[must_use]
    pub fn boss_damage(&self) -> u8 {
        if self.machine_shell_broken() && self.boss_phase == BOSS_PHASE_REFILL {
            0
        } else if self.boss_phase >= BOSS_PHASE_DEFEATED {
            FULL_BOSS_HEALTH
        } else if self.boss_phase >= BOSS_PHASE_FIGHTING && self.boss_health > 0 {
            FULL_BOSS_HEALTH.saturating_sub(self.boss_health)
        } else {
            0
        }
    }

    #[must_use]
    pub fn boss_fight_underway(&self) -> bool {
        self.boss_phase != BOSS_PHASE_NONE && self.boss_phase < BOSS_PHASE_DEFEATED
    }

    #[must_use]
    pub fn boss_intro(&self) -> bool {
        self.boss_fight_underway() && self.boss_health == 0
    }

    #[must_use]
    pub fn playable(&self) -> bool {
        self.bank == GAMEPLAY_BANK
            && self.health > 0
            && (PLAYER_STATE_HIT..=PLAYER_STATE_LADDER_TOP).contains(&self.player_state)
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.health == 0
            || (self.player_state == PLAYER_STATE_FALLEN && self.y >= BELOW_PLAY_AREA_Y)
    }

    #[must_use]
    pub fn arrived_at(&self, stage: u8) -> bool {
        self.stage == stage
            && self.bank == GAMEPLAY_BANK
            && self.player_state == PLAYER_STATE_STANDING
            && self.health == FULL_HEALTH
    }

    fn final_stage_cleared(&self) -> bool {
        self.stage == FINAL_STAGE
            && self.boss_phase == BOSS_PHASE_CLEARED
            && self.weapons_obtained == u8::MAX
    }

    #[must_use]
    pub fn lowest_energies(&self) -> [u8; 3] {
        let mut energies = self.weapon_energies;
        energies.sort_unstable();
        [energies[0], energies[1], energies[2]]
    }

    fn at_stage_select(&self) -> bool {
        self.menu_mode == MENU_MODE_STAGE_SELECT
    }

    fn reached(&self, mark: usize) -> bool {
        let fresh_boss = self.boss_damage() == 0
            && self.boss_health > 0
            && self.boss_fight_underway()
            && self.boss_phase >= BOSS_PHASE_FIGHTING;
        let here = usize::from(self.stage);
        let in_play = here < ENTERED.len() && self.playable();
        match Mark::of(mark) {
            Mark::Ending => self.ending,
            _ if self.ending => mark == Mark::Defeated(usize::from(LAST_WILY_STAGE)).index(),
            _ if self.dead => false,
            Mark::Defeated(stage) if stage < usize::from(ROBOT_MASTERS) => {
                self.weapons_obtained & (1 << stage) != 0
            }
            Mark::Defeated(stage) => {
                self.weapons_obtained == u8::MAX
                    && stage < usize::from(self.stage.min(FIRST_WILY_STAGE + 6))
            }
            Mark::Entered(stage) => in_play && stage == here && self.arrived,
            Mark::Boss(stage) => {
                in_play && stage == here && here != usize::from(WILY5_STAGE) && fresh_boss
            }
            Mark::Refight(bit) => {
                in_play && here == usize::from(WILY5_STAGE) && self.refights & (1 << bit) != 0
            }
            Mark::Machine => {
                in_play
                    && here == usize::from(WILY5_STAGE)
                    && self.current_boss == WILY_MACHINE
                    && fresh_boss
                    && !self.machine_shell_broken()
            }
            Mark::Shell => {
                in_play && here == usize::from(WILY5_STAGE) && self.machine_shell_broken()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mark {
    Entered(usize),
    Boss(usize),
    Defeated(usize),
    Refight(usize),
    Machine,
    Shell,
    Ending,
}

const MARK_STAGES: usize = 14;
const MARKS: usize = 3 * MARK_STAGES + 8 + 3;

impl Mark {
    fn of(index: usize) -> Self {
        match index {
            i if i < MARK_STAGES => Self::Entered(i),
            i if i < 2 * MARK_STAGES => Self::Boss(i - MARK_STAGES),
            i if i < 3 * MARK_STAGES => Self::Defeated(i - 2 * MARK_STAGES),
            i if i < 3 * MARK_STAGES + 8 => Self::Refight(i - 3 * MARK_STAGES),
            i if i == 3 * MARK_STAGES + 8 => Self::Machine,
            i if i == 3 * MARK_STAGES + 9 => Self::Shell,
            _ => Self::Ending,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Entered(i) => i,
            Self::Boss(i) => MARK_STAGES + i,
            Self::Defeated(i) => 2 * MARK_STAGES + i,
            Self::Refight(i) => 3 * MARK_STAGES + i,
            Self::Machine => 3 * MARK_STAGES + 8,
            Self::Shell => 3 * MARK_STAGES + 9,
            Self::Ending => 3 * MARK_STAGES + 10,
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Self::Entered(i) => Some(ENTERED[i]),
            Self::Boss(i) => BOSS[i],
            Self::Defeated(i) => Some(DEFEATED[i]),
            Self::Refight(i) => Some(REFIGHT[i]),
            Self::Machine => Some("wily5_machine"),
            Self::Shell => Some("wily5_machine_shell"),
            Self::Ending => Some("ending"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Drive {
    Award { stage: u8, chunks: u16 },
    Settle { chord: u8 },
    Walk { round: u8, chord: u8 },
    Path { stage: u8, chord: u8 },
    Arrive { stage: u8, waited: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mm2Move {
    Drive(Drive),
    Switch(WeaponSwitch),
}

const fn switch_label(weapon: u8) -> u16 {
    weapon as u16 + 1
}

const fn switches() -> [(u16, Play<Mm2Move>); SWITCH_OPTIONS] {
    let mut options = [(CHORD_LABEL, Play::Chord); SWITCH_OPTIONS];
    let mut weapon = 0;
    while weapon <= LAST_WEAPON {
        options[weapon as usize + 1] = (
            switch_label(weapon),
            Play::Move(Mm2Move::Switch(WeaponSwitch::to_row(weapon_menu_row(
                weapon,
            )))),
        );
        weapon += 1;
    }
    options
}

impl Drive {
    #[must_use]
    pub fn begin(transition: Transition) -> Self {
        match transition {
            Transition::RobotMaster(stage) => Self::Award { stage, chunks: 0 },
            Transition::Castle(stage) => Self::Arrive {
                stage: stage + 1,
                waited: 0,
            },
        }
    }
}

fn step(buttons: u8, hold: u8, rest: Drive) -> Option<Step<Drive>> {
    Some(Step {
        chord: ButtonChord::new(buttons, hold),
        rest: Some(rest),
    })
}

fn walk_round() -> Vec<ButtonChord> {
    let mut round = vec![
        ButtonChord::new(JOYPAD_DOWN, PRESS_FRAMES),
        ButtonChord::new(0, CURSOR_SETTLE_FRAMES),
        ButtonChord::new(JOYPAD_START, PRESS_FRAMES),
    ];
    round.extend(idle(MENU_SETTLE_FRAMES));
    round
}

fn idle(frames: u32) -> Vec<ButtonChord> {
    let mut chords = Vec::new();
    let mut left = frames;
    while left > 0 {
        let hold = left.min(u32::from(MAX_HOLD_FRAMES));
        chords.push(ButtonChord::new(
            0,
            u8::try_from(hold).unwrap_or(MAX_HOLD_FRAMES),
        ));
        left -= hold;
    }
    chords
}

#[must_use]
pub fn power_on_walk() -> Vec<ButtonChord> {
    let mut tape = idle(TITLE_SETTLE_FRAMES);
    for _ in 0..BOOT_PRESSES {
        tape.push(ButtonChord::new(JOYPAD_START, PRESS_FRAMES));
        tape.extend(idle(MENU_SETTLE_FRAMES));
    }
    tape
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MegaMan2 {
    stage: Option<u8>,
}

impl MegaMan2 {
    pub const WHOLE_GAME: Self = Self { stage: None };

    pub fn at_stage(stage: u8) -> Result<Self, String> {
        if stage < ROBOT_MASTERS {
            Ok(Self { stage: Some(stage) })
        } else {
            Err(format!(
                "Mega Man 2 stage {stage} is not a Robot Master stage; the stages are 0 to {}",
                ROBOT_MASTERS - 1
            ))
        }
    }

    #[must_use]
    pub fn stage(self) -> Option<u8> {
        self.stage
    }

    fn start_stage(self) -> u8 {
        self.stage.unwrap_or(CRASH)
    }

    #[must_use]
    pub fn strategy(self) -> String {
        match self.stage {
            None => Self::STRATEGY.to_owned(),
            Some(stage) => Self::STRATEGY.replacen(
                "goal = \"ending\"",
                &format!("goal = \"{}\"", DEFEATED[usize::from(stage)]),
                1,
            ),
        }
    }

    pub fn strategy_from(self, given: &str) -> Result<String, Box<dyn Error>> {
        let Some(stage) = self.stage else {
            return Ok(given.to_owned());
        };
        let goal = DEFEATED[usize::from(stage)];
        let mut document: DocumentMut = given.parse()?;
        match document.get_mut("goal").and_then(Item::as_value_mut) {
            Some(value) => {
                let decor = value.decor().clone();
                *value = Value::from(goal);
                *value.decor_mut() = decor;
            }
            None => document["goal"] = toml_edit::value(goal),
        }
        let text = document.to_string();
        if Strategy::parse(&text)?.define.contains_key(goal) {
            return Err(format!(
                "the strategy defines {goal}, the goal of Mega Man 2 stage {stage}, so the stage start would not win at its Robot Master"
            )
            .into());
        }
        Ok(text)
    }

    pub fn drive(self, mv: Drive, state: &Mm2State) -> Option<Step<Drive>> {
        match mv {
            Drive::Award { stage, chunks } => {
                if state.weapons_obtained & (1 << stage) != 0 {
                    self.drive(Drive::Settle { chord: 0 }, state)
                } else if chunks >= AWARD_WAIT_CHUNKS {
                    None
                } else {
                    step(
                        0,
                        MAX_HOLD_FRAMES,
                        Drive::Award {
                            stage,
                            chunks: chunks + 1,
                        },
                    )
                }
            }
            Drive::Settle { chord } => match idle(AWARD_SETTLE_FRAMES).get(usize::from(chord)) {
                Some(settle) => step(0, settle.hold_frames, Drive::Settle { chord: chord + 1 }),
                None => self.drive(Drive::Walk { round: 0, chord: 0 }, state),
            },
            Drive::Walk { round: _, chord: 0 } if state.at_stage_select() => self.drive(
                Drive::Path {
                    stage: next_stage_after(state.weapons_obtained),
                    chord: 0,
                },
                state,
            ),
            Drive::Walk { round, chord: 0 } if round >= STAGE_SELECT_WALK_ROUNDS => None,
            Drive::Walk { round, chord } => match walk_round().get(usize::from(chord)) {
                Some(press) => step(
                    press.buttons,
                    press.hold_frames,
                    Drive::Walk {
                        round,
                        chord: chord + 1,
                    },
                ),
                None => self.drive(
                    Drive::Walk {
                        round: round + 1,
                        chord: 0,
                    },
                    state,
                ),
            },
            Drive::Path { stage, chord } => {
                let path = select_path(stage);
                match path.get(usize::from(chord / 2)) {
                    Some(direction) if chord % 2 == 0 => step(
                        *direction,
                        PRESS_FRAMES,
                        Drive::Path {
                            stage,
                            chord: chord + 1,
                        },
                    ),
                    Some(_) => step(
                        0,
                        CURSOR_SETTLE_FRAMES,
                        Drive::Path {
                            stage,
                            chord: chord + 1,
                        },
                    ),
                    None => step(
                        JOYPAD_START,
                        PRESS_FRAMES,
                        Drive::Arrive { stage, waited: 0 },
                    ),
                }
            }
            Drive::Arrive { stage, waited } => {
                if state.arrived_at(stage) || waited >= arrival_limit(stage) {
                    None
                } else {
                    step(
                        0,
                        1,
                        Drive::Arrive {
                            stage,
                            waited: waited + 1,
                        },
                    )
                }
            }
        }
    }

    #[must_use]
    pub fn drive_frames(mv: Drive) -> u64 {
        match mv {
            Drive::Arrive { stage, .. } => u64::from(arrival_limit(stage)),
            _ => DRIVE_FRAMES,
        }
    }
}

fn value(name: &'static str, value: i64, min: i64, max: i64) -> Assertion {
    Assertion::Value {
        name,
        value: Number::new(value, min, max),
    }
}

fn byte_value(name: &'static str, value: u8) -> Assertion {
    Assertion::Value {
        name,
        value: Number::byte(value),
    }
}

fn flag(name: &'static str, value: bool) -> Assertion {
    Assertion::Value {
        name,
        value: Number::flag(value),
    }
}

impl Game for MegaMan2 {
    type State = Mm2State;
    type Move = Mm2Move;

    const NAME: &'static str = "mm2";
    const STRATEGY: &'static str = include_str!("mega_man.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: None,
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: LONG_HOLD_FRAMES,
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = power_on_walk();
        let stage = self.start_stage();
        for direction in select_path(stage) {
            tape.push(ButtonChord::new(*direction, PRESS_FRAMES));
            tape.push(ButtonChord::new(0, CURSOR_SETTLE_FRAMES));
        }
        tape.push(ButtonChord::new(JOYPAD_START, PRESS_FRAMES));
        tape.extend(idle(STAGE_ARRIVAL_WAIT_FRAMES[usize::from(stage)]));
        tape
    }

    fn genesis(&self, state: &Mm2State) -> Result<(), String> {
        let stage = self.start_stage();
        if state.arrived_at(stage) && state.weapons_obtained == 0 && !state.dead {
            Ok(())
        } else {
            Err(format!(
                "Mega Man 2 setup did not reach the first playable frame of stage {stage}: {state:?}"
            ))
        }
    }

    fn rooted_genesis(&self, state: &Mm2State) -> Result<(), String> {
        if state.arrived && !state.dead && state.driving.is_none() {
            Ok(())
        } else {
            Err(format!(
                "Mega Man 2 root input did not end in play after a stage arrival: {state:?}"
            ))
        }
    }

    fn genesis_id(&self) -> String {
        match self.stage {
            None => format!("{}-whole-game-crash-v1", Self::NAME),
            Some(stage) => format!("{}-stage-{stage}-v1", Self::NAME),
        }
    }

    fn decode(&self, _: Option<&Mm2State>, previous: Option<&Mm2State>, ram: &[u8]) -> Mm2State {
        match previous {
            None => Mm2State::fresh(ram),
            Some(previous) => match previous.driving {
                Some(transition) => Mm2State::driven(transition, ram),
                None => Mm2State::after(previous, ram),
            },
        }
    }

    fn declare(&self, state: &Mm2State, out: &mut impl Declarations) {
        let [low_1, low_2, low_3] = state.lowest_energies();
        out.declare(value(
            "robot_masters",
            i64::from(state.robot_masters()),
            0,
            i64::from(ROBOT_MASTERS),
        ));
        out.declare(value("castles", i64::from(state.castles()), 0, 5));
        out.declare(value(
            "refights_cleared",
            i64::from(state.refights_cleared()),
            0,
            8,
        ));
        out.declare(flag("castle_boss_defeated", state.castle_boss_defeated()));
        out.declare(flag("machine_shell", state.machine_shell_broken()));
        out.declare(byte_value("stage", state.stage));
        out.declare(byte_value("screen", state.screen));
        out.declare(byte_value("room", state.room));
        out.declare(value(
            "boss_damage",
            i64::from(state.boss_damage()),
            0,
            i64::from(FULL_BOSS_HEALTH),
        ));
        out.declare(value(
            "enemy_damage",
            i64::from(state.enemy_damage),
            0,
            i64::from(ENEMY_DAMAGE_CAP),
        ));
        out.declare(byte_value("x", state.x));
        out.declare(byte_value("y", state.y));
        out.declare(value("posture", i64::from(state.posture()), 0, 2));
        out.declare(value(
            "platforms",
            i64::from(state.platforms),
            0,
            i64::try_from(OBJECT_SLOTS).unwrap_or(i64::MAX),
        ));
        out.declare(flag("menu_open", state.menu != MENU_CLOSED));
        out.declare(byte_value("refights", state.refights));
        out.declare(byte_value("refight_boss", state.refight_boss()));
        out.declare(value(
            "boss_intro_frames",
            i64::from(state.boss_intro_frames),
            0,
            i64::from(u16::MAX),
        ));
        out.declare(byte_value("weapon", state.weapon));
        out.declare(byte_value("menu", state.menu));
        out.declare(byte_value("health", state.health));
        out.declare(value(
            "weapon_energy",
            i64::from(state.weapon_energy),
            0,
            MAX_WEAPON_ENERGY,
        ));
        out.declare(byte_value("lowest_energy_1", low_1));
        out.declare(byte_value("lowest_energy_2", low_2));
        out.declare(byte_value("lowest_energy_3", low_3));
        out.declare(byte_value("lives", state.lives));
        out.declare(byte_value("boss_phase", state.boss_phase));
        out.declare(byte_value("weapons_obtained", state.weapons_obtained));
        out.declare(flag("playable", state.playable()));
        out.declare(flag("dead", state.dead));
        for mark in 0..MARKS {
            if let Some(name) = Mark::of(mark).name() {
                out.declare(Assertion::Sometimes {
                    name,
                    condition: state.reached(mark),
                });
            }
        }
    }

    fn moves(&self, state: &Mm2State) -> Moves<Mm2Move> {
        match state.driving {
            Some(transition) => Moves::forced(Mm2Move::Drive(Drive::begin(transition))),
            None => match state.switch_offer() {
                0 => Moves::none(),
                offered => Moves::offer(Offer {
                    site: WEAPON_SITE,
                    default: CHORD_LABEL,
                    options: &SWITCHES,
                    offered,
                }),
            },
        }
    }

    fn move_sites(&self) -> Vec<ChoiceSite> {
        let labels = ["chord", "p"]
            .into_iter()
            .map(str::to_owned)
            .chain((1..=ROBOT_MASTERS).map(|weapon| format!("weapon_{weapon}")))
            .chain(
                (FIRST_ITEM_WEAPON..=LAST_WEAPON)
                    .map(|item| format!("item_{}", item - ROBOT_MASTERS)),
            );
        vec![ChoiceSite::new("weapon", labels).with_contrary(CERTAIN / SWITCH_ODDS as u32)]
    }

    fn play(&self, mv: Mm2Move, state: &Mm2State) -> Option<Step<Mm2Move>> {
        match mv {
            Mm2Move::Drive(drive) => self.drive(drive, state).map(|step| Step {
                chord: step.chord,
                rest: step.rest.map(Mm2Move::Drive),
            }),
            Mm2Move::Switch(mut switch) => {
                switch.step(state.menu, state.playable()).map(|chord| Step {
                    chord,
                    rest: Some(Mm2Move::Switch(switch)),
                })
            }
        }
    }

    fn move_frames(&self, mv: Mm2Move) -> u64 {
        match mv {
            Mm2Move::Drive(drive) => Self::drive_frames(drive),
            Mm2Move::Switch(_) => u64::from(SWITCH_FRAME_LIMIT),
        }
    }

    fn answer_frames(site: u16, label: u16) -> u64 {
        if site == WEAPON_SITE && label != CHORD_LABEL && usize::from(label) < SWITCH_OPTIONS {
            SWITCH_PLANNED_FRAMES
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::Lowered;

    fn drive_frames(mut mv: Drive, state: &Mm2State) -> (u64, u8) {
        let mut frames = 0;
        let mut longest = 0;
        while let Some(step) = MegaMan2::WHOLE_GAME.drive(mv, state) {
            frames += u64::from(step.chord.hold_frames);
            longest = longest.max(step.chord.hold_frames);
            match step.rest {
                Some(rest) => mv = rest,
                None => break,
            }
        }
        (frames, longest)
    }

    #[test]
    fn the_strategy_lowers_against_the_decoder() {
        Lowered::new(MegaMan2::WHOLE_GAME, MegaMan2::STRATEGY).unwrap();
    }

    #[test]
    fn a_root_must_end_in_play_after_an_arrival_and_outside_a_drive() {
        let arrived = Mm2State {
            stage: FIRST_WILY_STAGE + 3,
            arrived: true,
            ..Mm2State::default()
        };
        assert!(MegaMan2::WHOLE_GAME.rooted_genesis(&arrived).is_ok());
        for state in [
            Mm2State {
                arrived: false,
                ..arrived
            },
            Mm2State {
                dead: true,
                ..arrived
            },
            Mm2State {
                driving: Some(Transition::Castle(FIRST_WILY_STAGE)),
                ..arrived
            },
        ] {
            assert!(MegaMan2::WHOLE_GAME.rooted_genesis(&state).is_err());
        }
    }

    #[test]
    fn the_setup_tape_is_the_genesis_walk_of_2254_frames() {
        let frames: u32 = MegaMan2::WHOLE_GAME
            .setup()
            .iter()
            .map(|chord| u32::from(chord.hold_frames))
            .sum();
        assert_eq!(frames, 2_254);
    }

    #[test]
    fn the_crash_stage_start_plays_the_whole_game_setup_under_its_own_identity() {
        let crash = MegaMan2::at_stage(CRASH).unwrap();
        assert_eq!(crash.setup(), MegaMan2::WHOLE_GAME.setup());
        assert_eq!(MegaMan2::WHOLE_GAME.genesis_id(), "mm2-whole-game-crash-v1");
        assert_eq!(crash.genesis_id(), "mm2-stage-7-v1");
    }

    #[test]
    fn each_stage_start_wins_at_its_robot_master() {
        for stage in 0..ROBOT_MASTERS {
            let game = MegaMan2::at_stage(stage).unwrap();
            let strategy = game.strategy();
            let goal = format!("goal = \"{}\"", DEFEATED[usize::from(stage)]);
            assert!(strategy.starts_with(&goal), "{strategy}");
            Lowered::new(game, &strategy).unwrap();
            assert_eq!(game.stage(), Some(stage));
        }
        assert!(MegaMan2::at_stage(ROBOT_MASTERS).is_err());
        assert_eq!(MegaMan2::WHOLE_GAME.strategy(), MegaMan2::STRATEGY);
    }

    #[test]
    fn a_given_strategy_at_a_stage_start_wins_at_that_robot_master() {
        let given = MegaMan2::STRATEGY.replacen("capacity = 1", "capacity = 2", 1);
        assert_eq!(MegaMan2::WHOLE_GAME.strategy_from(&given).unwrap(), given);
        for stage in 0..ROBOT_MASTERS {
            let game = MegaMan2::at_stage(stage).unwrap();
            let goal = format!("goal = \"{}\"", DEFEATED[usize::from(stage)]);
            let strategy = game.strategy_from(&given).unwrap();
            assert_eq!(strategy, given.replacen("goal = \"ending\"", &goal, 1));
            assert_eq!(game.strategy_from(&strategy).unwrap(), strategy);
            Lowered::new(game, &strategy).unwrap();
        }
    }

    #[test]
    fn a_stage_start_changes_only_the_goal_line_of_every_shipped_strategy() {
        let heat = MegaMan2::at_stage(0).unwrap();
        let shipped = [
            include_str!("../donkey_kong/donkey_kong.toml"),
            include_str!("../famidash/famidash.toml"),
            include_str!("../famidash/lucky_draw.toml"),
            include_str!("../ice_climbers/ice_climbers.toml"),
            include_str!("../joust/joust.toml"),
            include_str!("../metroid/metroid.toml"),
            include_str!("../metroid/position.toml"),
            include_str!("../mm2/mega_man.toml"),
            include_str!("../pac_man/pac_man.toml"),
            include_str!("../punchout/punchout.toml"),
            include_str!("../smb/smb.toml"),
            include_str!("../smb/smb_warps.toml"),
            include_str!("../twin_dragons/twin_dragons.toml"),
            include_str!("../zelda/zelda.toml"),
        ];
        for (index, given) in shipped.into_iter().enumerate() {
            let edited = heat.strategy_from(given).unwrap();
            let mut parsed = Strategy::parse(&edited).unwrap();
            assert_eq!(parsed.goal.as_deref(), Some("heat_defeated"), "{index}");
            let original = Strategy::parse(given).unwrap();
            parsed.goal.clone_from(&original.goal);
            assert_eq!(parsed, original, "{index}");
            let before: Vec<&str> = given.lines().collect();
            let after: Vec<&str> = edited.lines().collect();
            assert_eq!(before.len(), after.len(), "{index}");
            let changed: Vec<usize> = (0..before.len())
                .filter(|&line| before[line] != after[line])
                .collect();
            assert_eq!(changed.len(), 1, "{index}");
            assert!(before[changed[0]].starts_with("goal = "), "{index}");
            assert_eq!(after[changed[0]], "goal = \"heat_defeated\"", "{index}");
        }
    }

    #[test]
    fn a_stage_start_sets_the_top_level_goal_of_any_strategy_text() {
        let heat = MegaMan2::at_stage(0).unwrap();
        let tail = "capacity = 1\ntier = [\"robot_masters\"]\n";
        for (given, edited) in [
            (
                "define.note = '''\ngoal = \"example\"\n'''\ngoal = \"ending\"\n",
                "define.note = '''\ngoal = \"example\"\n'''\ngoal = \"heat_defeated\"\n",
            ),
            (
                "'goal' = \"ending\" # the win\n",
                "'goal' = \"heat_defeated\" # the win\n",
            ),
            ("goal = '''\nending '''\n", "goal = \"heat_defeated\"\n"),
            ("", "goal = \"heat_defeated\"\n"),
        ] {
            let strategy = heat.strategy_from(&format!("{given}{tail}")).unwrap();
            let first = if given.is_empty() {
                format!("{tail}{edited}")
            } else {
                format!("{edited}{tail}")
            };
            assert_eq!(strategy, first, "{given:?}");
            assert_eq!(
                Strategy::parse(&strategy).unwrap().goal.as_deref(),
                Some("heat_defeated")
            );
        }
        let error = heat
            .strategy_from(
                "goal = \"ending\"\ndefine.heat_defeated = \"ending\"\ncapacity = 1\ntier = [\"robot_masters\"]\n",
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("the strategy defines heat_defeated"),
            "{error}"
        );
    }

    #[test]
    fn a_drive_that_never_reaches_stage_select_ends_inside_its_frame_bound() {
        let begin = Drive::begin(Transition::RobotMaster(CRASH));
        let awarded = Mm2State {
            weapons_obtained: 1 << CRASH,
            ..Mm2State::default()
        };
        let (frames, longest) = drive_frames(begin, &awarded);
        assert!(longest <= MAX_HOLD_FRAMES);
        assert!(frames <= MegaMan2::drive_frames(begin));
        let (waited, _) = drive_frames(begin, &Mm2State::default());
        assert_eq!(
            waited,
            u64::from(AWARD_WAIT_CHUNKS) * u64::from(MAX_HOLD_FRAMES)
        );
    }

    #[test]
    fn a_fresh_decode_of_blank_memory_drives_nothing() {
        let state = MegaMan2::WHOLE_GAME.decode(None, None, &[0; machine::nes::WRAM_SIZE]);
        assert!(state.driving.is_none());
    }

    #[test]
    fn each_switch_label_plays_the_switch_to_its_weapon_row() {
        for weapon in 0..=LAST_WEAPON {
            let label = switch_label(weapon);
            assert_eq!(
                SWITCHES[usize::from(label)],
                (
                    label,
                    Play::Move(Mm2Move::Switch(WeaponSwitch::to_row(weapon_menu_row(
                        weapon
                    ))))
                )
            );
        }
        assert_eq!(SWITCHES[0], (CHORD_LABEL, Play::Chord));
        assert_eq!(
            MegaMan2::WHOLE_GAME.move_sites()[0].labels.len(),
            SWITCHES.len()
        );
    }

    #[test]
    fn the_weapon_offer_needs_a_target_and_a_menu_that_can_open() {
        let playing = Mm2State {
            bank: GAMEPLAY_BANK,
            health: FULL_HEALTH,
            player_state: PLAYER_STATE_STANDING,
            menu: MENU_CLOSED,
            weapon: 0,
            ..Mm2State::default()
        };
        assert_eq!(MegaMan2::WHOLE_GAME.moves(&playing).offer, None);
        let armed = Mm2State {
            weapons_obtained: 1,
            weapon_energies: [28; WEAPONS],
            ..playing
        };
        let offer = MegaMan2::WHOLE_GAME.moves(&armed).offer.expect("an offer");
        assert_eq!(offer.labels(), [CHORD_LABEL, switch_label(1)]);
        assert_eq!(
            MegaMan2::WHOLE_GAME
                .moves(&Mm2State {
                    menu: MENU_UNKNOWN,
                    ..armed
                })
                .offer,
            None
        );
    }

    #[test]
    fn a_switch_answer_costs_the_switch_plan() {
        assert_eq!(
            MegaMan2::answer_frames(WEAPON_SITE, 1),
            SWITCH_PLANNED_FRAMES
        );
        assert_eq!(MegaMan2::answer_frames(WEAPON_SITE, CHORD_LABEL), 1);
    }
}
