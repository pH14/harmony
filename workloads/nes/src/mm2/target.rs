// SPDX-License-Identifier: AGPL-3.0-or-later

//! Mega Man 2 memory decoder and machine-backed target adapter.
//!
//! This module is the game-knowledge boundary. The generic search code sees
//! controller actions, opaque keys, observations, and snapshots; every Mega
//! Man 2 address and interpretation stays here.

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const CAMERA_STATE: usize = 0x1b;
/// Camera state while a scroll transition plays; the low bits flicker
/// during ordinary jumps, so only this value means the view is moving.
const CAMERA_STATE_SCROLLING: u8 = 0x80;
/// Largest one-frame downward move that play produces; a larger step is
/// the low position byte moving some other way than falling.
const FALL_STEP_LIMIT: u8 = 64;
/// Longest unbroken fall a stage can hold without the view scrolling: a
/// screen is shorter than this, so a longer fall left the stage.
const FALL_RUN_LIMIT: u16 = 256;
/// Health of the ordinary enemies on screen, one byte per object slot.
const ENEMY_HEALTH_TABLE_START: usize = 0x6d0;
const ENEMY_HEALTH_TABLE_END: usize = 0x6e0;
/// Value every empty enemy health slot holds.
const ENEMY_HEALTH_IDLE: u8 = 0x14;
/// Most damage one frame credits per enemy slot.
const ENEMY_HIT_CAP: u8 = 4;
/// Most enemy damage one screen credits; a screen whose enemies respawn
/// cannot be farmed past it.
const ENEMY_DAMAGE_CAP: u8 = 24;
const STAGE: usize = 0x2a;
const PLAYER_STATE: usize = 0x2c;
const WEAPONS_OBTAINED: usize = 0x9a;
const LIVES: usize = 0xa8;
const PLAYER_SCREEN: usize = 0x440;
const LEVEL_ROOM: usize = 0x20;
const GAME_MODE: usize = 0x04;
const SELECTED_WEAPON: usize = 0xa9;
const MENU_CURSOR: usize = 0xfd;
const MENU_PAGE: usize = 0xfe;
/// Game mode while the weapon menu is open.
const GAME_MODE_MENU: u8 = 0x03;
/// Rows per weapon menu page.
const MENU_ROWS: u8 = 8;
/// Menu value while the menu is closed.
pub const MENU_CLOSED: u8 = 0xff;
/// Frames the engine reports the dying state before it means death; a
/// weapon switch flashes the same state for a dozen frames.
const DYING_FRAMES: u32 = 30;
const PLAYER_X: usize = 0x460;
const PLAYER_Y: usize = 0x4a0;
const PLAYER_HEALTH: usize = 0x6c0;
const WEAPON_ENERGY: usize = 0x9c;
const WEAPON_ENERGY_BYTES: usize = 12;
const OBJECT_ID_TABLE: usize = 0x400;
const OBJECT_FLAG_TABLE: usize = 0x420;
const OBJECT_SLOTS: usize = 0x20;
/// Object flag bit set while the engine updates the object.
const OBJECT_ACTIVE: u8 = 0x80;
/// Object ids of the three summoned items; each is a platform the player
/// rides for a few seconds after firing it.
const ITEM_OBJECT_FIRST: u8 = 0x38;
const ITEM_OBJECT_LAST: u8 = 0x3a;
const BOSS_HEALTH: usize = 0x6c1;
const BOSS_PHASE: usize = 0xb1;

/// Lowest boss phase once the entrance and health fill have finished and
/// the boss takes damage; the fight alternates this with the next value.
const BOSS_PHASE_FIGHTING: u8 = 0x02;
/// Boss phase before the entrance; any other value means a boss is on
/// screen and the room is sealed.
const BOSS_PHASE_NONE: u8 = 0x00;
/// Lowest boss phase after the boss dies; the game holds these through the
/// explosion and the weapon award, which sets the defeated bit only some
/// 750 frames after the last hit.
const BOSS_PHASE_DEFEATED: u8 = 0xfe;
/// Health a boss holds when its fill finishes.
pub const FULL_BOSS_HEALTH: u8 = 28;

/// Player state the game holds while standing still in play.
const PLAYER_STATE_STANDING: u8 = 0x03;
/// Knocked back by a hit; airborne until the landing.
const PLAYER_STATE_HIT: u8 = 0x02;
/// Airborne from a jump or a fall.
const PLAYER_STATE_AIRBORNE: u8 = 0x06;
/// Climbing.
const PLAYER_STATE_LADDER: u8 = 0x09;
/// Stepping off the top of a ladder.
const PLAYER_STATE_LADDER_TOP: u8 = 0x0a;
/// Posture classes for the archive key.
pub const POSTURE_GROUNDED: u8 = 0;
pub const POSTURE_AIRBORNE: u8 = 1;
pub const POSTURE_LADDER: u8 = 2;
/// Player state the game holds while the player falls out of the play area.
const PLAYER_STATE_FALLEN: u8 = 0x01;
/// Player state the game holds while the player teleports in or dies to a
/// pit or spikes; in play it only follows a death, and genesis is sealed
/// after the teleport ends.
const PLAYER_STATE_DYING: u8 = 0x00;
/// Lowest on-screen Y that only occurs below the play area.
const BELOW_PLAY_AREA_Y: u8 = 0xe0;
/// Health the game refills to before handing the player control.
pub const FULL_HEALTH: u8 = 28;

const JOYPAD_START: u8 = 1 << 3;
const JOYPAD_UP: u8 = 1 << 4;
const JOYPAD_DOWN: u8 = 1 << 5;
const JOYPAD_LEFT: u8 = 1 << 6;
const JOYPAD_RIGHT: u8 = 1 << 7;

/// Number of robot master stages selectable from the stage select screen.
pub const MM2_STAGE_COUNT: u8 = 8;
/// Number of the first Wily castle stage; the six castle stages follow the
/// robot masters in the game's own numbering and chain into each other
/// without returning to stage select.
pub const MM2_FIRST_WILY_STAGE: u8 = 8;
const MM2_LAST_WILY_STAGE: u8 = 13;

/// One of the eight robot master stages or six Wily castle stages, numbered
/// as the game numbers them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2Stage(u8);

impl Mm2Stage {
    /// Validate a stage number from `0` through `13`.
    pub fn from_number(number: u8) -> Result<Self, MachineError> {
        if number <= MM2_LAST_WILY_STAGE {
            Ok(Self(number))
        } else {
            Err(MachineError::Backend(format!(
                "Mega Man 2 stage must be 0..={MM2_LAST_WILY_STAGE}, got {number}"
            )))
        }
    }

    /// Whether this is a Wily castle stage.
    #[must_use]
    pub fn is_wily(self) -> bool {
        self.0 >= MM2_FIRST_WILY_STAGE
    }

    /// Resolve a stage from its number or its robot master's name.
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

    /// The game's stage number.
    #[must_use]
    pub fn number(self) -> u8 {
        self.0
    }

    /// The robot master's name, lower case.
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

    /// Directional presses that move the stage select cursor from its
    /// power-on position, the centre of the grid, onto this stage.
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

/// A Mega Man 2 input replayed from the sealed gameplay genesis.
pub type Mm2Input = crate::search::archive::Input<ButtonChord>;

/// Mechanical state decoded from work RAM at one emulator frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2MechanicalState {
    /// Stage number.
    pub stage: u8,
    /// Index of the screen the player stands in; the stage lays its screens
    /// out in path order, so this rises along the intended route through
    /// both horizontal and vertical transitions.
    pub screen: u8,
    /// Index of the level room drawn around the player. A vertical scroll
    /// can move to another room without advancing the path index, so two
    /// rooms one above the other share a screen number.
    pub room: u8,
    /// Player X within the screen.
    pub x: u8,
    /// Player Y within the screen.
    pub y: u8,
    /// Current health.
    pub health: u8,
    /// Sum of every weapon and item energy meter plus energy tanks. Items
    /// spent early leave later obstacles that need them unpassable, so the
    /// preference keeps the lineage that still holds its energy.
    pub weapon_energy: u16,
    /// Energy meter of the equipped weapon, zero for the buster. Firing an
    /// item changes the world without moving the player, so the spent meter
    /// is the only trace of it.
    pub equipped_energy: u8,
    /// Summoned item platforms alive on screen. A platform carries the
    /// player only while it lives, so a state with one under way differs
    /// from the same position after it has faded.
    pub platforms: u8,
    /// Lives remaining.
    pub lives: u8,
    /// Player engine state.
    pub player_state: u8,
    /// Weapon or item currently equipped.
    pub weapon: u8,
    /// Weapon menu page and row while the menu is open, else
    /// [`MENU_CLOSED`]; the menu is the only way to change the weapon.
    pub menu: u8,
    /// Bitmask of robot masters defeated.
    pub weapons_obtained: u8,
    /// Health of the boss on screen, zero when none is active.
    pub boss_health: u8,
    /// Boss phase: zero without a boss, one through its entrance and health
    /// fill, two while it fights, 0xfe and 0xff once it is dead.
    pub boss_phase: u8,
    /// Camera state, nonzero while a scroll transition plays.
    pub camera_state: u8,
    /// Damage dealt to ordinary enemies since the player entered this
    /// screen, capped; the target accumulates it across frames because no
    /// work-RAM byte holds it.
    pub enemy_damage: u8,
}

impl Mm2MechanicalState {
    /// Number of robot masters defeated.
    #[must_use]
    pub fn bosses_beaten(self) -> u8 {
        self.weapons_obtained
            .count_ones()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    /// Grounded, airborne, or on a ladder.
    #[must_use]
    pub fn posture(self) -> u8 {
        match self.player_state {
            PLAYER_STATE_AIRBORNE | PLAYER_STATE_HIT => POSTURE_AIRBORNE,
            PLAYER_STATE_LADDER | PLAYER_STATE_LADDER_TOP => POSTURE_LADDER,
            _ => POSTURE_GROUNDED,
        }
    }

    /// Damage dealt to the boss being fought: zero outside a fight so the
    /// entrance health fill never reads as damage, and full once the boss
    /// is dead so the wait for the weapon award keeps its progress.
    #[must_use]
    pub fn boss_damage(self) -> u8 {
        if self.boss_phase >= BOSS_PHASE_DEFEATED {
            FULL_BOSS_HEALTH
        } else if self.boss_phase >= BOSS_PHASE_FIGHTING {
            FULL_BOSS_HEALTH.saturating_sub(self.boss_health)
        } else {
            0
        }
    }

    /// Whether a boss is on screen and still alive, which seals its room.
    #[must_use]
    pub fn boss_fight_underway(self) -> bool {
        self.boss_phase != BOSS_PHASE_NONE && self.boss_phase < BOSS_PHASE_DEFEATED
    }

    /// Whether the player is dead: out of health or fallen below the play
    /// area, neither of which the health byte alone reports. A pit or spike
    /// death shows only as a lasting dying state, which the target counts
    /// across frames.
    #[must_use]
    pub fn is_dead(self) -> bool {
        self.health == 0
            || (self.player_state == PLAYER_STATE_FALLEN && self.y >= BELOW_PLAY_AREA_Y)
    }

    /// Whether the engine reports the dying state this frame.
    #[must_use]
    pub fn is_dying(self) -> bool {
        self.player_state == PLAYER_STATE_DYING
    }
}

/// Decode the mechanical state from work RAM.
pub fn decode_state(wram: &[u8]) -> Result<Mm2MechanicalState, MachineError> {
    Ok(Mm2MechanicalState {
        stage: read_byte(wram, STAGE)?,
        screen: read_byte(wram, PLAYER_SCREEN)?,
        room: read_byte(wram, LEVEL_ROOM)?,
        x: read_byte(wram, PLAYER_X)?,
        y: read_byte(wram, PLAYER_Y)?,
        health: read_byte(wram, PLAYER_HEALTH)?,
        weapon_energy: (WEAPON_ENERGY..WEAPON_ENERGY + WEAPON_ENERGY_BYTES)
            .map(|index| read_byte(wram, index).map(u16::from))
            .sum::<Result<u16, MachineError>>()?,
        equipped_energy: match read_byte(wram, SELECTED_WEAPON)? {
            0 => 0,
            weapon => read_byte(wram, WEAPON_ENERGY + usize::from(weapon) - 1)?,
        },
        platforms: live_platforms(wram)?,
        lives: read_byte(wram, LIVES)?,
        player_state: read_byte(wram, PLAYER_STATE)?,
        weapon: read_byte(wram, SELECTED_WEAPON)?,
        menu: if read_byte(wram, GAME_MODE)? == GAME_MODE_MENU {
            read_byte(wram, MENU_PAGE)?
                .wrapping_mul(MENU_ROWS)
                .wrapping_add(read_byte(wram, MENU_CURSOR)?)
        } else {
            MENU_CLOSED
        },
        weapons_obtained: read_byte(wram, WEAPONS_OBTAINED)?,
        boss_health: read_byte(wram, BOSS_HEALTH)?,
        boss_phase: read_byte(wram, BOSS_PHASE)?,
        camera_state: read_byte(wram, CAMERA_STATE)?,
        enemy_damage: 0,
    })
}

/// Damage to ordinary enemies between two consecutive frames: the enemy
/// health table holds the idle value in every empty slot, so only a slot
/// already below it counts, and one hit is capped so a kill that clears a
/// slot cannot dwarf the shots that led to it.
fn enemy_damage_between(prior: &[u8], current: &[u8]) -> u8 {
    (ENEMY_HEALTH_TABLE_START..ENEMY_HEALTH_TABLE_END)
        .filter_map(|slot| {
            let before = *prior.get(slot)?;
            let after = *current.get(slot)?;
            (before != ENEMY_HEALTH_IDLE && after < before)
                .then(|| before.saturating_sub(after).min(ENEMY_HIT_CAP))
        })
        .fold(0_u8, u8::saturating_add)
}

/// Count of active objects that are summoned item platforms.
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

/// Coarse location used by observation emission.
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

/// Enemy damage per key bucket; one buster hit on a sturdy enemy, so every
/// hit that lands on a blocking enemy opens a new slot.
pub const ENEMY_DAMAGE_BUCKET: u8 = 2;

/// Boss damage per key bucket; one buster hit deals two.
pub const BOSS_DAMAGE_BUCKET: u8 = 2;

/// Adapter-owned lexicographic preference between states at one location.
/// Lives stay out: extra lives farm without bound from respawning enemies,
/// and a preference on them lets one farming lineage dominate every band.
#[must_use]
pub fn preference_tuple(state: Mm2MechanicalState) -> (u8, u8, u16) {
    (state.bosses_beaten(), state.health, state.weapon_energy)
}

/// Mechanical evidence emitted at a changed spatial or resource boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Observations {
    /// Frames emulated since the sealed gameplay genesis.
    pub frame_count: u64,
    /// Decoded mechanical state.
    pub decoded: Mm2MechanicalState,
    /// Sorted work-RAM indices changed since the prior emitted event.
    pub changed_indices: Vec<u16>,
    /// Whether the player is dead at this event.
    pub dead: bool,
    /// Pixels fallen without a break or a scroll up to this event.
    #[serde(default)]
    pub fall_run: u16,
    /// Compact game-neutral mechanical log line.
    pub log_line: String,
}

/// Geometry and frame count of one rendered replay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2VideoMetadata {
    /// Tightly packed frame width.
    pub width: u32,
    /// Tightly packed frame height.
    pub height: u32,
    /// Frames written.
    pub frames: u64,
    /// Native signed 16-bit PCM sample rate.
    pub audio_sample_rate: u32,
    /// Interleaved PCM channel count.
    pub audio_channels: u8,
    /// Stereo PCM frames written.
    pub audio_frames: u64,
    /// Decoded game state after the searched input and before the film tail.
    pub input_endpoint: Mm2MechanicalState,
}

/// Complete state needed to resume a Mega Man 2 prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Snapshot {
    emulator_state: Vec<u8>,
    observation: Mm2Observations,
    failed: bool,
}

impl Mm2Snapshot {
    /// Decoded endpoint state carried by this snapshot.
    #[must_use]
    pub fn state(&self) -> Mm2MechanicalState {
        self.observation.decoded
    }

    /// Number of persisted emulator-state bytes held by this snapshot.
    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.len()
    }
}

/// Start presses from the settled title screen to the stage select screen:
/// the title, then the start/password menu, then the stage select itself.
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
/// Frames each menu needs after a Start press before it accepts the next.
const MENU_SETTLE_FRAMES: u32 = 300;
/// Frames the title screen needs before it accepts Start.
const TITLE_SETTLE_FRAMES: u32 = 600;
/// Frames from the weapon award until the password/stage-select menu
/// accepts input.
const AWARD_SETTLE_FRAMES: u32 = 900;
/// Longest wait, in frames, from the stage select confirmation to the first
/// frame of play.
const STAGE_START_WAIT_FRAMES: u32 = 1_200;
/// Frames allowed for the fortress scene before the first castle stage.
const WILY_STAGE_START_WAIT_FRAMES: u32 = 8_000;

/// Machine-backed target used by Mega Man 2 campaigns.
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

/// Inputs from power-on to the stage select screen with the cursor at its
/// centre.
#[must_use]
pub fn power_on_walk() -> Vec<ButtonChord> {
    let mut chords = idle_chords(TITLE_SETTLE_FRAMES);
    for press in BOOT_TO_STAGE_SELECT {
        chords.push(press);
        chords.extend(idle_chords(MENU_SETTLE_FRAMES));
    }
    chords
}

/// Work-RAM byte that marks the stage select screen and the stage intro
/// that follows it; the award, message, item and password screens all hold
/// the next value.
const MENU_MODE: usize = 0xf7;
const MENU_MODE_STAGE_SELECT: u8 = 0x90;
const STAGE_SELECT_WALK_ROUNDS: usize = 16;

/// Whether the game rests on the stage select screen, checked before any
/// Start press so the stage intro that shares the mode byte cannot begin.
fn at_stage_select(wram: &[u8]) -> bool {
    wram.get(MENU_MODE).copied() == Some(MENU_MODE_STAGE_SELECT)
}

/// Run `chords` from power-on, then walk from the weapon award back to the
/// stage select screen and return the chords that walk took. The award is
/// followed by a varying run of screens (the weapon, a message and an item
/// for some bosses) and a password/stage-select menu whose cursor rests on
/// the password entry, so each round moves the cursor down and presses
/// Start until the stage select screen appears.
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
    /// Load the ROM, walk the menus to the requested stage, and seal genesis
    /// at the first frame the player stands in play with full health.
    pub fn from_rom_bytes_headless_at_stage(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        stage: Mm2Stage,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_after(rom, core_path, core_sha256, &power_on_walk(), stage)
    }

    /// Load the ROM, run `prefix` from power-on so the game rests on the
    /// stage select screen with the cursor at its centre, pick the stage,
    /// and seal genesis at the first frame the player stands in play with
    /// full health.
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
        )
    }

    fn from_machine(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
        stage: Mm2Stage,
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
        let state = decode_state(&wram)?;
        let genesis = machine.snapshot()?;
        let observation = Mm2Observations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            dead: false,
            fall_run: 0,
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
        })
    }

    /// Every input from power-on to sealed genesis; the same chords replay
    /// the genesis frame exactly.
    #[must_use]
    pub fn genesis_prefix(&self) -> &[ButtonChord] {
        &self.genesis_prefix
    }

    /// Current decoded state.
    #[must_use]
    pub fn mechanical_state(&self) -> Mm2MechanicalState {
        self.observation.decoded
    }

    /// Whether the current state is a death.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.dead
    }

    /// Whether this input defeated a robot master beyond sealed genesis.
    #[must_use]
    pub fn defeated_a_boss(&self) -> bool {
        let state = self.observation.decoded;
        state.weapons_obtained & !self.genesis_weapons != 0
            || state.stage > self.genesis_observation.decoded.stage
    }

    /// Robot masters defeated at sealed genesis.
    #[must_use]
    pub fn genesis_weapons(&self) -> u8 {
        self.genesis_weapons
    }

    /// Total deterministic frames this instance has emulated.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    /// Observer events emitted by the most recent action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[Mm2Observations] {
        &self.action_observations
    }

    /// Test one fixed continuation and restore the caller's state afterward.
    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_dead() || self.defeated_a_boss() || frames == 0 {
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

    /// Replay an input from gameplay genesis and write RGB24 video and S16LE
    /// stereo audio.
    ///
    /// Video is a replay-only observer. Search workers never enable it, and
    /// the recorded headless campaign identity is unchanged.
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
            log_line: format!("frame={frame_count} changed={changed_indices:?}"),
        }
    }

    /// Run one chord and return every frame's work RAM, or `None` when the
    /// emulator failed.
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
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_dead() || self.defeated_a_boss() {
            return;
        }
        let Some(mut frames) = self.run_action(action) else {
            self.failed = true;
            return;
        };
        let mut waited = 0;
        while waited < AWARD_SETTLE_FRAMES
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
        let mut dying_frames = 0_u32;
        let mut fall_run = self.observation.fall_run;
        let mut enemy_damage = self.observation.decoded.enemy_damage;
        let mut prior_frame = self.current_wram;
        let Ok(mut previous) = decode_state(&prior_frame) else {
            self.failed = true;
            return;
        };
        for (offset, wram) in frames.iter().enumerate() {
            let Ok(mut state) = decode_state(wram) else {
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
            dying_frames = if state.is_dying() {
                dying_frames.saturating_add(1)
            } else {
                0
            };
            let dead = died
                || state.is_dead()
                || dying_frames >= DYING_FRAMES
                || state.lives < genesis.lives
                || state.stage < genesis.stage
                || escaped
                || fell_out;
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
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
            let Ok(endpoint_state) = decode_state(&endpoint_wram) else {
                self.failed = true;
                return;
            };
            let mut observation =
                self.make_observation(endpoint_frame, endpoint_state, &endpoint_wram, &prior_wram);
            observation.dead = died;
            observation.fall_run = fall_run;
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
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
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
    fn enemy_damage_counts_hits_on_live_slots_only() {
        let mut prior = vec![ENEMY_HEALTH_IDLE; WRAM_SIZE];
        let mut current = prior.clone();
        current[0x6dc] = 0x10;
        assert_eq!(enemy_damage_between(&prior, &current), 0);
        prior[0x6dc] = 0x12;
        assert_eq!(enemy_damage_between(&prior, &current), 2);
        current[0x6dc] = 0;
        assert_eq!(enemy_damage_between(&prior, &current), ENEMY_HIT_CAP);
        prior[0x6d1] = 0x10;
        current[0x6d1] = ENEMY_HEALTH_IDLE;
        assert_eq!(enemy_damage_between(&prior, &current), ENEMY_HIT_CAP);
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
