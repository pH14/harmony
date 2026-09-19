// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::Path};

use machine::{
    nes,
    quicknes::{QuickNesMachine, VideoFrame, QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE},
    Machine, MachineError, SnapId, StopConditions,
};
use serde::{Deserialize, Serialize};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const CAMERA_STATE: usize = 0x1b;
const CAMERA_STATE_SCROLLING: u8 = 0x80;
const SCROLL_DIRECTION: usize = 0x37;
const FALL_STEP_LIMIT: u8 = 64;
const FALL_RUN_LIMIT: u16 = 256;
const ENEMY_HEALTH_TABLE_START: usize = 0x6d0;
const ENEMY_HEALTH_TABLE_END: usize = 0x6e0;
const ENEMY_INDEX_TABLE: usize = 0x100;
const ENEMY_HIT_FLAGS: usize = 0x110;
const KILLED_OBJECT: u8 = 0x06;
const ENEMY_HIT_CAP: u8 = 4;
const ENEMY_DAMAGE_CAP: u8 = 24;
const STAGE: usize = 0x2a;
const PLAYER_STATE: usize = 0x2c;
const WEAPONS_OBTAINED: usize = 0x9a;
const LIVES: usize = 0xa8;
const PLAYER_SCREEN: usize = 0x440;
const LEVEL_ROOM: usize = 0x20;
const CURRENT_BANK: usize = 0x29;
const SELECTED_WEAPON: usize = 0xa9;
const MENU_CURSOR: usize = 0xfd;
const MENU_PAGE: usize = 0xfe;
const MENU_BANK: u8 = 0x0d;
const MENU_ROWS: u8 = 8;
pub const MENU_CLOSED: u8 = 0xff;
pub const MENU_UNKNOWN: u8 = 0xfe;
const DYING_FRAMES: u32 = 30;
const PLAYER_X: usize = 0x460;
const PLAYER_Y: usize = 0x4a0;
const PLAYER_HEALTH: usize = 0x6c0;
const WEAPON_ENERGY: usize = 0x9c;
const WEAPON_ENERGY_BYTES: usize = 12;
const OBJECT_ID_TABLE: usize = 0x400;
const OBJECT_FLAG_TABLE: usize = 0x420;
const OBJECT_SLOTS: usize = 0x20;
const OBJECT_ACTIVE: u8 = 0x80;
const ITEM_OBJECT_FIRST: u8 = 0x38;
const ITEM_OBJECT_LAST: u8 = 0x3a;
const BOSS_HEALTH: usize = 0x6c1;
const BOSS_PHASE: usize = 0xb1;
const CURRENT_BOSS: usize = 0xb3;
const WILY_MACHINE_BOSS: u8 = 12;
const WILY_MACHINE_REFILL_PHASE: u8 = 4;
const WILY5_REFIGHTING_MASK: usize = 0xbc;
const BOOBEAM_STAGE: u8 = 11;
pub const WILY5_STAGE: u8 = 12;
pub const WILY5_REFIGHTS_COMPLETE: u8 = 0xff;
pub const WILY5_REFIGHT_HUB: u8 = 0xff;
const BOOBEAM_TARGET_FIRST_SLOT: usize = 20;
const BOOBEAM_TARGET_LAST_SLOT: usize = 29;
const BOOBEAM_TRAP_ID: u8 = 109;
const BOOBEAM_BARRIER_ID: u8 = 87;
const CRASH_WEAPON_ENERGY_INDEX: usize = 7;

const BOSS_PHASE_FIGHTING: u8 = 0x02;
const BOSS_PHASE_NONE: u8 = 0x00;
pub const BOSS_PHASE_DEFEATED: u8 = 0xfe;
pub const FULL_BOSS_HEALTH: u8 = 28;

const PLAYER_STATE_STANDING: u8 = 0x03;
const PLAYER_STATE_HIT: u8 = 0x02;
const PLAYER_STATE_AIRBORNE: u8 = 0x06;
const PLAYER_STATE_LADDER: u8 = 0x09;
const PLAYER_STATE_LADDER_TOP: u8 = 0x0a;
pub const POSTURE_GROUNDED: u8 = 0;
pub const POSTURE_AIRBORNE: u8 = 1;
pub const POSTURE_LADDER: u8 = 2;
const PLAYER_STATE_FALLEN: u8 = 0x01;
const PLAYER_STATE_DYING: u8 = 0x00;
const PLAYER_STATE_TELEPORTING: u8 = 0x0b;
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

pub type Mm2Input = crate::search::archive::Input<ButtonChord>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Mm2Scene {
    #[default]
    Unknown,
    Title,
    StageSelect,
    Gameplay,
    Death,
    Ending,
}

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
    pub boobeam_targets: u16,
    pub crash_shots: u8,
    pub refighting_mask: u8,
    pub refight_boss: u8,
    pub wily_machine_shell_broken: bool,
    pub camera_state: u8,
    pub enemy_damage: u8,
    pub castle_clears: u8,
    pub weapon_energies: [u8; WEAPON_ENERGY_BYTES],
    pub stage_select_cursor: u8,
    pub scene: Mm2Scene,
    pub current_bank: u8,
    pub ppu_ctrl: u8,
    pub menu_cursor: u8,
    pub menu_page: u8,
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
    pub fn posture(self) -> u8 {
        match self.player_state {
            PLAYER_STATE_AIRBORNE | PLAYER_STATE_HIT => POSTURE_AIRBORNE,
            PLAYER_STATE_LADDER | PLAYER_STATE_LADDER_TOP => POSTURE_LADDER,
            _ => POSTURE_GROUNDED,
        }
    }

    #[must_use]
    pub fn boss_damage(self) -> u8 {
        if self.wily_machine_shell_broken && self.boss_phase == WILY_MACHINE_REFILL_PHASE {
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
    pub fn is_dead(self) -> bool {
        self.health == 0
            || (self.player_state == PLAYER_STATE_FALLEN && self.y >= BELOW_PLAY_AREA_Y)
    }

    #[must_use]
    pub fn is_dying(self) -> bool {
        self.player_state == PLAYER_STATE_DYING
    }
}

pub fn decode_state(wram: &[u8]) -> Result<Mm2MechanicalState, MachineError> {
    decode_state_with_expected_stage(wram, None)
}

fn decode_state_for_stage(
    wram: &[u8],
    expected_stage: u8,
) -> Result<Mm2MechanicalState, MachineError> {
    decode_state_with_expected_stage(wram, Some(expected_stage))
}

fn decode_state_with_expected_stage(
    wram: &[u8],
    expected_stage: Option<u8>,
) -> Result<Mm2MechanicalState, MachineError> {
    let raw_stage = read_byte(wram, STAGE)?;
    let player_state = read_byte(wram, PLAYER_STATE)?;
    let boss_phase = read_byte(wram, BOSS_PHASE)?;
    let stage = semantic_stage(raw_stage, player_state, boss_phase, expected_stage);
    let weapon_energies: [u8; WEAPON_ENERGY_BYTES] = (WEAPON_ENERGY
        ..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
        .map(|index| read_byte(wram, index))
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| {
            MachineError::Backend("Mega Man 2 weapon energy table is incomplete".into())
        })?;
    let menu = menu_selection(wram)?;
    let state = Mm2MechanicalState {
        stage,
        screen: read_byte(wram, PLAYER_SCREEN)?,
        room: read_byte(wram, LEVEL_ROOM)?,
        x: read_byte(wram, PLAYER_X)?,
        y: read_byte(wram, PLAYER_Y)?,
        health: read_byte(wram, PLAYER_HEALTH)?,
        weapon_energy: weapon_energies
            .iter()
            .map(|energy| u16::from(*energy))
            .sum(),
        equipped_energy: match read_byte(wram, SELECTED_WEAPON)? {
            0 => 0,
            weapon if usize::from(weapon) <= WEAPON_ENERGY_BYTES => {
                weapon_energies[usize::from(weapon) - 1]
            }
            _ => 0,
        },
        platforms: live_platforms(wram)?,
        lives: read_byte(wram, LIVES)?,
        player_state,
        weapon: read_byte(wram, SELECTED_WEAPON)?,
        menu,
        weapons_obtained: read_byte(wram, WEAPONS_OBTAINED)?,
        boss_health: read_byte(wram, BOSS_HEALTH)?,
        boss_phase,
        boobeam_targets: boobeam_targets(wram, stage, boss_phase)?,
        crash_shots: crash_shots(wram, stage, boss_phase)?,
        refighting_mask: refighting_mask(wram, stage)?,
        refight_boss: refight_boss(wram, stage, boss_phase)?,
        wily_machine_shell_broken: stage == WILY5_STAGE
            && read_byte(wram, CURRENT_BOSS)? == WILY_MACHINE_BOSS
            && (WILY_MACHINE_REFILL_PHASE..BOSS_PHASE_DEFEATED).contains(&boss_phase),
        camera_state: read_byte(wram, CAMERA_STATE)?,
        enemy_damage: 0,
        castle_clears: 0,
        weapon_energies,
        stage_select_cursor: MENU_CLOSED,
        scene: Mm2Scene::Unknown,
        current_bank: read_byte(wram, CURRENT_BANK)?,
        ppu_ctrl: read_byte(wram, MENU_MODE)?,
        menu_cursor: read_byte(wram, MENU_CURSOR)?,
        menu_page: read_byte(wram, MENU_PAGE)?,
    };
    Ok(state)
}

fn semantic_stage(
    raw_stage: u8,
    player_state: u8,
    boss_phase: u8,
    expected_stage: Option<u8>,
) -> u8 {
    if expected_stage == Some(WILY5_STAGE)
        && raw_stage < WILY5_STAGE - 4
        && player_state == PLAYER_STATE_TELEPORTING
        && boss_phase == BOSS_PHASE_NONE
    {
        WILY5_STAGE
    } else {
        raw_stage
    }
}

fn menu_selection(wram: &[u8]) -> Result<u8, MachineError> {
    if read_byte(wram, CURRENT_BANK)? != MENU_BANK {
        return Ok(MENU_CLOSED);
    }
    let page = read_byte(wram, MENU_PAGE)?;
    let cursor = read_byte(wram, MENU_CURSOR)?;
    let rows = match page {
        0 => MENU_ROWS,
        1 => MENU_ROWS - 1,
        _ => return Ok(MENU_UNKNOWN),
    };
    if cursor < rows {
        Ok(page * MENU_ROWS + cursor)
    } else {
        Ok(MENU_UNKNOWN)
    }
}

fn is_wily5_stage_borrow(wram: &[u8], expected_stage: Option<u8>) -> bool {
    expected_stage == Some(WILY5_STAGE)
        && wram
            .get(STAGE)
            .copied()
            .is_some_and(|stage| stage < WILY5_STAGE - 4)
        && wram.get(PLAYER_STATE).copied() == Some(PLAYER_STATE_TELEPORTING)
        && wram.get(BOSS_PHASE).copied() == Some(BOSS_PHASE_NONE)
}

fn boobeam_targets(wram: &[u8], stage: u8, boss_phase: u8) -> Result<u16, MachineError> {
    if !wily4_boss_active(stage, boss_phase) {
        return Ok(0);
    }
    let mut targets = 0_u16;
    for slot in BOOBEAM_TARGET_FIRST_SLOT..=BOOBEAM_TARGET_LAST_SLOT {
        let id = read_byte(wram, OBJECT_ID_TABLE + slot)?;
        let flags = read_byte(wram, OBJECT_FLAG_TABLE + slot)?;
        if flags & OBJECT_ACTIVE != 0 && (id == BOOBEAM_TRAP_ID || id == BOOBEAM_BARRIER_ID) {
            targets |= 1_u16 << (slot - BOOBEAM_TARGET_FIRST_SLOT);
        }
    }
    Ok(targets)
}

fn crash_shots(wram: &[u8], stage: u8, boss_phase: u8) -> Result<u8, MachineError> {
    if !wily4_boss_active(stage, boss_phase) {
        return Ok(0);
    }
    Ok(read_byte(wram, WEAPON_ENERGY + CRASH_WEAPON_ENERGY_INDEX)?.min(28) / 4)
}

fn refighting_mask(wram: &[u8], stage: u8) -> Result<u8, MachineError> {
    if stage != WILY5_STAGE {
        return Ok(0);
    }
    read_byte(wram, WILY5_REFIGHTING_MASK)
}

fn refight_boss(wram: &[u8], stage: u8, boss_phase: u8) -> Result<u8, MachineError> {
    if stage != WILY5_STAGE || !wily5_boss_active(boss_phase) {
        return Ok(WILY5_REFIGHT_HUB);
    }
    read_byte(wram, CURRENT_BOSS)
}

fn wily4_boss_active(stage: u8, boss_phase: u8) -> bool {
    stage == BOOBEAM_STAGE && (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&boss_phase)
}

fn wily5_boss_active(boss_phase: u8) -> bool {
    (BOSS_PHASE_FIGHTING..BOSS_PHASE_DEFEATED).contains(&boss_phase)
}

fn should_settle_award(state: Mm2MechanicalState, genesis_stage: u8, genesis_weapons: u8) -> bool {
    state.boss_phase >= BOSS_PHASE_DEFEATED
        && state.weapons_obtained & !genesis_weapons == 0
        && state.stage == genesis_stage
        && !state.is_dead()
        && (state.stage != WILY5_STAGE || state.refighting_mask == WILY5_REFIGHTS_COMPLETE)
}

fn ablation_identity_changed(left: Mm2MechanicalState, right: Mm2MechanicalState) -> bool {
    left.boobeam_targets != right.boobeam_targets
        || left.crash_shots != right.crash_shots
        || left.refighting_mask != right.refighting_mask
        || left.refight_boss != right.refight_boss
        || left.wily_machine_shell_broken != right.wily_machine_shell_broken
}

fn coherent_world_violation(
    genesis_stage: u8,
    state: Mm2MechanicalState,
    scroll_direction: u8,
) -> bool {
    state.stage == genesis_stage
        && scroll_direction == 0
        && state.health > 0
        && !state.is_dying()
        && state.screen.abs_diff(state.room) > 1
}

fn enemy_damage_between(prior: &[u8], current: &[u8]) -> u8 {
    (ENEMY_HEALTH_TABLE_START..ENEMY_HEALTH_TABLE_END)
        .filter_map(|address| {
            let enemy = address - ENEMY_HEALTH_TABLE_START;
            let slot = enemy + 16;
            let before = *prior.get(address)?;
            let after = *current.get(address)?;
            let active = *prior.get(OBJECT_FLAG_TABLE + slot)? & OBJECT_ACTIVE != 0;
            let hit = *current.get(ENEMY_HIT_FLAGS + enemy)? != 0;
            let same_enemy = prior.get(OBJECT_ID_TABLE + slot)
                == current.get(OBJECT_ID_TABLE + slot)
                && prior.get(ENEMY_INDEX_TABLE + enemy) == current.get(ENEMY_INDEX_TABLE + enemy);
            let killed =
                after == 0 && current.get(OBJECT_ID_TABLE + slot).copied() == Some(KILLED_OBJECT);
            (active && hit && (same_enemy || killed) && after < before)
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

fn dying_run_for_frame(previous: u32, state: Mm2MechanicalState) -> u32 {
    if state.is_dying() {
        previous.saturating_add(1)
    } else {
        0
    }
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
pub fn preference_tuple(state: Mm2MechanicalState) -> (u8, u8, u16) {
    (state.bosses_beaten(), state.health, state.weapon_energy)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Observations {
    pub frame_count: u64,
    pub decoded: Mm2MechanicalState,
    pub changed_indices: Vec<u16>,
    pub dead: bool,
    #[serde(default)]
    pub fall_run: u16,
    #[serde(default)]
    pub dying_run: u32,
    pub log_line: String,
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
    coherent_world: bool,
    whole_game: bool,
    trusted_stage: Option<u8>,
    ending_reached: bool,
    final_stage_seen: bool,
    castle_completion_mask: u8,
}

impl Mm2Snapshot {
    #[cfg(test)]
    pub(crate) fn for_census_tests(decoded: Mm2MechanicalState) -> Self {
        Self {
            emulator_state: Vec::new(),
            observation: Mm2Observations {
                frame_count: 0,
                decoded,
                changed_indices: Vec::new(),
                dead: false,
                fall_run: 0,
                dying_run: 0,
                log_line: String::new(),
            },
            failed: false,
            coherent_world: false,
            whole_game: false,
            trusted_stage: None,
            ending_reached: false,
            final_stage_seen: false,
            castle_completion_mask: 0,
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
    coherent_world: bool,
    whole_game: bool,
    trusted_stage: Option<u8>,
    genesis_trusted_stage: Option<u8>,
    ending_reached: bool,
    genesis_ending_reached: bool,
    final_stage_seen: bool,
    genesis_final_stage_seen: bool,
    castle_completion_mask: u8,
    genesis_castle_completion_mask: u8,
}

struct RenderTracking {
    prior_wram: [u8; WRAM_SIZE],
    prior_state: Mm2MechanicalState,
    enemy_damage: u8,
    expected_stage: Option<u8>,
    whole_game: bool,
    final_stage_seen: bool,
    ending_reached: bool,
    castle_completion_mask: u8,
}

impl RenderTracking {
    fn new(
        prior_wram: [u8; WRAM_SIZE],
        prior_state: Mm2MechanicalState,
        expected_stage: Option<u8>,
        whole_game: bool,
        castle_completion_mask: u8,
    ) -> Self {
        Self {
            prior_wram,
            prior_state,
            enemy_damage: prior_state.enemy_damage,
            expected_stage,
            whole_game,
            final_stage_seen: final_completion_marker(prior_state),
            ending_reached: prior_state.scene == Mm2Scene::Ending,
            castle_completion_mask,
        }
    }

    fn update(&mut self, wram: [u8; WRAM_SIZE]) -> Result<Mm2MechanicalState, MachineError> {
        let mut state = decode_state_with_expected_stage(&wram, self.expected_stage)?;
        if self.whole_game {
            if final_completion_marker(self.prior_state) || final_completion_marker(state) {
                self.final_stage_seen = true;
            }
            self.ending_reached = self.ending_reached
                || ending_transition_seen(self.prior_state, state)
                || (self.final_stage_seen && ending_scene_marker(state));
            if progress_reset(self.prior_state, state) {
                self.castle_completion_mask = 0;
            }
            if let Some(bit) = castle_clear_bit(self.prior_state, state) {
                self.castle_completion_mask |= bit;
            }
            if !is_wily5_stage_borrow(&wram, self.expected_stage) {
                self.expected_stage = Some(state.stage);
            }
            state.scene = scene_for_wram(self.ending_reached);
            state.castle_clears = castle_clear_count(self.castle_completion_mask);
            state.stage_select_cursor = if state.scene == Mm2Scene::StageSelect {
                state.stage
            } else {
                MENU_CLOSED
            };
        }
        if state.screen != self.prior_state.screen || state.stage != self.prior_state.stage {
            self.enemy_damage = 0;
        }
        self.enemy_damage = self
            .enemy_damage
            .saturating_add(enemy_damage_between(&self.prior_wram, &wram))
            .min(ENEMY_DAMAGE_CAP);
        state.enemy_damage = self.enemy_damage;
        self.prior_wram = wram;
        self.prior_state = state;
        Ok(state)
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
const FINAL_STAGE: u8 = MM2_LAST_WILY_STAGE + 1;
const ENDING_SCENE_STAGE: u8 = 5;
const ENDING_BOSS_PHASE: u8 = 0xff;
const CASTLE_COMPLETION_MASK: u8 = 0x3f;
const STAGE_SELECT_WALK_ROUNDS: usize = 16;

fn at_stage_select(wram: &[u8]) -> bool {
    wram.get(MENU_MODE).copied() == Some(MENU_MODE_STAGE_SELECT)
}

fn ending_scene_marker(state: Mm2MechanicalState) -> bool {
    state.stage == ENDING_SCENE_STAGE
        && state.boss_phase == ENDING_BOSS_PHASE
        && state.weapons_obtained == u8::MAX
}

fn final_completion_marker(state: Mm2MechanicalState) -> bool {
    state.stage == FINAL_STAGE
        && state.boss_phase == ENDING_BOSS_PHASE
        && state.weapons_obtained == u8::MAX
}

fn castle_clear_bit(previous: Mm2MechanicalState, state: Mm2MechanicalState) -> Option<u8> {
    if (MM2_FIRST_WILY_STAGE..=MM2_LAST_WILY_STAGE).contains(&previous.stage)
        && state.stage == previous.stage.saturating_add(1)
        && previous.boss_phase == ENDING_BOSS_PHASE
    {
        Some(1 << (previous.stage - MM2_FIRST_WILY_STAGE))
    } else {
        None
    }
}

fn progress_reset(previous: Mm2MechanicalState, state: Mm2MechanicalState) -> bool {
    previous.weapons_obtained != 0 && state.weapons_obtained == 0
}

fn castle_clear_count(mask: u8) -> u8 {
    (mask & CASTLE_COMPLETION_MASK)
        .count_ones()
        .try_into()
        .unwrap_or(u8::MAX)
}

fn ending_transition_seen(previous: Mm2MechanicalState, state: Mm2MechanicalState) -> bool {
    final_completion_marker(previous) && ending_scene_marker(state)
}

fn scene_for_wram(ending_reached: bool) -> Mm2Scene {
    if ending_reached {
        Mm2Scene::Ending
    } else {
        Mm2Scene::Unknown
    }
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

fn run_chords(machine: &mut QuickNesMachine, chords: &[ButtonChord]) -> Result<(), MachineError> {
    let here = machine.snapshot()?;
    machine.branch(here, &nes::reproducer(chords))?;
    machine.run(StopConditions::default(), None)?;
    machine.drop_snapshot(here)
}

impl Mm2Target {
    pub fn from_rom_bytes_whole_game(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let prefix = power_on_walk();
        let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        for chunk in prefix.chunks(64) {
            run_chords(&mut machine, chunk)?;
        }
        Self::from_machine_whole_game(machine, &prefix)
    }

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
        Self::from_rom_bytes_after_with_coherent_world(
            rom,
            core_path,
            core_sha256,
            prefix,
            stage,
            false,
        )
    }

    pub fn from_rom_bytes_after_with_coherent_world(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
        stage: Mm2Stage,
        coherent_world: bool,
    ) -> Result<Self, MachineError> {
        Self::from_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            prefix,
            stage,
            coherent_world,
        )
    }

    fn from_machine(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
        stage: Mm2Stage,
        coherent_world: bool,
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
            let state = decode_state_for_stage(&wram, stage.number())?;
            if state.stage == stage.number()
                && state.player_state == PLAYER_STATE_STANDING
                && state.health == FULL_HEALTH
            {
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
        let state = decode_state_for_stage(&wram, stage.number())?;
        let genesis = machine.snapshot()?;
        let observation = Mm2Observations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            dead: false,
            fall_run: 0,
            dying_run: 0,
            log_line: "frame=0 changed=[]".to_owned(),
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
            coherent_world,
            whole_game: false,
            trusted_stage: Some(stage.number()),
            genesis_trusted_stage: Some(stage.number()),
            ending_reached: false,
            genesis_ending_reached: false,
            final_stage_seen: false,
            genesis_final_stage_seen: false,
            castle_completion_mask: 0,
            genesis_castle_completion_mask: 0,
        })
    }

    fn from_machine_whole_game(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let wram = machine.read_wram()?;
        let mut state = decode_state(&wram)?;
        state.scene = Mm2Scene::StageSelect;
        state.stage_select_cursor = state.stage;
        let observation = Mm2Observations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            dead: false,
            fall_run: 0,
            dying_run: 0,
            log_line: "frame=0 changed=[]".to_owned(),
        };
        let genesis = machine.snapshot()?;
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
            genesis_prefix: prefix.to_vec(),
            execution_work: 0,
            coherent_world: false,
            whole_game: true,
            trusted_stage: None,
            genesis_trusted_stage: None,
            ending_reached: false,
            genesis_ending_reached: false,
            final_stage_seen: false,
            genesis_final_stage_seen: false,
            castle_completion_mask: 0,
            genesis_castle_completion_mask: 0,
        })
    }

    #[must_use]
    pub fn genesis_prefix(&self) -> &[ButtonChord] {
        &self.genesis_prefix
    }

    pub fn advance_genesis(&mut self, actions: &[ButtonChord]) -> Result<(), MachineError> {
        for action in actions {
            if !self.whole_game && (self.failed || self.is_dead() || self.defeated_a_boss()) {
                return Err(MachineError::Backend(
                    "root input reached a terminal state".into(),
                ));
            }
            let before = self.execution_work;
            self.apply(action);
            if self.execution_work.saturating_sub(before) != u64::from(action.bounded_hold_frames())
            {
                return Err(MachineError::Backend(
                    "root action inserted unrecorded transition frames".into(),
                ));
            }
        }
        if !self.whole_game && (self.failed || self.is_dead() || self.defeated_a_boss()) {
            return Err(MachineError::Backend(
                "root input ended in a terminal state".into(),
            ));
        }
        let genesis = self.machine.snapshot()?;
        self.machine.drop_snapshot(self.genesis)?;
        self.genesis = genesis;
        self.genesis_wram = self.current_wram;
        self.genesis_observation = self.observation.clone();
        self.genesis_trusted_stage = self.trusted_stage;
        self.genesis_ending_reached = self.ending_reached;
        self.genesis_final_stage_seen = self.final_stage_seen;
        self.genesis_castle_completion_mask = self.castle_completion_mask;
        self.genesis_prefix.extend_from_slice(actions);
        self.action_observations = vec![self.observation.clone()];
        self.execution_work = 0;
        Ok(())
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
    pub fn defeated_a_boss(&self) -> bool {
        if self.whole_game {
            return false;
        }
        let state = self.observation.decoded;
        state.weapons_obtained & !self.genesis_weapons != 0
            || state.stage > self.genesis_observation.decoded.stage
    }

    pub fn start_capturing(&mut self) {
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
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
    pub fn coherent_world(&self) -> bool {
        self.coherent_world
    }

    #[must_use]
    pub fn is_whole_game(&self) -> bool {
        self.whole_game
    }

    #[must_use]
    pub fn ending_reached(&self) -> bool {
        self.ending_reached
    }

    #[must_use]
    pub fn castle_clears(&self) -> u8 {
        castle_clear_count(self.castle_completion_mask)
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[Mm2Observations] {
        &self.action_observations
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.whole_game || self.failed || self.is_dead() || self.defeated_a_boss() || frames == 0
        {
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
                let state = decode_state_for_stage(wram, self.genesis_observation.decoded.stage)?;
                if state.is_dead()
                    || (self.coherent_world
                        && coherent_world_violation(
                            self.genesis_observation.decoded.stage,
                            state,
                            wram[SCROLL_DIRECTION],
                        ))
                {
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
            let mut tracking = RenderTracking::new(
                self.current_wram,
                self.observation.decoded,
                if self.whole_game {
                    self.trusted_stage
                } else {
                    Some(self.genesis_observation.decoded.stage)
                },
                self.whole_game,
                self.castle_completion_mask,
            );
            for action in &input.actions {
                self.render_action(
                    *action,
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut tracking,
                )?;
            }
            let input_endpoint = tracking.prior_state;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                let hold = u8::try_from(hold)?;
                self.render_action(
                    ButtonChord::new(0, hold),
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut tracking,
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
        tracking: &mut RenderTracking,
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
            let wram = self.machine.read_wram()?;
            let state = tracking.update(wram)?;
            self.current_wram = wram;
            self.observation.decoded = state;
            if self.whole_game {
                self.trusted_stage = tracking.expected_stage;
                self.final_stage_seen = tracking.final_stage_seen;
                self.ending_reached = tracking.ending_reached;
                self.castle_completion_mask = tracking.castle_completion_mask;
            }
            self.observation.frame_count = self.observation.frame_count.saturating_add(1);
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

    fn make_observation(
        &self,
        frame_count: u64,
        state: Mm2MechanicalState,
        wram: &[u8; WRAM_SIZE],
        prior_wram: &[u8; WRAM_SIZE],
    ) -> Mm2Observations {
        let changed_indices = wram
            .iter()
            .zip(prior_wram)
            .enumerate()
            .filter_map(|(index, (current, prior))| {
                (current != prior)
                    .then(|| u16::try_from(index).ok())
                    .flatten()
            })
            .collect::<Vec<_>>();
        Mm2Observations {
            frame_count,
            decoded: state,
            changed_indices: changed_indices.clone(),
            dead: state.is_dead(),
            fall_run: 0,
            dying_run: 0,
            log_line: format!("frame={frame_count} changed={changed_indices:?}"),
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
        self.trusted_stage = self.genesis_trusted_stage;
        self.ending_reached = self.genesis_ending_reached;
        self.final_stage_seen = self.genesis_final_stage_seen;
        self.castle_completion_mask = self.genesis_castle_completion_mask;
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || (!self.whole_game && (self.is_dead() || self.defeated_a_boss())) {
            return;
        }
        let Some(mut frames) = self.run_action(action) else {
            self.failed = true;
            return;
        };
        let mut waited = 0;
        while !self.whole_game
            && waited < AWARD_SETTLE_FRAMES
            && frames.last().is_some_and(|wram| {
                decode_state_for_stage(wram, self.genesis_observation.decoded.stage).is_ok_and(
                    |state| {
                        should_settle_award(
                            state,
                            self.genesis_observation.decoded.stage,
                            self.genesis_weapons,
                        )
                    },
                )
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
        let mut died = if self.whole_game {
            false
        } else {
            self.observation.dead
        };
        let mut dying_run = self.observation.dying_run;
        let mut fall_run = self.observation.fall_run;
        let mut enemy_damage = self.observation.decoded.enemy_damage;
        let mut prior_frame = self.current_wram;
        let expected_stage = if self.whole_game {
            self.trusted_stage
        } else {
            Some(self.genesis_observation.decoded.stage)
        };
        let Ok(mut previous) = decode_state_with_expected_stage(&prior_frame, expected_stage)
        else {
            self.failed = true;
            return;
        };
        for (offset, wram) in frames.iter().enumerate() {
            let expected_stage = if self.whole_game {
                self.trusted_stage
            } else {
                Some(self.genesis_observation.decoded.stage)
            };
            let Ok(mut state) = decode_state_with_expected_stage(wram, expected_stage) else {
                self.failed = true;
                return;
            };
            if self.whole_game {
                if final_completion_marker(previous) || final_completion_marker(state) {
                    self.final_stage_seen = true;
                }
                self.ending_reached = self.ending_reached
                    || ending_transition_seen(previous, state)
                    || (self.final_stage_seen && ending_scene_marker(state));
                if progress_reset(previous, state) {
                    self.castle_completion_mask = 0;
                }
                if let Some(bit) = castle_clear_bit(previous, state) {
                    self.castle_completion_mask |= bit;
                }
                if !is_wily5_stage_borrow(wram, self.trusted_stage) {
                    self.trusted_stage = Some(state.stage);
                }
                state.scene = scene_for_wram(self.ending_reached);
                state.castle_clears = castle_clear_count(self.castle_completion_mask);
                state.stage_select_cursor = if state.scene == Mm2Scene::StageSelect {
                    state.stage
                } else {
                    MENU_CLOSED
                };
            }
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
            let coherent_violation = !self.whole_game
                && self.coherent_world
                && coherent_world_violation(genesis.stage, state, wram[SCROLL_DIRECTION]);
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
            dying_run = dying_run_for_frame(dying_run, state);
            let dead = if self.whole_game {
                state.is_dead() || state.is_dying()
            } else {
                died || state.is_dead()
                    || dying_run >= DYING_FRAMES
                    || state.lives < genesis.lives
                    || state.stage < genesis.stage
                    || escaped
                    || fell_out
                    || coherent_violation
            };
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
                || ablation_identity_changed(state, prior_state)
                || state.castle_clears != prior_state.castle_clears
                || dead != died;
            died = dead;
            if boundary {
                let frame_count = self
                    .observation
                    .frame_count
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
                let mut observation = self.make_observation(frame_count, state, wram, &prior_wram);
                observation.dead = dead;
                observation.fall_run = fall_run;
                observation.dying_run = dying_run;
                observations.push(observation);
                prior_wram = *wram;
                prior_state = state;
                emitted = true;
            }
        }
        let endpoint_wram = frames.last().copied().unwrap_or(prior_wram);
        let endpoint_frame = self
            .observation
            .frame_count
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        if !emitted
            || !observations
                .last()
                .is_some_and(|observation| observation.frame_count == endpoint_frame)
        {
            let expected_stage = if self.whole_game {
                self.trusted_stage
            } else {
                Some(self.genesis_observation.decoded.stage)
            };
            let Ok(mut endpoint_state) =
                decode_state_with_expected_stage(&endpoint_wram, expected_stage)
            else {
                self.failed = true;
                return;
            };
            if self.whole_game {
                if !is_wily5_stage_borrow(&endpoint_wram, self.trusted_stage) {
                    self.trusted_stage = Some(endpoint_state.stage);
                }
                endpoint_state.scene = scene_for_wram(self.ending_reached);
                endpoint_state.castle_clears = castle_clear_count(self.castle_completion_mask);
                endpoint_state.stage_select_cursor =
                    if endpoint_state.scene == Mm2Scene::StageSelect {
                        endpoint_state.stage
                    } else {
                        MENU_CLOSED
                    };
            }
            endpoint_state.enemy_damage = enemy_damage;
            let mut observation =
                self.make_observation(endpoint_frame, endpoint_state, &endpoint_wram, &prior_wram);
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
        let Ok(snap) = self.machine.snapshot() else {
            self.failed = true;
            return None;
        };
        let Ok(emulator_state) = self.machine.take_snapshot(snap) else {
            self.failed = true;
            return None;
        };
        Some(Mm2Snapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
            coherent_world: self.coherent_world,
            whole_game: self.whole_game,
            trusted_stage: self.trusted_stage,
            ending_reached: self.ending_reached,
            final_stage_seen: self.final_stage_seen,
            castle_completion_mask: self.castle_completion_mask,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        if snapshot.coherent_world != self.coherent_world {
            return Err("Mega Man 2 snapshot coherent-world policy does not match".into());
        }
        if snapshot.whole_game != self.whole_game {
            return Err("Mega Man 2 snapshot whole-game policy does not match".into());
        }
        self.machine
            .restore_bytes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        self.current_wram = self
            .machine
            .read_wram()
            .map_err(|error| error.to_string())?;
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        self.trusted_stage = snapshot.trusted_stage;
        self.ending_reached = snapshot.ending_reached;
        self.final_stage_seen = snapshot.final_stage_seen;
        self.castle_completion_mask = snapshot.castle_completion_mask;
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
    fn coherent_world_rejects_idle_same_stage_screen_gaps_only() {
        let normal = Mm2MechanicalState {
            stage: 11,
            screen: 33,
            room: 31,
            health: FULL_HEALTH,
            player_state: PLAYER_STATE_AIRBORNE,
            ..Mm2MechanicalState::default()
        };
        assert!(coherent_world_violation(11, normal, 0));
        assert!(!coherent_world_violation(11, normal, 1));
        assert!(!coherent_world_violation(
            11,
            Mm2MechanicalState {
                screen: 32,
                ..normal
            },
            0
        ));
        assert!(!coherent_world_violation(10, normal, 0));
        assert!(!coherent_world_violation(
            11,
            Mm2MechanicalState {
                player_state: PLAYER_STATE_DYING,
                ..normal
            },
            0,
        ));
    }

    #[test]
    fn menu_detection_uses_menu_bank_instead_of_sprite_scratch() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[CURRENT_BANK] = 0x0e;
        wram[0x04] = 3;
        wram[MENU_CURSOR] = 15;
        assert_eq!(decode_state(&wram).expect("gameplay").menu, MENU_CLOSED);

        wram[CURRENT_BANK] = MENU_BANK;
        wram[0x04] = 0;
        wram[MENU_CURSOR] = 5;
        assert_eq!(decode_state(&wram).expect("opening menu").menu, 5);
        wram[MENU_PAGE] = 1;
        assert_eq!(decode_state(&wram).expect("second menu page").menu, 13);
    }

    #[test]
    fn menu_selection_collapses_invalid_register_pairs_to_unknown() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[CURRENT_BANK] = MENU_BANK;
        for page in 0..=u8::MAX {
            for cursor in 0..=u8::MAX {
                wram[MENU_PAGE] = page;
                wram[MENU_CURSOR] = cursor;
                let expected = match page {
                    0 if cursor < MENU_ROWS => cursor,
                    1 if cursor < MENU_ROWS - 1 => MENU_ROWS + cursor,
                    _ => MENU_UNKNOWN,
                };
                assert_eq!(menu_selection(&wram).expect("menu registers"), expected);
            }
        }
        for bank in [0, MENU_BANK - 1, MENU_BANK + 1, u8::MAX] {
            wram[CURRENT_BANK] = bank;
            wram[MENU_PAGE] = 1;
            wram[MENU_CURSOR] = 5;
            assert_eq!(menu_selection(&wram).expect("non-menu bank"), MENU_CLOSED);
        }
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
    fn wily4_targets_and_crash_shots_decode_from_stable_slots() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[STAGE] = BOOBEAM_STAGE;
        wram[BOSS_PHASE] = BOSS_PHASE_FIGHTING;
        wram[WEAPON_ENERGY + CRASH_WEAPON_ENERGY_INDEX] = 31;
        wram[OBJECT_ID_TABLE + 20] = BOOBEAM_TRAP_ID;
        wram[OBJECT_FLAG_TABLE + 20] = OBJECT_ACTIVE;
        wram[OBJECT_ID_TABLE + 21] = BOOBEAM_BARRIER_ID;
        wram[OBJECT_FLAG_TABLE + 21] = OBJECT_ACTIVE;
        wram[OBJECT_ID_TABLE + 22] = BOOBEAM_TRAP_ID;
        wram[OBJECT_ID_TABLE + 29] = BOOBEAM_BARRIER_ID;
        wram[OBJECT_FLAG_TABLE + 29] = OBJECT_ACTIVE;
        wram[OBJECT_ID_TABLE + 30] = BOOBEAM_TRAP_ID;
        wram[OBJECT_FLAG_TABLE + 30] = OBJECT_ACTIVE;

        let state = decode_state(&wram).expect("decode Wily4 targets");
        assert_eq!(state.boobeam_targets, 0b10_0000_0011);
        assert_eq!(state.crash_shots, 7);

        wram[BOSS_PHASE] = BOSS_PHASE_DEFEATED;
        assert_eq!(
            decode_state(&wram).expect("defeated boss").boobeam_targets,
            0
        );
        assert_eq!(decode_state(&wram).expect("defeated boss").crash_shots, 0);
        wram[BOSS_PHASE] = BOSS_PHASE_NONE;
        let hub = decode_state(&wram).expect("boss hub");
        assert_eq!(hub.boobeam_targets, 0);
        assert_eq!(hub.crash_shots, 0);
        wram[BOSS_PHASE] = BOSS_PHASE_FIGHTING;
        wram[STAGE] = 10;
        let other_stage = decode_state(&wram).expect("other stage");
        assert_eq!(other_stage.boobeam_targets, 0);
        assert_eq!(other_stage.crash_shots, 0);
    }

    #[test]
    fn wily5_refight_state_is_gated_and_normalizes_inactive_boss_identity() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[STAGE] = WILY5_STAGE;
        wram[BOSS_PHASE] = BOSS_PHASE_FIGHTING;
        wram[WILY5_REFIGHTING_MASK] = 0xa5;
        wram[CURRENT_BOSS] = 4;

        let active = decode_state(&wram).expect("active Wily5 refight");
        assert_eq!(active.refighting_mask, 0xa5);
        assert_eq!(active.refight_boss, 4);

        wram[BOSS_PHASE] = 1;
        let intro = decode_state(&wram).expect("Wily5 boss intro");
        assert_eq!(intro.refighting_mask, 0xa5);
        assert_eq!(intro.refight_boss, WILY5_REFIGHT_HUB);

        wram[BOSS_PHASE] = BOSS_PHASE_NONE;
        let hub = decode_state(&wram).expect("Wily5 hub");
        assert_eq!(hub.refighting_mask, 0xa5);
        assert_eq!(hub.refight_boss, WILY5_REFIGHT_HUB);

        wram[STAGE] = BOOBEAM_STAGE;
        let other_stage = decode_state(&wram).expect("non-Wily5 stage");
        assert_eq!(other_stage.refighting_mask, 0);
        assert_eq!(other_stage.refight_boss, WILY5_REFIGHT_HUB);
    }

    #[test]
    fn wily5_teleport_stage_borrow_requires_trusted_wily5_genesis() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[STAGE] = 7;
        wram[PLAYER_STATE] = PLAYER_STATE_TELEPORTING;
        wram[BOSS_PHASE] = BOSS_PHASE_NONE;
        wram[WILY5_REFIGHTING_MASK] = 0xa5;
        wram[CURRENT_BOSS] = 4;

        let borrowed = decode_state_for_stage(&wram, WILY5_STAGE).expect("Wily5 stage borrow");
        assert_eq!(borrowed.stage, WILY5_STAGE);
        assert_eq!(borrowed.refighting_mask, 0xa5);
        assert_eq!(borrowed.refight_boss, WILY5_REFIGHT_HUB);

        assert_eq!(
            decode_state_for_stage(&wram, 7)
                .expect("ordinary stage 7")
                .stage,
            7
        );
        wram[STAGE] = BOOBEAM_STAGE;
        assert_eq!(
            decode_state_for_stage(&wram, BOOBEAM_STAGE)
                .expect("Wily4 teleport")
                .stage,
            BOOBEAM_STAGE
        );

        wram[BOSS_PHASE] = BOSS_PHASE_FIGHTING;
        wram[STAGE] = 7;
        assert_eq!(
            decode_state_for_stage(&wram, WILY5_STAGE)
                .expect("active stage")
                .stage,
            7
        );
    }

    #[test]
    fn machine_refill_is_not_damage_and_form_identity_is_encounter_specific() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        wram[STAGE] = WILY5_STAGE;
        wram[CURRENT_BOSS] = WILY_MACHINE_BOSS;
        wram[BOSS_PHASE] = WILY_MACHINE_REFILL_PHASE;
        for health in 1..=FULL_BOSS_HEALTH {
            wram[BOSS_HEALTH] = health;
            let state = decode_state(&wram).expect("machine refill");
            assert!(state.wily_machine_shell_broken);
            assert_eq!(state.boss_damage(), 0);
        }
        wram[BOSS_PHASE] = 5;
        wram[BOSS_HEALTH] = 20;
        let second = decode_state(&wram).expect("second form");
        assert!(second.wily_machine_shell_broken);
        assert_eq!(second.boss_damage(), 8);
        wram[BOSS_PHASE] = 3;
        assert!(
            !decode_state(&wram)
                .expect("first form")
                .wily_machine_shell_broken
        );
        wram[BOSS_PHASE] = WILY_MACHINE_REFILL_PHASE;
        wram[CURRENT_BOSS] = 4;
        let quick = decode_state(&wram).expect("other refight");
        assert!(!quick.wily_machine_shell_broken);
        assert_eq!(quick.boss_damage(), 8);
        wram[CURRENT_BOSS] = WILY_MACHINE_BOSS;
        wram[STAGE] = BOOBEAM_STAGE;
        assert!(
            !decode_state(&wram)
                .expect("other stage")
                .wily_machine_shell_broken
        );
    }

    #[test]
    fn decoded_state_preserves_uniform_weapon_energy_and_raw_menu_registers() {
        let mut wram = vec![0_u8; WRAM_SIZE];
        for (index, energy) in (WEAPON_ENERGY..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
            .zip(1_u8..=WEAPON_ENERGY_BYTES as u8)
        {
            wram[index] = energy;
        }
        wram[CURRENT_BANK] = MENU_BANK;
        wram[MENU_PAGE] = 1;
        wram[MENU_CURSOR] = 3;
        wram[MENU_MODE] = MENU_MODE_STAGE_SELECT;
        let state = decode_state(&wram).expect("decode menu state");
        assert_eq!(
            state.weapon_energies,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
        assert_eq!(state.weapon_energy, 78);
        assert_eq!(state.menu, 11);
        assert_eq!(state.menu_cursor, 3);
        assert_eq!(state.menu_page, 1);
        assert_eq!(state.ppu_ctrl, MENU_MODE_STAGE_SELECT);
        assert_eq!(state.current_bank, MENU_BANK);
        assert_eq!(state.scene, Mm2Scene::Unknown);
    }

    #[test]
    fn ending_marker_requires_the_source_backed_final_stage_transition() {
        let marker = Mm2MechanicalState {
            stage: ENDING_SCENE_STAGE,
            boss_phase: ENDING_BOSS_PHASE,
            weapons_obtained: u8::MAX,
            ..Mm2MechanicalState::default()
        };
        assert!(ending_scene_marker(marker));
        assert!(!ending_transition_seen(
            Mm2MechanicalState::default(),
            marker
        ));
        assert!(!final_completion_marker(Mm2MechanicalState {
            stage: FINAL_STAGE,
            ..Mm2MechanicalState::default()
        }));
        assert!(ending_transition_seen(
            Mm2MechanicalState {
                stage: FINAL_STAGE,
                boss_phase: ENDING_BOSS_PHASE,
                weapons_obtained: u8::MAX,
                ..Mm2MechanicalState::default()
            },
            marker,
        ));
        assert!(!ending_transition_seen(
            Mm2MechanicalState {
                stage: FINAL_STAGE,
                ..Mm2MechanicalState::default()
            },
            Mm2MechanicalState {
                boss_phase: BOSS_PHASE_NONE,
                weapons_obtained: u8::MAX,
                ..marker
            },
        ));
    }

    #[test]
    fn castle_completion_uses_each_source_stage_transition_once() {
        let mut mask = 0;
        for stage in MM2_FIRST_WILY_STAGE..=MM2_LAST_WILY_STAGE {
            let previous = Mm2MechanicalState {
                stage,
                boss_phase: ENDING_BOSS_PHASE,
                ..Mm2MechanicalState::default()
            };
            let next = Mm2MechanicalState {
                stage: stage + 1,
                ..previous
            };
            let bit = castle_clear_bit(previous, next).expect("Wily clear transition");
            mask |= bit;
            mask |= bit;
            assert_eq!(castle_clear_count(mask), stage - MM2_FIRST_WILY_STAGE + 1);
        }
        assert_eq!(mask, CASTLE_COMPLETION_MASK);

        let clear = Mm2MechanicalState {
            stage: MM2_FIRST_WILY_STAGE,
            boss_phase: ENDING_BOSS_PHASE,
            ..Mm2MechanicalState::default()
        };
        assert_eq!(castle_clear_bit(clear, clear), None);
        assert_eq!(
            castle_clear_bit(
                Mm2MechanicalState {
                    boss_phase: BOSS_PHASE_DEFEATED,
                    ..clear
                },
                Mm2MechanicalState {
                    stage: MM2_FIRST_WILY_STAGE + 1,
                    ..clear
                },
            ),
            None
        );
        assert_eq!(castle_clear_count(0x3f), 6);
        assert_eq!(castle_clear_count(0xff), 6);
        assert_eq!(castle_clear_count(1 | 1), 1);
    }

    #[test]
    fn castle_completion_resets_only_when_the_dedicated_weapon_flag_is_cleared() {
        let progressed = Mm2MechanicalState {
            weapons_obtained: 0x20,
            ..Mm2MechanicalState::default()
        };
        assert!(progress_reset(
            progressed,
            Mm2MechanicalState {
                weapons_obtained: 0,
                ..progressed
            }
        ));
        assert!(!progress_reset(
            progressed,
            Mm2MechanicalState {
                weapons_obtained: 0x20,
                ..progressed
            }
        ));
        assert!(!progress_reset(
            Mm2MechanicalState::default(),
            Mm2MechanicalState::default()
        ));
    }

    #[test]
    fn whole_game_scene_stays_unknown_without_a_trusted_lifecycle_event() {
        assert_eq!(scene_for_wram(false), Mm2Scene::Unknown);
        assert_eq!(scene_for_wram(true), Mm2Scene::Ending);
    }

    #[test]
    #[ignore = "requires the explicitly supplied ROM, QuickNES core, and oracle tape"]
    fn whole_game_replays_the_preserved_power_on_oracle_without_hidden_work() {
        use sha2::{Digest, Sha256};
        use std::{env, fs};

        let rom_path = env::var("HARMONY_MM2_ROM").expect("HARMONY_MM2_ROM");
        let core_path = env::var("HARMONY_QUICKNES_CORE").expect("HARMONY_QUICKNES_CORE");
        let input_path = env::var("HARMONY_MM2_ORACLE_INPUT").expect("HARMONY_MM2_ORACLE_INPUT");
        let rom = fs::read(&rom_path).expect("read MM2 ROM");
        let input: Mm2Input = serde_json::from_slice(
            &fs::read(&input_path).expect("read preserved MM2 oracle input"),
        )
        .expect("decode preserved MM2 oracle input");
        let prefix = power_on_walk();
        assert!(input.actions.len() > prefix.len());
        assert_eq!(&input.actions[..prefix.len()], prefix.as_slice());

        let core_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(&core_path).expect("read QuickNES core"))
        );
        let mut target =
            Mm2Target::from_rom_bytes_whole_game(&rom, Path::new(&core_path), &core_sha256)
                .expect("construct whole-game target");
        assert!(target.is_whole_game());
        assert_eq!(target.genesis_prefix(), prefix.as_slice());

        let expected_work = input.actions[prefix.len()..]
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum::<u64>();
        let ending_action = 6_697;
        let mut restored = false;
        let mut observed_clear_counts = Vec::new();
        for (absolute_index, action) in input.actions.iter().enumerate().skip(prefix.len()) {
            let previous_clears = target.castle_clears();
            target.apply(action);
            assert_eq!(target.exit_kind(), ExitKind::Ok, "action {absolute_index}");
            let current_clears = target.castle_clears();
            assert!(
                current_clears == previous_clears
                    || current_clears == previous_clears.saturating_add(1),
                "unexpected castle clear jump at action {absolute_index}: {previous_clears} -> {current_clears}"
            );
            if current_clears > previous_clears {
                observed_clear_counts.push(current_clears);
            }
            if absolute_index < ending_action {
                assert!(
                    !target.ending_reached(),
                    "ending reported at action {absolute_index}"
                );
            }
            if absolute_index == ending_action {
                assert!(
                    target.ending_reached(),
                    "ending missing at action {absolute_index}"
                );
            }
            if absolute_index == 1_000 {
                let snapshot = target.snapshot().expect("snapshot at oracle checkpoint");
                let observation = target.observe();
                let work = target.execution_work();
                let castle_clears = target.castle_clears();
                target
                    .restore(&snapshot)
                    .expect("restore oracle checkpoint");
                assert_eq!(target.observe(), observation);
                assert_eq!(target.execution_work(), work);
                assert_eq!(target.castle_clears(), castle_clears);
                assert_eq!(target.mechanical_state().castle_clears, castle_clears);
                restored = true;
            }
        }
        assert!(restored);
        assert_eq!(target.execution_work(), expected_work);
        assert!(target.ending_reached());
        assert_eq!(observed_clear_counts, vec![1, 2, 3, 4, 5, 6]);
        let endpoint = target.mechanical_state();
        assert_eq!(
            (
                endpoint.stage,
                endpoint.room,
                endpoint.screen,
                endpoint.health,
                endpoint.lives,
                endpoint.boss_phase,
                endpoint.weapons_obtained,
                endpoint.scene,
            ),
            (
                ENDING_SCENE_STAGE,
                0,
                0,
                6,
                2,
                ENDING_BOSS_PHASE,
                u8::MAX,
                Mm2Scene::Ending,
            )
        );
        assert_eq!(endpoint.castle_clears, 6);
        let raw = target.machine.read_wram().expect("read ending RAM");
        assert_eq!(raw[STAGE], ENDING_SCENE_STAGE);
        assert_eq!(raw[BOSS_PHASE], ENDING_BOSS_PHASE);
        assert_eq!(raw[PLAYER_HEALTH], 6);
        assert_eq!(raw[LIVES], 2);
        assert_eq!(raw[WEAPONS_OBTAINED], u8::MAX);
    }

    #[test]
    fn intermediate_wily5_awards_do_not_trigger_settling_wait() {
        let partial = Mm2MechanicalState {
            stage: WILY5_STAGE,
            health: FULL_HEALTH,
            boss_phase: BOSS_PHASE_DEFEATED,
            refighting_mask: 0x7f,
            ..Mm2MechanicalState::default()
        };
        assert!(!should_settle_award(partial, WILY5_STAGE, 0));

        let final_clear = Mm2MechanicalState {
            refighting_mask: WILY5_REFIGHTS_COMPLETE,
            ..partial
        };
        assert!(should_settle_award(final_clear, WILY5_STAGE, 0));
    }

    #[test]
    fn retention_identity_changes_emit_boundaries() {
        let first = Mm2MechanicalState::default();
        assert!(!ablation_identity_changed(first, first));
        assert!(ablation_identity_changed(
            first,
            Mm2MechanicalState {
                boobeam_targets: 1,
                ..first
            }
        ));
        assert!(ablation_identity_changed(
            first,
            Mm2MechanicalState {
                crash_shots: 1,
                ..first
            }
        ));
        assert!(ablation_identity_changed(
            first,
            Mm2MechanicalState {
                refighting_mask: 1,
                ..first
            }
        ));
        assert!(ablation_identity_changed(
            first,
            Mm2MechanicalState {
                refight_boss: 1,
                ..first
            }
        ));
    }

    #[test]
    fn enemy_damage_counts_hits_on_live_slots_only() {
        let mut prior = vec![0; WRAM_SIZE];
        prior[0x43f] = 0x87;
        prior[0x41f] = 0x4e;
        prior[0x10f] = 44;
        prior[0x6df] = 20;
        let mut current = prior.clone();
        current[0x6df] = 6;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        current[0x11f] = 1;
        assert_eq!(enemy_damage_between(&prior, &current), ENEMY_HIT_CAP);

        current[0x10f] = 45;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        current[0x10f] = 44;
        prior[0x43f] = 0;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
    }

    #[test]
    fn enemy_damage_counts_a_confirmed_kill_without_counting_a_despawn() {
        let mut prior = vec![0; WRAM_SIZE];
        prior[0x43f] = 0x87;
        prior[0x41f] = 0x4e;
        prior[0x10f] = 44;
        prior[0x6df] = 6;
        let mut current = prior.clone();
        current[0x6df] = 0;
        current[0x41f] = KILLED_OBJECT;
        current[0x10f] = 255;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        current[0x11f] = 1;
        assert_eq!(enemy_damage_between(&prior, &current), ENEMY_HIT_CAP);
        current[0x41f] = 53;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
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
        assert!(!decode_state(&[0_u8; WRAM_SIZE])
            .expect("decode")
            .boss_fight_underway());
        wram[0xb1] = 0xfe;
        wram[0x6c1] = 0;
        let dead = decode_state(&wram).expect("decode");
        assert_eq!(dead.boss_damage(), FULL_BOSS_HEALTH);
    }

    #[test]
    fn dying_run_is_equivalent_when_a_death_spans_short_actions() {
        let dying = Mm2MechanicalState {
            player_state: PLAYER_STATE_DYING,
            ..Mm2MechanicalState::default()
        };
        let mut unsplit = 0_u32;
        for _ in 0..DYING_FRAMES {
            unsplit = dying_run_for_frame(unsplit, dying);
        }

        let mut split = 0_u32;
        for _ in 0..(DYING_FRAMES / 2) {
            split = dying_run_for_frame(split, dying);
        }
        assert!(split < DYING_FRAMES);
        for _ in 0..(DYING_FRAMES - DYING_FRAMES / 2) {
            split = dying_run_for_frame(split, dying);
        }

        assert_eq!(split, unsplit);
        assert!(split >= DYING_FRAMES);
    }

    #[test]
    fn a_short_dying_flash_resets_before_the_terminal_threshold() {
        let dying = Mm2MechanicalState {
            player_state: PLAYER_STATE_DYING,
            ..Mm2MechanicalState::default()
        };
        let standing = Mm2MechanicalState {
            player_state: PLAYER_STATE_STANDING,
            ..dying
        };
        let mut run = 0_u32;
        for _ in 0..(DYING_FRAMES - 1) {
            run = dying_run_for_frame(run, dying);
        }
        assert!(run < DYING_FRAMES);
        assert_eq!(dying_run_for_frame(run, standing), 0);
    }
}
