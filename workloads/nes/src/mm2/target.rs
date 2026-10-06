// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine, VideoFrame},
};
use serde::{Deserialize, Serialize};

use crate::{
    nes_backend::NesBackend,
    target::{ExitKind, Target},
};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const CAMERA_STATE: usize = 0x1b;
const CAMERA_STATE_SCROLLING: u8 = 0x80;
const FALL_STEP_LIMIT: u8 = 64;
const FALL_RUN_LIMIT: u16 = 256;
const ENEMY_HEALTH_TABLE_START: usize = 0x6d0;
const ENEMY_HIT_CAP: u8 = 4;
const ENEMY_DAMAGE_CAP: u8 = 24;
const ENEMY_INDEX_TABLE: usize = 0x100;
const ENEMY_HIT_TABLE: usize = 0x110;
const ENEMY_SLOTS: usize = 16;
const ENEMY_KILLED_ID: u8 = 0x06;
const STAGE: usize = 0x2a;
const PLAYER_STATE: usize = 0x2c;
const WEAPONS_OBTAINED: usize = 0x9a;
const LIVES: usize = 0xa8;
const PLAYER_SCREEN: usize = 0x440;
const LEVEL_ROOM: usize = 0x20;
const CURRENT_BANK: usize = 0x29;
const MENU_BANK: u8 = 0x0d;
const GAMEPLAY_BANK: u8 = 0x0e;
const SELECTED_WEAPON: usize = 0xa9;
const MENU_CURSOR: usize = 0xfd;
const MENU_PAGE: usize = 0xfe;
const MENU_ROWS: [u8; 2] = [8, 7];
const MENU_PAGE_ROWS: u8 = 8;
pub const MENU_CLOSED: u8 = 0xff;
const MENU_UNKNOWN: u8 = 0xfe;
const CURRENT_BOSS: usize = 0xb3;
const REFIGHTS: usize = 0xbc;
const WILY_MACHINE: u8 = 12;
pub const NO_REFIGHT_BOSS: u8 = 0xff;
const BOOBEAM_STAGE: u8 = 11;
const WILY5_STAGE: u8 = 12;
const FINAL_STAGE: u8 = 14;
const BOOBEAM_TRAP_ID: u8 = 0x6d;
const BOOBEAM_BARRIER_ID: u8 = 0x57;
const DYING_FRAMES: u32 = 30;
const PLAYER_X: usize = 0x460;
const PLAYER_Y: usize = 0x4a0;
const PLAYER_HEALTH: usize = 0x6c0;
const WEAPON_ENERGY: usize = 0x9c;
const WEAPON_ENERGY_BYTES: usize = 12;
const OBJECT_ID_TABLE: usize = 0x400;
const OBJECT_FLAG_TABLE: usize = 0x420;
const OBJECT_SLOTS: usize = 0x20;
const OBJECT_X_TABLE: usize = 0x460;
const OBJECT_Y_TABLE: usize = 0x4a0;
const TARGET_GRID_CELL: u8 = 32;
const TARGET_GRID_COLUMNS: u8 = 8;
const OBJECT_ACTIVE: u8 = 0x80;
const ITEM_OBJECT_FIRST: u8 = 0x38;
const ITEM_OBJECT_LAST: u8 = 0x3a;
const BOSS_HEALTH: usize = 0x6c1;
const BOSS_PHASE: usize = 0xb1;

const BOSS_PHASE_FIGHTING: u8 = 0x02;
const BOSS_PHASE_NONE: u8 = 0x00;
const BOSS_PHASE_REFILL: u8 = 0x04;
pub const BOSS_PHASE_DEFEATED: u8 = 0xfe;
const BOSS_PHASE_CLEARED: u8 = 0xff;
pub const FULL_BOSS_HEALTH: u8 = 28;

const PLAYER_STATE_STANDING: u8 = 0x03;
const PLAYER_STATE_HIT: u8 = 0x02;
const PLAYER_STATE_AIRBORNE: u8 = 0x06;
const PLAYER_STATE_LADDER: u8 = 0x09;
const PLAYER_STATE_LADDER_TOP: u8 = 0x0a;
const PLAYER_STATE_TELEPORTING: u8 = 0x0b;
pub const POSTURE_GROUNDED: u8 = 0;
pub const POSTURE_AIRBORNE: u8 = 1;
pub const POSTURE_LADDER: u8 = 2;
const PLAYER_STATE_FALLEN: u8 = 0x01;
const PLAYER_STATE_DYING: u8 = 0x00;
const BELOW_PLAY_AREA_Y: u8 = 0xe0;
pub const FULL_HEALTH: u8 = 28;

const JOYPAD_START: u8 = 1 << 3;
const JOYPAD_UP: u8 = 1 << 4;
const JOYPAD_DOWN: u8 = 1 << 5;
const JOYPAD_LEFT: u8 = 1 << 6;
const JOYPAD_RIGHT: u8 = 1 << 7;

pub const MM2_STAGE_COUNT: u8 = 8;
pub const MM2_FIRST_WILY_STAGE: u8 = 8;
const MM2_LAST_WILY_STAGE: u8 = 13;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2Stage(u8);

impl Mm2Stage {
    pub fn from_number(number: u8) -> Result<Self, MachineError> {
        if number <= MM2_LAST_WILY_STAGE {
            Ok(Self(number))
        } else {
            Err(MachineError::Backend(format!(
                "Mega Man 2 stage must be 0..={MM2_LAST_WILY_STAGE}, got {number}"
            )))
        }
    }

    #[must_use]
    pub fn is_wily(self) -> bool {
        self.0 >= MM2_FIRST_WILY_STAGE
    }

    pub fn parse(text: &str) -> Result<Self, MachineError> {
        if let Ok(number) = text.parse::<u8>() {
            return Self::from_number(number);
        }
        let wanted = text.to_ascii_lowercase();
        (0..=MM2_LAST_WILY_STAGE)
            .map(Self)
            .find(|stage| stage.name() == wanted)
            .ok_or_else(|| MachineError::Backend(format!("unknown Mega Man 2 stage {text:?}")))
    }

    #[must_use]
    pub fn number(self) -> u8 {
        self.0
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "heat",
            1 => "air",
            2 => "wood",
            3 => "bubble",
            4 => "quick",
            5 => "flash",
            6 => "metal",
            7 => "crash",
            8 => "wily1",
            9 => "wily2",
            10 => "wily3",
            11 => "wily4",
            12 => "wily5",
            _ => "wily6",
        }
    }

    fn select_path(self) -> &'static [u8] {
        match self.0 {
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
}

impl Default for Mm2Stage {
    fn default() -> Self {
        Self(6)
    }
}

pub const ROBOT_MASTER_ORDER: [Mm2Stage; 8] = [
    Mm2Stage(7),
    Mm2Stage(5),
    Mm2Stage(6),
    Mm2Stage(1),
    Mm2Stage(3),
    Mm2Stage(0),
    Mm2Stage(2),
    Mm2Stage(4),
];

#[must_use]
pub fn next_stage_after(weapons_obtained: u8) -> Mm2Stage {
    ROBOT_MASTER_ORDER
        .into_iter()
        .find(|stage| weapons_obtained & (1 << stage.number()) == 0)
        .unwrap_or(Mm2Stage(MM2_FIRST_WILY_STAGE))
}

pub type Mm2Input = crate::search::archive::Input<ButtonChord>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2MechanicalState {
    pub stage: u8,
    pub screen: u8,
    pub room: u8,
    pub x: u8,
    pub y: u8,
    pub health: u8,
    pub weapon_energy: u16,
    pub equipped_energy: u8,
    pub platforms: u8,
    pub lives: u8,
    pub player_state: u8,
    pub weapon: u8,
    pub menu: u8,
    pub weapons_obtained: u8,
    pub boss_health: u8,
    pub boss_phase: u8,
    pub camera_state: u8,
    pub enemy_damage: u8,
    pub bank: u8,
    pub current_boss: u8,
    pub refights: u8,
    pub boobeam_targets: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2Tier {
    pub robot_masters: u8,
    pub castles: u8,
    pub refights: u8,
    pub castle_boss_defeated: bool,
    pub machine_shell: bool,
}

impl Mm2MechanicalState {
    #[must_use]
    pub fn bosses_beaten(self) -> u8 {
        self.weapons_obtained
            .count_ones()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    #[must_use]
    pub fn tier(self) -> Mm2Tier {
        let castles = if (MM2_FIRST_WILY_STAGE..=MM2_LAST_WILY_STAGE).contains(&self.stage) {
            self.stage - MM2_FIRST_WILY_STAGE
        } else {
            0
        };
        Mm2Tier {
            robot_masters: self.bosses_beaten(),
            castles,
            refights: self.refights.count_ones().try_into().unwrap_or(u8::MAX),
            castle_boss_defeated: self.castle_boss_defeated(),
            machine_shell: self.machine_shell_broken(),
        }
    }

    #[must_use]
    pub fn castle_boss_defeated(self) -> bool {
        (MM2_FIRST_WILY_STAGE..=MM2_LAST_WILY_STAGE).contains(&self.stage)
            && self.boss_phase >= BOSS_PHASE_DEFEATED
            && self.last_castle_boss()
    }

    fn last_castle_boss(self) -> bool {
        self.stage != WILY5_STAGE
            || (self.refights == u8::MAX && self.current_boss == WILY_MACHINE)
    }

    #[must_use]
    pub fn machine_shell_broken(self) -> bool {
        self.stage == WILY5_STAGE
            && self.current_boss == WILY_MACHINE
            && (BOSS_PHASE_REFILL..BOSS_PHASE_DEFEATED).contains(&self.boss_phase)
    }

    #[must_use]
    pub fn refight_boss(self) -> u8 {
        if self.stage == WILY5_STAGE
            && (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&self.boss_phase)
        {
            self.current_boss
        } else {
            NO_REFIGHT_BOSS
        }
    }

    #[must_use]
    pub fn posture(self) -> u8 {
        match self.player_state {
            PLAYER_STATE_AIRBORNE | PLAYER_STATE_HIT => POSTURE_AIRBORNE,
            PLAYER_STATE_LADDER | PLAYER_STATE_LADDER_TOP => POSTURE_LADDER,
            _ => POSTURE_GROUNDED,
        }
    }

    #[must_use]
    pub fn boss_damage(self) -> u8 {
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
    pub fn boss_fight_underway(self) -> bool {
        self.boss_phase != BOSS_PHASE_NONE && self.boss_phase < BOSS_PHASE_DEFEATED
    }

    #[must_use]
    pub fn playable(self) -> bool {
        self.bank == GAMEPLAY_BANK
            && self.health > 0
            && (PLAYER_STATE_HIT..=PLAYER_STATE_LADDER_TOP).contains(&self.player_state)
    }

    #[must_use]
    pub fn is_dead(self) -> bool {
        self.health == 0
            || (self.player_state == PLAYER_STATE_FALLEN && self.y >= BELOW_PLAY_AREA_Y)
    }

    #[must_use]
    pub fn is_dying(self) -> bool {
        self.player_state == PLAYER_STATE_DYING
    }

    fn arrived_at(self, stage: Mm2Stage) -> bool {
        self.stage == stage.number()
            && self.bank == GAMEPLAY_BANK
            && self.player_state == PLAYER_STATE_STANDING
            && self.health == FULL_HEALTH
    }

    fn final_stage_cleared(self) -> bool {
        self.stage == FINAL_STAGE
            && self.boss_phase == BOSS_PHASE_CLEARED
            && self.weapons_obtained == u8::MAX
    }
}

pub fn decode_state(wram: &[u8]) -> Result<Mm2MechanicalState, MachineError> {
    decode_state_after(wram, None)
}

fn decode_state_after(
    wram: &[u8],
    previous_stage: Option<u8>,
) -> Result<Mm2MechanicalState, MachineError> {
    let raw_stage = read_byte(wram, STAGE)?;
    let player_state = read_byte(wram, PLAYER_STATE)?;
    let boss_phase = read_byte(wram, BOSS_PHASE)?;
    let stage = if previous_stage == Some(WILY5_STAGE)
        && raw_stage < MM2_FIRST_WILY_STAGE
        && player_state == PLAYER_STATE_TELEPORTING
        && boss_phase == BOSS_PHASE_NONE
    {
        WILY5_STAGE
    } else {
        raw_stage
    };
    let bank = read_byte(wram, CURRENT_BANK)?;
    let page = read_byte(wram, MENU_PAGE)?;
    let cursor = read_byte(wram, MENU_CURSOR)?;
    let menu = if bank != MENU_BANK {
        MENU_CLOSED
    } else {
        match MENU_ROWS.get(usize::from(page)) {
            Some(rows) if cursor < *rows => page * MENU_PAGE_ROWS + cursor,
            _ => MENU_UNKNOWN,
        }
    };
    let weapon = read_byte(wram, SELECTED_WEAPON)?;
    let fighting = (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&boss_phase);
    Ok(Mm2MechanicalState {
        stage,
        screen: read_byte(wram, PLAYER_SCREEN)?,
        room: read_byte(wram, LEVEL_ROOM)?,
        x: read_byte(wram, PLAYER_X)?,
        y: read_byte(wram, PLAYER_Y)?,
        health: read_byte(wram, PLAYER_HEALTH)?,
        weapon_energy: (WEAPON_ENERGY..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
            .map(|index| read_byte(wram, index).map(u16::from))
            .sum::<Result<u16, MachineError>>()?,
        equipped_energy: match usize::from(weapon) {
            index @ 1..=WEAPON_ENERGY_BYTES => read_byte(wram, WEAPON_ENERGY + index - 1)?,
            _ => 0,
        },
        platforms: live_platforms(wram)?,
        lives: read_byte(wram, LIVES)?,
        player_state,
        weapon,
        menu,
        weapons_obtained: read_byte(wram, WEAPONS_OBTAINED)?,
        boss_health: read_byte(wram, BOSS_HEALTH)?,
        boss_phase,
        camera_state: read_byte(wram, CAMERA_STATE)?,
        enemy_damage: 0,
        bank,
        current_boss: read_byte(wram, CURRENT_BOSS)?,
        refights: if stage == WILY5_STAGE {
            read_byte(wram, REFIGHTS)?
        } else {
            0
        },
        boobeam_targets: if stage == BOOBEAM_STAGE && fighting {
            boobeam_targets(wram)?
        } else {
            0
        },
    })
}

fn boobeam_targets(wram: &[u8]) -> Result<u64, MachineError> {
    let mut targets = 0_u64;
    for slot in 0..OBJECT_SLOTS {
        let id = read_byte(wram, OBJECT_ID_TABLE + slot)?;
        let flags = read_byte(wram, OBJECT_FLAG_TABLE + slot)?;
        if flags & OBJECT_ACTIVE != 0 && (id == BOOBEAM_TRAP_ID || id == BOOBEAM_BARRIER_ID) {
            let column = read_byte(wram, OBJECT_X_TABLE + slot)? / TARGET_GRID_CELL;
            let row = read_byte(wram, OBJECT_Y_TABLE + slot)? / TARGET_GRID_CELL;
            targets |= 1 << (row * TARGET_GRID_COLUMNS + column);
        }
    }
    Ok(targets)
}

fn enemy_damage_between(prior: &[u8], current: &[u8]) -> u8 {
    (0..ENEMY_SLOTS)
        .filter_map(|enemy| {
            let object = OBJECT_SLOTS - ENEMY_SLOTS + enemy;
            let before = *prior.get(ENEMY_HEALTH_TABLE_START + enemy)?;
            let after = *current.get(ENEMY_HEALTH_TABLE_START + enemy)?;
            let active = prior.get(OBJECT_FLAG_TABLE + object)? & OBJECT_ACTIVE != 0;
            let hit = *current.get(ENEMY_HIT_TABLE + enemy)? != 0;
            let id = *current.get(OBJECT_ID_TABLE + object)?;
            let same = *prior.get(OBJECT_ID_TABLE + object)? == id
                && prior.get(ENEMY_INDEX_TABLE + enemy)? == current.get(ENEMY_INDEX_TABLE + enemy)?;
            let killed = after == 0 && id == ENEMY_KILLED_ID;
            (active && hit && (same || killed) && after < before)
                .then(|| before.saturating_sub(after).min(ENEMY_HIT_CAP))
        })
        .fold(0_u8, u8::saturating_add)
}

fn live_platforms(wram: &[u8]) -> Result<u8, MachineError> {
    let mut count = 0_u8;
    for slot in 0..OBJECT_SLOTS {
        let id = read_byte(wram, OBJECT_ID_TABLE + slot)?;
        let flags = read_byte(wram, OBJECT_FLAG_TABLE + slot)?;
        if (ITEM_OBJECT_FIRST..=ITEM_OBJECT_LAST).contains(&id) && flags & OBJECT_ACTIVE != 0 {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn read_byte(bytes: &[u8], address: usize) -> Result<u8, MachineError> {
    bytes.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Mega Man 2 RAM address {address:#x} is absent"))
    })
}

#[must_use]
pub fn spatial_bucket(state: Mm2MechanicalState) -> (u8, u8, u8, u8, u8, u8) {
    (
        state.stage,
        state.screen,
        state.boss_damage() / BOSS_DAMAGE_BUCKET,
        state.enemy_damage / ENEMY_DAMAGE_BUCKET,
        state.x / 32,
        state.y / 32,
    )
}

pub const ENEMY_DAMAGE_BUCKET: u8 = 2;

pub const BOSS_DAMAGE_BUCKET: u8 = 2;

#[must_use]
pub fn preference_tuple(state: Mm2MechanicalState) -> (Mm2Tier, u8, u16) {
    (state.tier(), state.health, state.weapon_energy)
}

#[must_use]
pub fn encounter(state: Mm2MechanicalState) -> (u8, u8, u64) {
    (state.refights, state.refight_boss(), state.boobeam_targets)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Observations {
    pub frame_count: u64,
    pub decoded: Mm2MechanicalState,
    pub dead: bool,
    pub fall_run: u16,
    pub dying_run: u32,
    pub arrived: bool,
    pub ending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mm2Route {
    Stage(Mm2Stage),
    WholeGame,
}

impl Mm2Route {
    #[must_use]
    pub fn stage(self) -> Option<Mm2Stage> {
        match self {
            Self::Stage(stage) => Some(stage),
            Self::WholeGame => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    RobotMaster(u8),
    Castle(u8),
}

fn transition(state: Mm2MechanicalState) -> Option<Transition> {
    if state.stage < MM2_FIRST_WILY_STAGE && state.boss_phase >= BOSS_PHASE_DEFEATED {
        return Some(Transition::RobotMaster(state.stage));
    }
    ((MM2_FIRST_WILY_STAGE..MM2_LAST_WILY_STAGE).contains(&state.stage)
        && state.boss_phase == BOSS_PHASE_CLEARED
        && state.last_castle_boss())
        .then_some(Transition::Castle(state.stage))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub input_endpoint: Mm2MechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Snapshot {
    emulator_state: Vec<u8>,
    observation: Mm2Observations,
    failed: bool,
}

impl Mm2Snapshot {
    #[cfg(test)]
    pub(crate) fn for_census_tests(decoded: Mm2MechanicalState) -> Self {
        Self {
            emulator_state: Vec::new(),
            observation: Mm2Observations {
                decoded,
                ..Mm2Observations::default()
            },
            failed: false,
        }
    }

    #[must_use]
    pub fn state(&self) -> Mm2MechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.len()
    }
}

const BOOT_TO_STAGE_SELECT: [ButtonChord; 3] = [
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 4,
    },
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 4,
    },
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 4,
    },
];
const MENU_SETTLE_FRAMES: u32 = 300;
const TITLE_SETTLE_FRAMES: u32 = 600;
const AWARD_SETTLE_FRAMES: u32 = 900;
const STAGE_START_WAIT_FRAMES: u32 = 1_200;
const WILY_STAGE_START_WAIT_FRAMES: u32 = 8_000;

#[derive(Debug)]
pub struct Mm2Target {
    machine: QuickNesMachine,
    genesis: SnapId,
    genesis_observation: Mm2Observations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: Mm2Observations,
    action_observations: Vec<Mm2Observations>,
    failed: bool,
    genesis_weapons: u8,
    genesis_prefix: Vec<ButtonChord>,
    execution_work: u64,
    route: Mm2Route,
    sink: Option<CaptureSink>,
}

pub type FrameSink = Box<dyn FnMut(&[VideoFrame], &[i16]) -> Result<(), Box<dyn Error>> + Send>;

struct CaptureSink(FrameSink);

impl std::fmt::Debug for CaptureSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CaptureSink")
    }
}

fn idle_chords(frames: u32) -> Vec<ButtonChord> {
    let mut chords = Vec::new();
    let mut remaining = frames;
    while remaining > 0 {
        let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
        chords.push(ButtonChord::new(
            0,
            u8::try_from(hold).unwrap_or(MAX_HOLD_FRAMES),
        ));
        remaining -= hold;
    }
    chords
}

#[must_use]
pub fn power_on_walk() -> Vec<ButtonChord> {
    let mut chords = idle_chords(TITLE_SETTLE_FRAMES);
    for press in BOOT_TO_STAGE_SELECT {
        chords.push(press);
        chords.extend(idle_chords(MENU_SETTLE_FRAMES));
    }
    chords
}

const MENU_MODE: usize = 0xf7;
const MENU_MODE_STAGE_SELECT: u8 = 0x90;
const STAGE_SELECT_WALK_ROUNDS: usize = 16;

fn at_stage_select(wram: &[u8]) -> bool {
    wram.get(MENU_MODE).copied() == Some(MENU_MODE_STAGE_SELECT)
}

pub fn walk_to_stage_select(
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
    chords: &[ButtonChord],
) -> Result<Vec<ButtonChord>, MachineError> {
    let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
    for chunk in chords.chunks(64) {
        run_chords(&mut machine, chunk)?;
    }
    let mut walk = idle_chords(AWARD_SETTLE_FRAMES);
    run_chords(&mut machine, &walk)?;
    for _ in 0..STAGE_SELECT_WALK_ROUNDS {
        if at_stage_select(&machine.read_wram()?) {
            return Ok(walk);
        }
        let round = [
            ButtonChord::new(JOYPAD_DOWN, 4),
            ButtonChord::new(0, 30),
            ButtonChord::new(JOYPAD_START, 4),
        ];
        run_chords(&mut machine, &round)?;
        walk.extend(round);
        let settle = idle_chords(MENU_SETTLE_FRAMES);
        run_chords(&mut machine, &settle)?;
        walk.extend(settle);
    }
    Err(MachineError::Backend(
        "Mega Man 2 award screens did not return to stage select".into(),
    ))
}

pub fn target_from_args<'a>(
    args: &'a [String],
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
) -> Result<(Mm2Target, &'a [String]), Box<dyn Error>> {
    let read = |path: &String| -> Result<Mm2Input, Box<dyn Error>> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    };
    match args {
        [mode, option, path, rest @ ..] if mode == "whole-game" && option == "--root" => Ok((
            Mm2Target::whole_game(rom, core_path, core_sha256, &read(path)?.actions)?,
            rest,
        )),
        [mode, option, path, rest @ ..] if mode == "whole-game" && option == "--tape" => Ok((
            Mm2Target::whole_game_after_tape(rom, core_path, core_sha256, &read(path)?.actions)?,
            rest,
        )),
        [mode, rest @ ..] if mode == "whole-game" => Ok((
            Mm2Target::whole_game(rom, core_path, core_sha256, &[])?,
            rest,
        )),
        [stage, prefix, rest @ ..] => Ok((
            Mm2Target::from_rom_bytes_after(
                rom,
                core_path,
                core_sha256,
                &read(prefix)?.actions,
                Mm2Stage::parse(stage)?,
            )?,
            rest,
        )),
        _ => Err("expected <stage> <chain-prefix.json> or whole-game".into()),
    }
}

fn run_chords(machine: &mut QuickNesMachine, chords: &[ButtonChord]) -> Result<(), MachineError> {
    let here = machine.snapshot()?;
    machine.branch(here, &nes::reproducer(chords))?;
    machine.run(StopConditions::default(), None)?;
    machine.drop_snapshot(here)
}

impl Mm2Target {
    pub fn from_rom_bytes_headless_at_stage(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        stage: Mm2Stage,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_after(rom, core_path, core_sha256, &power_on_walk(), stage)
    }

    pub fn from_rom_bytes_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
        stage: Mm2Stage,
    ) -> Result<Self, MachineError> {
        Self::from_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            prefix,
            stage,
            Mm2Route::Stage(stage),
        )
    }

    pub fn whole_game(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        root: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let mut target = Self::from_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            &power_on_walk(),
            ROBOT_MASTER_ORDER[0],
            Mm2Route::WholeGame,
        )?;
        for (index, action) in root.iter().enumerate() {
            target.apply(action);
            if target.failed || target.is_terminal() {
                return Err(MachineError::Backend(format!(
                    "Mega Man 2 root input ends its run at action {index}"
                )));
            }
        }
        target.rebase_genesis()?;
        Ok(target)
    }

    pub fn whole_game_after_tape(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        tape: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        for chunk in tape.chunks(64) {
            run_chords(&mut machine, chunk)?;
        }
        let wram = machine.read_wram()?;
        let state = decode_state(&wram)?;
        if !state.playable() {
            return Err(MachineError::Backend(format!(
                "Mega Man 2 tape does not end in play: {state:?}"
            )));
        }
        Self::at_genesis(machine, wram, tape.to_vec(), Mm2Route::WholeGame)
    }

    fn rebase_genesis(&mut self) -> Result<(), MachineError> {
        self.machine.drop_snapshot(self.genesis)?;
        self.genesis = self.machine.snapshot()?;
        self.genesis_wram = self.current_wram;
        self.genesis_observation = Mm2Observations {
            frame_count: 0,
            ..self.observation.clone()
        };
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.genesis_weapons = self.observation.decoded.weapons_obtained;
        Ok(())
    }

    fn from_machine(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
        stage: Mm2Stage,
        route: Mm2Route,
    ) -> Result<Self, MachineError> {
        let mut genesis_prefix = prefix.to_vec();
        for chunk in prefix.chunks(64) {
            run_chords(&mut machine, chunk)?;
        }
        for direction in stage.select_path() {
            let chords = [ButtonChord::new(*direction, 4), ButtonChord::new(0, 30)];
            run_chords(&mut machine, &chords)?;
            genesis_prefix.extend(chords);
        }
        if stage.number() <= MM2_FIRST_WILY_STAGE {
            run_chords(&mut machine, &[ButtonChord::new(JOYPAD_START, 4)])?;
            genesis_prefix.push(ButtonChord::new(JOYPAD_START, 4));
        }
        let wait_limit = if stage.is_wily() {
            WILY_STAGE_START_WAIT_FRAMES
        } else {
            STAGE_START_WAIT_FRAMES
        };
        let mut waited = 0;
        let mut wram = machine.read_wram()?;
        loop {
            let state = decode_state(&wram)?;
            if state.arrived_at(stage) {
                break;
            }
            if waited >= wait_limit {
                return Err(MachineError::Backend(format!(
                    "Mega Man 2 stage {} did not reach play within {wait_limit} \
                     frames: stage={} player_state={} health={}",
                    stage.number(),
                    state.stage,
                    state.player_state,
                    state.health
                )));
            }
            run_chords(&mut machine, &[ButtonChord::new(0, 1)])?;
            waited += 1;
            wram = machine.read_wram()?;
        }
        genesis_prefix.extend(idle_chords(waited));
        Self::at_genesis(machine, wram, genesis_prefix, route)
    }

    fn at_genesis(
        mut machine: QuickNesMachine,
        wram: [u8; WRAM_SIZE],
        genesis_prefix: Vec<ButtonChord>,
        route: Mm2Route,
    ) -> Result<Self, MachineError> {
        let state = decode_state(&wram)?;
        let genesis = machine.snapshot()?;
        let observation = Mm2Observations {
            decoded: state,
            arrived: true,
            ..Mm2Observations::default()
        };
        Ok(Self {
            machine,
            genesis,
            genesis_observation: observation.clone(),
            genesis_wram: wram,
            current_wram: wram,
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            genesis_weapons: state.weapons_obtained,
            genesis_prefix,
            execution_work: 0,
            route,
            sink: None,
        })
    }

    #[must_use]
    pub fn genesis_prefix(&self) -> &[ButtonChord] {
        &self.genesis_prefix
    }

    #[must_use]
    pub fn mechanical_state(&self) -> Mm2MechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.dead
    }

    #[must_use]
    pub fn route(&self) -> Mm2Route {
        self.route
    }

    #[must_use]
    pub fn defeated_a_boss(&self) -> bool {
        let state = self.observation.decoded;
        matches!(self.route, Mm2Route::Stage(_))
            && (state.weapons_obtained & !self.genesis_weapons != 0
                || state.stage > self.genesis_observation.decoded.stage)
    }

    #[must_use]
    pub fn objective_reached(&self) -> bool {
        match self.route {
            Mm2Route::Stage(_) => self.defeated_a_boss(),
            Mm2Route::WholeGame => self.observation.ending,
        }
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.is_dead() || self.objective_reached()
    }

    pub fn start_capturing(&mut self) {
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
    }

    pub fn set_frame_sink(&mut self, sink: Option<FrameSink>) {
        self.sink = sink.map(CaptureSink);
    }

    pub fn drain_frames(&mut self) -> Vec<VideoFrame> {
        self.machine.take_video_frames()
    }

    pub fn drain_audio(&mut self) -> Vec<i16> {
        self.machine.take_audio_samples()
    }

    pub fn diagnostic_weapon_energies(&self) -> Result<Vec<u8>, MachineError> {
        let wram = self.machine.read_wram()?;
        (WEAPON_ENERGY..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
            .map(|index| read_byte(&wram, index))
            .collect()
    }

    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    #[must_use]
    pub fn genesis_weapons(&self) -> u8 {
        self.genesis_weapons
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[Mm2Observations] {
        &self.action_observations
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_terminal() || frames == 0 {
            return false;
        }
        let mut actions = Vec::new();
        let mut remaining = frames;
        while remaining > 0 {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            actions.push(ButtonChord::new(buttons, u8::try_from(hold).unwrap_or(1)));
            remaining -= hold;
        }
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return false;
        };
        let survived = (|| {
            self.machine.branch(start, &nes::reproducer(&actions))?;
            self.machine.run(StopConditions::default(), None)?;
            let mut alive = true;
            for wram in self.machine.frames() {
                if decode_state(wram)?.is_dead() {
                    alive = false;
                    break;
                }
            }
            self.machine.replay(start)?;
            Ok::<bool, MachineError>(alive)
        })();
        let _ = self.machine.drop_snapshot(start);
        match survived {
            Ok(alive) => alive,
            Err(_) => {
                self.failed = true;
                false
            }
        }
    }

    pub fn render_input(
        &mut self,
        input: &Mm2Input,
        tail_frames: u32,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<Mm2VideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Mega Man 2 gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            for action in &input.actions {
                self.render_action(*action, video_output, audio_output, &mut metadata)?;
            }
            let input_endpoint = decode_state(&self.machine.read_wram()?)?;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                let hold = u8::try_from(hold)?;
                self.render_action(
                    ButtonChord::new(0, hold),
                    video_output,
                    audio_output,
                    &mut metadata,
                )?;
                remaining -= u32::from(hold);
            }
            let mut metadata = metadata.ok_or("QuickNES produced no video frames")?;
            if metadata.audio_frames == 0 {
                return Err("QuickNES produced no audio samples".into());
            }
            metadata.input_endpoint = input_endpoint;
            Ok(metadata)
        })();
        self.machine.set_audio_capture(false);
        self.machine.set_video_capture(false);
        result
    }

    fn render_action(
        &mut self,
        action: ButtonChord,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
        metadata: &mut Option<Mm2VideoMetadata>,
    ) -> Result<(), Box<dyn Error>> {
        let start = self.machine.snapshot()?;
        self.machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))?;
        self.machine.drop_snapshot(start)?;
        for _ in 0..action.bounded_hold_frames() {
            if !self
                .run_one_frame()
                .map_err(|_| "QuickNES film frame failed")?
            {
                break;
            }
            let frame = self
                .machine
                .take_video_frame()
                .ok_or("QuickNES omitted a requested video frame")?;
            match metadata {
                Some(existing)
                    if (existing.width, existing.height) != (frame.width, frame.height) =>
                {
                    return Err("QuickNES video geometry changed during replay".into());
                }
                Some(existing) => existing.frames = existing.frames.saturating_add(1),
                None => {
                    *metadata = Some(Mm2VideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        input_endpoint: Mm2MechanicalState::default(),
                    });
                }
            }
            video_output.write_all(&frame.rgb24)?;
            let audio = self.machine.take_audio_samples();
            if !audio
                .len()
                .is_multiple_of(usize::from(QUICKNES_AUDIO_CHANNELS))
            {
                return Err("QuickNES produced a partial stereo audio frame".into());
            }
            for sample in &audio {
                audio_output.write_all(&sample.to_le_bytes())?;
            }
            let audio_frames = u64::try_from(audio.len() / usize::from(QUICKNES_AUDIO_CHANNELS))?;
            if let Some(existing) = metadata {
                existing.audio_frames = existing.audio_frames.saturating_add(audio_frames);
            }
        }
        Ok(())
    }

    fn run_one_frame(&mut self) -> Result<bool, ()> {
        let deadline = machine::Moment(self.machine.now().0.saturating_add(1));
        match self.machine.run(
            StopConditions {
                deadline: Some(deadline),
                on: machine::StopMask::NONE,
            },
            None,
        ) {
            Ok(machine::StopReason::Deadline { .. }) => Ok(true),
            Ok(machine::StopReason::Quiescent { .. }) => Ok(false),
            Ok(_) | Err(_) => {
                self.failed = true;
                Err(())
            }
        }
    }

    fn make_observation(&self, frame_count: u64, state: Mm2MechanicalState) -> Mm2Observations {
        Mm2Observations {
            frame_count,
            decoded: state,
            dead: state.is_dead(),
            ..Mm2Observations::default()
        }
    }

    fn run_action(&mut self, action: &ButtonChord) -> Option<Vec<[u8; WRAM_SIZE]>> {
        let start = self.machine.snapshot().ok()?;
        let branched = self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)));
        let _ = self.machine.drop_snapshot(start);
        branched.ok()?;
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(run, Ok(machine::StopReason::Quiescent { .. })) {
            return None;
        }
        let frames = self.machine.frames().to_vec();
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        (!frames.is_empty()).then_some(frames)
    }

    fn advance(&mut self, chords: &[ButtonChord]) -> Option<u64> {
        let mut frames = 0_u64;
        for chord in chords {
            frames += u64::try_from(self.run_action(chord)?.len()).ok()?;
            if let Some(CaptureSink(sink)) = &mut self.sink {
                let video = self.machine.take_video_frames();
                let audio = self.machine.take_audio_samples();
                sink(&video, &audio).ok()?;
            }
        }
        Some(frames)
    }

    fn wram_state(&self) -> Option<Mm2MechanicalState> {
        decode_state(&self.machine.read_wram().ok()?).ok()
    }

    fn wait_for_arrival(&mut self, stage: Mm2Stage, limit: u32) -> Option<u64> {
        let mut waited = 0_u64;
        while !self.wram_state()?.arrived_at(stage) {
            if waited >= u64::from(limit) {
                return None;
            }
            waited += self.advance(&[ButtonChord::new(0, 1)])?;
        }
        Some(waited)
    }

    fn drive_after_robot_master(&mut self, stage: u8) -> Option<u64> {
        let mut frames = 0_u64;
        let award = 1_u8 << stage;
        while self.wram_state()?.weapons_obtained & award == 0 {
            if frames >= u64::from(2 * AWARD_SETTLE_FRAMES) {
                return None;
            }
            frames += self.advance(&[ButtonChord::new(0, MAX_HOLD_FRAMES)])?;
        }
        frames += self.advance(&idle_chords(AWARD_SETTLE_FRAMES))?;
        for _ in 0..STAGE_SELECT_WALK_ROUNDS {
            if at_stage_select(&self.machine.read_wram().ok()?) {
                break;
            }
            frames += self.advance(&[
                ButtonChord::new(JOYPAD_DOWN, 4),
                ButtonChord::new(0, 30),
                ButtonChord::new(JOYPAD_START, 4),
            ])?;
            frames += self.advance(&idle_chords(MENU_SETTLE_FRAMES))?;
        }
        if !at_stage_select(&self.machine.read_wram().ok()?) {
            return None;
        }
        let next = next_stage_after(self.wram_state()?.weapons_obtained);
        for direction in next.select_path() {
            frames += self.advance(&[ButtonChord::new(*direction, 4), ButtonChord::new(0, 30)])?;
        }
        frames += self.advance(&[ButtonChord::new(JOYPAD_START, 4)])?;
        let limit = if next.is_wily() {
            WILY_STAGE_START_WAIT_FRAMES
        } else {
            STAGE_START_WAIT_FRAMES
        };
        Some(frames + self.wait_for_arrival(next, limit)?)
    }

    fn drive(&mut self, transition: Transition) -> Option<u64> {
        match transition {
            Transition::RobotMaster(stage) => self.drive_after_robot_master(stage),
            Transition::Castle(stage) => {
                self.wait_for_arrival(Mm2Stage(stage + 1), WILY_STAGE_START_WAIT_FRAMES)
            }
        }
    }
}

impl Target for Mm2Target {
    type Action = ButtonChord;
    type Observations = Mm2Observations;
    type Snapshot = Mm2Snapshot;

    fn reset(&mut self) {
        self.failed = self.machine.replay(self.genesis).is_err();
        self.current_wram = self.genesis_wram;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_terminal() {
            return;
        }
        let Some(mut frames) = self.run_action(action) else {
            self.failed = true;
            return;
        };
        let whole_game = self.route == Mm2Route::WholeGame;
        let mut waited = 0;
        while !whole_game
            && waited < AWARD_SETTLE_FRAMES
            && frames.last().is_some_and(|wram| {
                decode_state(wram).is_ok_and(|state| {
                    state.boss_phase >= BOSS_PHASE_DEFEATED
                        && state.weapons_obtained & !self.genesis_weapons == 0
                        && state.stage == self.genesis_observation.decoded.stage
                        && !state.is_dead()
                })
            })
        {
            let Some(idle) = self.run_action(&ButtonChord::new(0, MAX_HOLD_FRAMES)) else {
                self.failed = true;
                return;
            };
            waited += u32::from(MAX_HOLD_FRAMES);
            frames.extend(idle);
        }
        let frames = &frames;
        let mut prior_wram = self.current_wram;
        let mut prior_state = self.observation.decoded;
        let mut emitted = false;
        let mut observations = Vec::new();
        let genesis = self.genesis_observation.decoded;
        let mut died = self.observation.dead;
        let mut dying_run = self.observation.dying_run;
        let mut fall_run = self.observation.fall_run;
        let mut enemy_damage = self.observation.decoded.enemy_damage;
        let mut prior_frame = self.current_wram;
        let mut previous = self.observation.decoded;
        let mut pending = None;
        let mut ending = false;
        let mut decoded_frames = frames.len();
        for (offset, wram) in frames.iter().enumerate() {
            let Ok(mut state) = decode_state_after(wram, Some(previous.stage)) else {
                self.failed = true;
                return;
            };
            let steady = state.camera_state != CAMERA_STATE_SCROLLING
                && previous.camera_state != CAMERA_STATE_SCROLLING
                && state.screen == previous.screen;
            let dropped = state.y.wrapping_sub(previous.y);
            fall_run = if steady && dropped > 0 && dropped <= FALL_STEP_LIMIT {
                fall_run.saturating_add(u16::from(dropped))
            } else {
                0
            };
            let fell_out = fall_run > FALL_RUN_LIMIT;
            let lost_life = if whole_game {
                state.lives < previous.lives
            } else {
                state.lives < genesis.lives || state.stage < genesis.stage
            };
            ending = whole_game && previous.stage == MM2_LAST_WILY_STAGE && state.final_stage_cleared();
            previous = state;
            if state.screen != prior_state.screen || state.stage != prior_state.stage {
                enemy_damage = 0;
            }
            enemy_damage = enemy_damage
                .saturating_add(enemy_damage_between(&prior_frame, wram))
                .min(ENEMY_DAMAGE_CAP);
            state.enemy_damage = enemy_damage;
            prior_frame = *wram;
            let escaped = prior_state.boss_phase >= BOSS_PHASE_FIGHTING
                && prior_state.boss_phase < BOSS_PHASE_DEFEATED
                && state.screen != prior_state.screen;
            dying_run = if state.is_dying() {
                dying_run.saturating_add(1)
            } else {
                0
            };
            let dead = died
                || state.is_dead()
                || dying_run >= DYING_FRAMES
                || lost_life
                || escaped
                || fell_out;
            if whole_game && !dead {
                pending = transition(state);
            }
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
                || encounter(state) != encounter(prior_state)
                || dead != died
                || ending
                || pending.is_some();
            died = dead;
            if boundary {
                let frame_count = self
                    .observation
                    .frame_count
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
                let mut observation = self.make_observation(frame_count, state);
                observation.dead = dead;
                observation.fall_run = fall_run;
                observation.dying_run = dying_run;
                observation.ending = ending;
                observations.push(observation);
                prior_wram = *wram;
                prior_state = state;
                emitted = true;
            }
            if ending || pending.is_some() {
                decoded_frames = offset + 1;
                break;
            }
        }
        let action_frames = self
            .observation
            .frame_count
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        if let Some(transition) = pending {
            let Some(driven) = self.drive(transition) else {
                self.failed = true;
                return;
            };
            let Ok(wram) = self.machine.read_wram() else {
                self.failed = true;
                return;
            };
            let Ok(state) = decode_state(&wram) else {
                self.failed = true;
                return;
            };
            observations.push(Mm2Observations {
                frame_count: action_frames.saturating_add(driven),
                decoded: state,
                arrived: true,
                ..Mm2Observations::default()
            });
            self.action_observations = observations;
            self.observation = self.action_observations.last().cloned().unwrap_or_default();
            self.current_wram = wram;
            return;
        }
        let endpoint_wram = frames
            .get(decoded_frames.saturating_sub(1))
            .copied()
            .unwrap_or(prior_wram);
        let endpoint_frame = if ending {
            self.observation
                .frame_count
                .saturating_add(u64::try_from(decoded_frames).unwrap_or(u64::MAX))
        } else {
            action_frames
        };
        if !emitted
            || !observations
                .last()
                .is_some_and(|observation| observation.frame_count == endpoint_frame)
        {
            let Ok(mut endpoint_state) = decode_state_after(&endpoint_wram, Some(previous.stage))
            else {
                self.failed = true;
                return;
            };
            endpoint_state.enemy_damage = enemy_damage;
            let mut observation = self.make_observation(endpoint_frame, endpoint_state);
            observation.dead = died;
            observation.fall_run = fall_run;
            observation.dying_run = dying_run;
            observations.push(observation);
        }
        self.action_observations = observations;
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        (u64::from(state.stage) << 40)
            | (u64::from(state.screen) << 32)
            | (u64::from(state.x / 32) << 16)
            | u64::from(state.y / 32)
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
        let Ok(emulator_state) = self.machine.capture_nes(None) else {
            self.failed = true;
            return None;
        };
        Some(Mm2Snapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        self.machine
            .restore_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        self.current_wram = self
            .machine
            .read_wram()
            .map_err(|error| error.to_string())?;
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_parse_by_number_and_name() {
        assert_eq!(Mm2Stage::parse("6").expect("number").name(), "metal");
        assert_eq!(Mm2Stage::parse("Wood").expect("name").number(), 2);
        assert_eq!(Mm2Stage::parse("8").expect("castle").name(), "wily1");
        assert!(Mm2Stage::parse("wily1").expect("castle").is_wily());
        assert!(!Mm2Stage::parse("crash").expect("master").is_wily());
        assert!(Mm2Stage::parse("14").is_err());
        assert!(Mm2Stage::parse("wily").is_err());
    }

    #[test]
    fn death_is_zero_health_a_pit_or_a_fall_below_the_play_area() {
        let alive = Mm2MechanicalState {
            health: 28,
            player_state: 0x06,
            y: 0xb4,
            ..Mm2MechanicalState::default()
        };
        assert!(!alive.is_dead());
        assert!(Mm2MechanicalState { health: 0, ..alive }.is_dead());
        let falling = Mm2MechanicalState {
            player_state: PLAYER_STATE_FALLEN,
            y: 0xe8,
            ..alive
        };
        assert!(falling.is_dead());
        let landing = Mm2MechanicalState { y: 0x90, ..falling };
        assert!(!landing.is_dead());
        let pit = Mm2MechanicalState {
            player_state: PLAYER_STATE_DYING,
            ..alive
        };
        assert!(!pit.is_dead());
        assert!(pit.is_dying());
    }

    #[test]
    fn state_is_decoded_from_fixed_offsets() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x2a] = 6;
        wram[0x440] = 11;
        wram[0x460] = 0xf8;
        wram[0x4a0] = 0xb4;
        wram[0x6c0] = 28;
        wram[0xa8] = 2;
        wram[0x2c] = 5;
        wram[0x9a] = 0x40;
        let state = decode_state(&wram).expect("decode");
        assert_eq!(
            (state.stage, state.screen, state.x, state.y),
            (6, 11, 0xf8, 0xb4)
        );
        assert_eq!((state.health, state.lives, state.player_state), (28, 2, 5));
        assert_eq!(state.bosses_beaten(), 1);
        assert_eq!(spatial_bucket(state), (6, 11, 0, 0, 7, 5));
    }

    #[test]
    fn enemy_damage_counts_confirmed_hits_on_active_enemies_only() {
        let mut prior = vec![0_u8; WRAM_SIZE];
        prior[0x10f] = 61;
        prior[0x41f] = 78;
        prior[0x43f] = 0xc3;
        prior[0x6df] = 20;
        let mut current = prior.clone();
        current[0x6df] = 18;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        current[0x11f] = 1;
        assert_eq!(enemy_damage_between(&prior, &current), 2);
        current[0x41f] = 79;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        current[0x41f] = ENEMY_KILLED_ID;
        current[0x6df] = 0;
        assert_eq!(enemy_damage_between(&prior, &current), ENEMY_HIT_CAP);
        prior[0x43f] = 0;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
    }

    #[test]
    fn the_menu_is_open_only_in_the_menu_bank_with_a_cursor_on_the_page() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x04] = 3;
        wram[0x29] = 0x0e;
        wram[0xfd] = 10;
        wram[0xfe] = 2;
        assert_eq!(decode_state(&wram).expect("decode").menu, MENU_CLOSED);
        wram[0x29] = 0x0d;
        assert_eq!(decode_state(&wram).expect("decode").menu, MENU_UNKNOWN);
        wram[0xfd] = 6;
        wram[0xfe] = 0;
        assert_eq!(decode_state(&wram).expect("decode").menu, 6);
        wram[0xfd] = 6;
        wram[0xfe] = 1;
        assert_eq!(decode_state(&wram).expect("decode").menu, 14);
        wram[0xfd] = 7;
        assert_eq!(decode_state(&wram).expect("decode").menu, MENU_UNKNOWN);
    }

    #[test]
    fn a_wily5_teleport_keeps_the_castle_stage_while_the_portal_borrows_the_byte() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x2a] = 4;
        wram[0x2c] = PLAYER_STATE_TELEPORTING;
        wram[0xbc] = 0x80;
        let borrowed = decode_state_after(&wram, Some(WILY5_STAGE)).expect("decode");
        assert_eq!((borrowed.stage, borrowed.refights), (WILY5_STAGE, 0x80));
        assert_eq!(decode_state(&wram).expect("decode").stage, 4);
        wram[0x2c] = PLAYER_STATE_STANDING;
        assert_eq!(decode_state_after(&wram, Some(WILY5_STAGE)).expect("decode").stage, 4);
    }

    #[test]
    fn the_machine_refill_after_its_shell_breaks_is_not_damage() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x2a] = WILY5_STAGE;
        wram[0xb1] = BOSS_PHASE_REFILL;
        wram[0xb3] = WILY_MACHINE;
        wram[0xbc] = u8::MAX;
        wram[0x6c1] = 1;
        let state = decode_state(&wram).expect("decode");
        assert!(state.machine_shell_broken());
        assert_eq!(state.boss_damage(), 0);
        assert!(state.tier().machine_shell);
    }

    #[test]
    fn boobeam_targets_and_barriers_are_read_only_during_the_wily4_fight() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x2a] = BOOBEAM_STAGE;
        wram[0xb1] = BOSS_PHASE_FIGHTING;
        wram[0x414] = BOOBEAM_TRAP_ID;
        wram[0x434] = 0xc3;
        wram[0x419] = BOOBEAM_BARRIER_ID;
        wram[0x439] = 0x92;
        wram[0x41a] = BOOBEAM_BARRIER_ID;
        wram[0x474] = 232;
        wram[0x4b4] = 112;
        wram[0x479] = 56;
        wram[0x4b9] = 96;
        assert_eq!(
            decode_state(&wram).expect("decode").boobeam_targets,
            1 << 31 | 1 << 25
        );
        wram[0xb1] = BOSS_PHASE_NONE;
        assert_eq!(decode_state(&wram).expect("decode").boobeam_targets, 0);
    }

    #[test]
    fn boobeam_targets_follow_positions_whatever_slots_hold_them() {
        let mut first = vec![0_u8; WRAM_SIZE];
        first[0x2a] = BOOBEAM_STAGE;
        first[0xb1] = BOSS_PHASE_FIGHTING;
        let mut second = first.clone();
        for (wram, trap, barrier) in [(&mut first, 0x14, 0x15), (&mut second, 0x17, 0x12)] {
            wram[0x400 + trap] = BOOBEAM_TRAP_ID;
            wram[0x420 + trap] = 0x80;
            wram[0x460 + trap] = 172;
            wram[0x4a0 + trap] = 60;
            wram[0x400 + barrier] = BOOBEAM_BARRIER_ID;
            wram[0x420 + barrier] = 0x80;
            wram[0x460 + barrier] = 120;
            wram[0x4a0 + barrier] = 42;
        }
        assert_eq!(
            decode_state(&first).expect("decode").boobeam_targets,
            decode_state(&second).expect("decode").boobeam_targets
        );
    }

    #[test]
    fn the_route_takes_the_next_unbeaten_robot_master_then_wily() {
        assert_eq!(next_stage_after(0).name(), "crash");
        assert_eq!(next_stage_after(0x80).name(), "flash");
        assert_eq!(next_stage_after(0xe0).name(), "air");
        assert_eq!(next_stage_after(0xef).name(), "quick");
        assert_eq!(next_stage_after(u8::MAX).name(), "wily1");
    }

    #[test]
    fn transitions_start_at_a_robot_master_kill_or_a_castle_clear() {
        let mut state = Mm2MechanicalState {
            stage: 7,
            boss_phase: BOSS_PHASE_DEFEATED,
            ..Mm2MechanicalState::default()
        };
        assert_eq!(transition(state), Some(Transition::RobotMaster(7)));
        state.stage = 9;
        assert_eq!(transition(state), None);
        state.boss_phase = BOSS_PHASE_CLEARED;
        assert_eq!(transition(state), Some(Transition::Castle(9)));
        state.stage = WILY5_STAGE;
        state.refights = 0x7f;
        assert_eq!(transition(state), None);
        state.refights = u8::MAX;
        state.current_boss = WILY_MACHINE;
        assert_eq!(transition(state), Some(Transition::Castle(WILY5_STAGE)));
        state.stage = MM2_LAST_WILY_STAGE;
        assert_eq!(transition(state), None);
    }

    #[test]
    fn a_boss_on_screen_without_a_loaded_meter_takes_no_damage() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0xb1] = BOSS_PHASE_FIGHTING;
        wram[0x6c1] = 0;
        let approaching = decode_state(&wram).expect("decode");
        assert_eq!(approaching.boss_damage(), 0);
        assert!(approaching.boss_fight_underway());
        wram[0x6c1] = FULL_BOSS_HEALTH;
        assert_eq!(decode_state(&wram).expect("decode").boss_damage(), 0);
        wram[0x6c1] = FULL_BOSS_HEALTH - 4;
        assert_eq!(decode_state(&wram).expect("decode").boss_damage(), 4);
    }

    #[test]
    fn boss_damage_counts_only_while_the_boss_fights() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[0x6c1] = 20;
        wram[0xb1] = 1;
        let filling = decode_state(&wram).expect("decode");
        assert_eq!(filling.boss_damage(), 0);
        wram[0xb1] = 2;
        let fighting = decode_state(&wram).expect("decode");
        assert_eq!(fighting.boss_damage(), 8);
        assert_eq!(spatial_bucket(fighting).2, 4);
        assert!(fighting.boss_fight_underway());
        wram[0xb1] = 3;
        assert_eq!(decode_state(&wram).expect("decode").boss_damage(), 8);
        assert!(
            !decode_state(&[0_u8; WRAM_SIZE])
                .expect("decode")
                .boss_fight_underway()
        );
        wram[0xb1] = 0xfe;
        wram[0x6c1] = 0;
        let dead = decode_state(&wram).expect("decode");
        assert_eq!(dead.boss_damage(), FULL_BOSS_HEALTH);
    }
}
