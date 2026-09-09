// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metroid memory decoder and machine-backed target adapter.
//!
//! This module is the game-knowledge boundary. The generic search code sees
//! controller actions, opaque keys, observations, and snapshots; every
//! Metroid address and interpretation stays here.

use std::{error::Error, path::Path};

use machine::{Machine, MachineError, SnapId, StopConditions, nes, quicknes::QuickNesMachine};
use serde::{Deserialize, Serialize};

use super::progress::{BossDefeats, TourianEvents};
use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

/// Engine mode; the game plays only at [`GAME_MODE_PLAYING`].
const GAME_MODE: usize = 0x1e;
/// Engine mode while the player has control of Samus.
const GAME_MODE_PLAYING: u8 = 0x03;
/// Scrolling direction of the current room: [`SCROLL_UP`], [`SCROLL_DOWN`],
/// [`SCROLL_LEFT`], or [`SCROLL_RIGHT`]. Rooms scroll along one axis only.
const SCROLL_DIRECTION: usize = 0x49;
const SCROLL_UP: u8 = 0;
const SCROLL_DOWN: u8 = 1;
const SCROLL_LEFT: u8 = 2;
/// Map row and column the engine keeps for scrolling. Along the scroll
/// axis they name the screen the scroll is heading into, so Samus's own
/// screen is this or one back, decided by which name table she occupies
/// (see [`SAMUS_NAME_TABLE`]).
const MAP_Y: usize = 0x4f;
const MAP_X: usize = 0x50;
/// Scroll offsets loaded into the PPU. Along the scroll axis the picture
/// starts this far into the name table selected by [`PPU_CONTROL`] and
/// continues into the other name table.
const SCROLL_Y: usize = 0xfc;
const SCROLL_X: usize = 0xfd;
/// Data byte for PPU control register 0; its low two bits select the name
/// table at the top-left of the picture, either 0 or 3.
const PPU_CONTROL: usize = 0xff;
const PPU_NAME_TABLE_MASK: u8 = 0x03;
/// Name table Samus is drawn from: 0 for name table 0, 1 for name table 3.
const SAMUS_NAME_TABLE: usize = 0x30c;
/// Samus's position within her screen, in engine coordinates that do not
/// move with the scroll.
const SAMUS_Y: usize = 0x30d;
const SAMUS_X: usize = 0x30e;
/// Door transition: zero outside a door, otherwise the side being entered.
const DOOR_STATE: usize = 0x56;
/// Area of the world Samus is in; the five areas are not ordered by
/// progress, so this identifies a place rather than ranking one.
const AREA: usize = 0x74;
/// Area byte for Brinstar once an elevator has been ridden; the engine
/// leaves it zero from power-on until then, so both values are one place.
const AREA_BRINSTAR: u8 = 0x10;
/// Samus's pose. The engine leaves this at [`POSE_UNMOVED`] until Samus
/// first moves after a genesis.
const POSE: usize = 0x300;
/// Pose before Samus has moved.
const POSE_UNMOVED: u8 = 0xff;
/// Pose while standing still.
const POSE_STANDING: u8 = 0x00;
/// Pose while running along the ground.
const POSE_RUNNING: u8 = 0x01;
/// Pose while airborne.
const POSE_AIRBORNE: u8 = 0x02;
/// Posture classes for the archive key.
pub const POSTURE_GROUNDED: u8 = 0;
pub const POSTURE_AIRBORNE: u8 = 1;
/// Posture for any pose the decoder does not name, such as rolling as a
/// morph ball or climbing into a door. Each is a distinct way to occupy a
/// position, so the pose byte itself separates them.
pub const POSTURE_OTHER: u8 = 2;

/// Health, as binary-coded decimal digits of a fixed-point `###.#` value.
/// The high byte carries the hundreds and tens digits.
const HEALTH_HIGH: usize = 0x107;
const HEALTH_LOW: usize = 0x106;
/// Health the game grants at the start of a new game, in tenths.
pub const STARTING_HEALTH_TENTHS: u16 = 300;

/// Versioned terminal interpretation; legacy replay keeps its original predicate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MetroidTerminalPolicy {
    /// The historical predicate considers only zero health.
    #[default]
    Legacy,
    /// Reject the intermediate BCD borrow value exposed during lethal damage.
    BcdUnderflow,
}

impl MetroidTerminalPolicy {
    /// Stable game-policy identity carried in campaign streams.
    #[must_use]
    pub fn identifier(self) -> &'static str {
        match self {
            Self::Legacy => "death_or_ending_v2",
            Self::BcdUnderflow => "death_or_bcd_underflow_or_ending_v3",
        }
    }

    /// Resolve only explicitly supported semantics.
    pub fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "death_or_ending_v2" => Ok(Self::Legacy),
            "death_or_bcd_underflow_or_ending_v3" => Ok(Self::BcdUnderflow),
            _ => Err("unknown Metroid terminal policy".into()),
        }
    }

    /// Classify a decoded state under these versioned terminal semantics.
    #[must_use]
    pub fn is_dead(self, state: MetroidMechanicalState) -> bool {
        // Bank07 $CED7 stores the BCD subtraction before $CEE4 checks borrow
        // and $CEEB clears health. A frame boundary can expose this intermediate
        // negative value. Six tanks cap normal health at 6999; the >=8000 sign
        // range is not a resource advantage. Keep raw health for forensic replay.
        state.is_dead() || (self == Self::BcdUnderflow && state.health >= 8000)
    }
}

/// First address of the cartridge work RAM window, where the game keeps the
/// progress that survives leaving a room.
pub const CARTRIDGE_RAM_BASE: u64 = 0x6000;
/// Equipment obtained, one bit per item.
const EQUIPMENT: usize = 0x878;
/// Missiles carried.
const MISSILES: usize = 0x879;
/// Missile capacity, which rises with each missile tank collected.
const MISSILE_CAPACITY: usize = 0x87a;
/// Kraid's status; bit 0 is set once Kraid is defeated.
const KRAID_STATUS: usize = 0x87b;
/// Ridley's status; bit 1 is set once Ridley is defeated. Bank07.asm LDD75
/// stores (InArea & 0x0f) >> 1: Kraid = 1, Ridley = 2.
const RIDLEY_STATUS: usize = 0x87c;
const KRAID_DEFEATED_BIT: u8 = 1;
const RIDLEY_DEFEATED_BIT: u8 = 2;
/// Non-zero while the ending plays.
const ENDING: usize = 0x883;
/// Energy tanks collected.
const ENERGY_TANKS: usize = 0x877;

const JOYPAD_START: u8 = 1 << 3;

/// A Metroid input replayed from the sealed gameplay genesis.
pub type MetroidInput = crate::search::archive::Input<ButtonChord>;

/// Mechanical state decoded from memory at one emulator frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidMechanicalState {
    /// Area of the world.
    pub area: u8,
    /// Column of the world map.
    pub map_x: u8,
    /// Row of the world map.
    pub map_y: u8,
    /// Samus's horizontal position within her screen.
    pub x: u8,
    /// Samus's vertical position within her screen.
    pub y: u8,
    /// Samus's pose.
    pub pose: u8,
    /// Door transition state; a door is the only route between many rooms,
    /// and passing through one takes several frames.
    pub door: u8,
    /// Engine mode.
    pub mode: u8,
    /// Health in tenths.
    pub health: u16,
    /// Equipment obtained, one bit per item.
    pub equipment: u8,
    /// Missiles carried.
    pub missiles: u8,
    /// Missile capacity, which rises with each missile tank collected.
    pub missile_capacity: u8,
    /// Energy tanks collected.
    pub energy_tanks: u8,
    /// Bosses defeated.
    pub bosses: u8,
    /// Whether the ending is playing.
    pub ending: bool,
}

impl MetroidMechanicalState {
    /// Legacy progress count: equipment bits plus defeated bosses. Item identity does
    /// not matter to the search; the count orders progress.
    #[must_use]
    pub fn items(self) -> u8 {
        u8::try_from(self.equipment.count_ones())
            .unwrap_or(u8::MAX)
            .saturating_add(self.bosses)
    }

    /// Missile and energy tanks collected. Missile capacity rises by a
    /// fixed step per tank, so the capacity counts them without naming any
    /// one tank.
    #[must_use]
    pub fn collectibles(self) -> u8 {
        (self.missile_capacity / MISSILE_TANK_STEP).saturating_add(self.energy_tanks)
    }

    /// Whether the game has reached its ending.
    #[must_use]
    pub fn is_victory(self) -> bool {
        self.ending
    }

    /// Grounded, airborne, or another pose the decoder does not name.
    #[must_use]
    pub fn posture(self) -> u8 {
        match self.pose {
            POSE_UNMOVED | POSE_STANDING | POSE_RUNNING => POSTURE_GROUNDED,
            POSE_AIRBORNE => POSTURE_AIRBORNE,
            _ => POSTURE_OTHER,
        }
    }

    /// Whether Samus is out of health.
    #[must_use]
    pub fn is_dead(self) -> bool {
        self.health == 0
    }

    /// Whether the engine has handed the player control.
    #[must_use]
    pub fn in_play(self) -> bool {
        self.mode == GAME_MODE_PLAYING
    }
}

/// Missile capacity granted per missile tank.
const MISSILE_TANK_STEP: u8 = 5;

/// Decode two binary-coded decimal digits.
fn bcd(byte: u8) -> u16 {
    u16::from(byte >> 4) * 10 + u16::from(byte & 0x0f)
}

/// Decode the mechanical state from work RAM and cartridge work RAM.
pub fn decode_state(wram: &[u8], cartridge: &[u8]) -> Result<MetroidMechanicalState, MachineError> {
    let (map_x, map_y) = samus_screen(wram)?;
    Ok(MetroidMechanicalState {
        area: match read_byte(wram, AREA)? {
            0 => AREA_BRINSTAR,
            area => area,
        },
        map_x,
        map_y,
        x: read_byte(wram, SAMUS_X)?,
        y: read_byte(wram, SAMUS_Y)?,
        pose: read_byte(wram, POSE)?,
        door: read_byte(wram, DOOR_STATE)?,
        mode: read_byte(wram, GAME_MODE)?,
        health: bcd(read_byte(wram, HEALTH_HIGH)?) * 100 + bcd(read_byte(wram, HEALTH_LOW)?),
        equipment: read_byte(cartridge, EQUIPMENT)?,
        missiles: read_byte(cartridge, MISSILES)?,
        missile_capacity: read_byte(cartridge, MISSILE_CAPACITY)?,
        energy_tanks: read_byte(cartridge, ENERGY_TANKS)?,
        bosses: u8::from(read_byte(cartridge, KRAID_STATUS)? & KRAID_DEFEATED_BIT != 0)
            + u8::from(read_byte(cartridge, RIDLEY_STATUS)? & RIDLEY_DEFEATED_BIT != 0),
        ending: read_byte(cartridge, ENDING)? != 0,
    })
}

fn decode_boss_defeats(cartridge: &[u8]) -> Result<BossDefeats, MachineError> {
    Ok(BossDefeats {
        kraid: read_byte(cartridge, KRAID_STATUS)? & KRAID_DEFEATED_BIT != 0,
        ridley: read_byte(cartridge, RIDLEY_STATUS)? & RIDLEY_DEFEATED_BIT != 0,
    })
}

fn read_byte(bytes: &[u8], address: usize) -> Result<u8, MachineError> {
    bytes
        .get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Metroid RAM address {address:#x} is absent")))
}

/// Adapter-owned lexicographic preference between states at one location.
/// Items and tanks outrank the consumables, since an item opens routes that
/// no amount of health or ammunition reaches.
#[must_use]
pub fn preference_tuple(state: MetroidMechanicalState) -> (u8, u8, u16, u8) {
    (
        state.items(),
        state.collectibles(),
        state.health,
        state.missiles,
    )
}

/// Mechanical evidence emitted at a changed spatial or resource boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidObservations {
    /// Frames emulated since the sealed gameplay genesis.
    pub frame_count: u64,
    /// Decoded mechanical state.
    pub decoded: MetroidMechanicalState,
    /// Named boss flags for reporting. Never consulted by archive policy.
    pub boss_defeats: BossDefeats,
    /// Tourian's Mother Brain state machine ($98), reporting-only. Other banks
    /// may reuse this byte; interpretation must require Tourian gameplay.
    pub mother_brain_status: u8,
    /// Tourian transitions latched across frames of this action, reporting-only.
    pub tourian_events: TourianEvents,
    /// Sorted work-RAM indices changed since the prior emitted event.
    pub changed_indices: Vec<u16>,
    /// Whether Samus is dead at this event.
    pub dead: bool,
    /// Compact game-neutral mechanical log line.
    pub log_line: String,
}

/// Complete state needed to resume a Metroid prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidSnapshot {
    emulator_state: Vec<u8>,
    observation: MetroidObservations,
    failed: bool,
}

impl MetroidSnapshot {
    /// Decoded endpoint state carried by this snapshot.
    #[must_use]
    pub fn state(&self) -> MetroidMechanicalState {
        self.observation.decoded
    }

    /// Number of persisted emulator-state bytes held by this snapshot.
    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.len()
    }
}

/// Frames the title screen needs before it accepts Start.
const TITLE_SETTLE_FRAMES: u32 = 300;
/// Frames each menu needs after a Start press before it accepts the next.
const MENU_SETTLE_FRAMES: u32 = 180;
/// Start presses from power-on: the title screen, then the start/continue
/// menu resting on a new game.
const BOOT_PRESSES: usize = 2;
/// Longest wait, in frames, from the last menu press to the first frame of
/// play.
const PLAY_WAIT_FRAMES: u32 = 1_200;

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

/// Inputs from power-on to the first frame the game accepts play input.
#[must_use]
pub fn power_on_walk() -> Vec<ButtonChord> {
    let mut chords = idle_chords(TITLE_SETTLE_FRAMES);
    for _ in 0..BOOT_PRESSES {
        chords.push(ButtonChord::new(JOYPAD_START, 4));
        chords.extend(idle_chords(MENU_SETTLE_FRAMES));
    }
    chords
}

fn run_chords(machine: &mut QuickNesMachine, chords: &[ButtonChord]) -> Result<(), MachineError> {
    let here = machine.snapshot()?;
    machine.branch(here, &nes::reproducer(chords))?;
    machine.run(StopConditions::default(), None)?;
    machine.drop_snapshot(here)
}

/// Machine-backed target used by Metroid campaigns.
#[derive(Debug)]
pub struct MetroidTarget {
    machine: QuickNesMachine,
    genesis: SnapId,
    genesis_observation: MetroidObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: MetroidObservations,
    action_observations: Vec<MetroidObservations>,
    failed: bool,
    genesis_prefix: Vec<ButtonChord>,
    terminal_policy: MetroidTerminalPolicy,
}

impl MetroidTarget {
    /// Load the ROM, walk the menus into a new game, and seal genesis at the
    /// first frame the engine hands the player control.
    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_after(rom, core_path, core_sha256, &power_on_walk())
    }

    /// Load the ROM, run `prefix` from power-on, and seal genesis at the
    /// first frame the engine hands the player control.
    pub fn from_rom_bytes_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        // The game keeps equipment, missiles, and collected tanks in
        // cartridge work RAM, which a backend publishes only for an image
        // that declares the region.
        let image = nes::with_cartridge_ram(rom)?;
        Self::from_machine(
            QuickNesMachine::from_rom_bytes(&image, core_path, core_sha256)?,
            prefix,
        )
    }

    fn from_machine(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        let mut genesis_prefix = prefix.to_vec();
        for chunk in prefix.chunks(64) {
            run_chords(&mut machine, chunk)?;
        }
        let mut waited = 0;
        loop {
            let wram = machine.read_wram()?;
            let cartridge = machine.read_save_ram()?;
            let state = decode_state(&wram, &cartridge)?;
            if state.in_play() && state.health == STARTING_HEALTH_TENTHS {
                break;
            }
            if waited >= PLAY_WAIT_FRAMES {
                return Err(MachineError::Backend(format!(
                    "Metroid did not reach play within {PLAY_WAIT_FRAMES} frames: \
                     mode={} health={}",
                    state.mode, state.health
                )));
            }
            run_chords(&mut machine, &[ButtonChord::new(0, 1)])?;
            waited += 1;
        }
        genesis_prefix.extend(idle_chords(waited));
        let wram = machine.read_wram()?;
        let cartridge = machine.read_save_ram()?;
        let state = decode_state(&wram, &cartridge)?;
        let genesis = machine.snapshot()?;
        let observation = MetroidObservations {
            frame_count: 0,
            decoded: state,
            boss_defeats: decode_boss_defeats(&cartridge)?,
            mother_brain_status: read_byte(&wram, 0x98)?,
            tourian_events: TourianEvents::default(),
            changed_indices: Vec::new(),
            dead: false,
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
            genesis_prefix,
            terminal_policy: MetroidTerminalPolicy::Legacy,
        })
    }

    /// Select terminal semantics without changing execution or raw observations.
    #[must_use]
    pub fn with_terminal_policy(mut self, policy: MetroidTerminalPolicy) -> Self {
        self.terminal_policy = policy;
        self.observation.dead = policy.is_dead(self.observation.decoded);
        self.genesis_observation.dead = policy.is_dead(self.genesis_observation.decoded);
        self.action_observations = vec![self.observation.clone()];
        self
    }

    /// Every input from power-on to sealed genesis; the same chords replay
    /// the genesis frame exactly.
    #[must_use]
    pub fn genesis_prefix(&self) -> &[ButtonChord] {
        &self.genesis_prefix
    }

    /// Current decoded state.
    #[must_use]
    pub fn mechanical_state(&self) -> MetroidMechanicalState {
        self.observation.decoded
    }

    /// Whether the current state is a death.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.dead
    }

    /// Whether the last action reached the ending.
    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.observation.decoded.is_victory()
    }

    /// Equipment items and collectibles held at sealed genesis.
    #[must_use]
    pub fn genesis_holdings(&self) -> (u8, u8) {
        let state = self.genesis_observation.decoded;
        (state.items(), state.collectibles())
    }

    /// Whether this input gained an item or a collectible beyond sealed
    /// genesis. Neither is ever lost, so a gain is permanent progress.
    #[must_use]
    pub fn gained_an_item(&self) -> bool {
        let (items, tanks) = self.genesis_holdings();
        let state = self.observation.decoded;
        state.items() > items || state.collectibles() > tanks
    }

    /// Total deterministic frames this instance has emulated.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    /// Observer events emitted by the most recent action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[MetroidObservations] {
        &self.action_observations
    }

    /// Read the cartridge work RAM window the decoder needs.
    fn cartridge(&self) -> Result<Vec<u8>, MachineError> {
        self.machine.read_save_ram()
    }

    fn make_observation(
        frame_count: u64,
        state: MetroidMechanicalState,
        wram: &[u8; WRAM_SIZE],
        prior_wram: &[u8; WRAM_SIZE],
        boss_defeats: BossDefeats,
        tourian_events: TourianEvents,
    ) -> MetroidObservations {
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
        MetroidObservations {
            frame_count,
            decoded: state,
            boss_defeats,
            mother_brain_status: wram[0x98],
            tourian_events,
            changed_indices: changed_indices.clone(),
            dead: state.is_dead(),
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

impl Target for MetroidTarget {
    type Action = ButtonChord;
    type Observations = MetroidObservations;
    type Snapshot = MetroidSnapshot;

    fn reset(&mut self) {
        self.failed = self.machine.replay(self.genesis).is_err();
        self.current_wram = self.genesis_wram;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_dead() {
            return;
        }
        let Some(frames) = self.run_action(action) else {
            self.failed = true;
            return;
        };
        // Cartridge RAM is sampled at the action endpoint. Its values describe
        // that endpoint, not the exact pickup frame within a held chord.
        let Ok(cartridge) = self.cartridge() else {
            self.failed = true;
            return;
        };
        match decode_action_observations_with_policy(
            &frames,
            &cartridge,
            &self.observation,
            self.current_wram,
            self.terminal_policy,
        ) {
            Ok((observations, wram)) => {
                if let Some(last) = observations.last() {
                    self.observation = last.clone();
                }
                self.current_wram = wram;
                self.action_observations = observations;
            }
            Err(_) => self.failed = true,
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        (u64::from(state.area) << 40)
            | (u64::from(state.map_x) << 32)
            | (u64::from(state.map_y) << 24)
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
        Some(MetroidSnapshot {
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
        self.observation.dead = self.terminal_policy.is_dead(self.observation.decoded);
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

/// Decode the held action without adding reporting transitions to the search
/// event stream. The endpoint carries every live Tourian event in the action.
#[cfg(test)]
fn decode_action_observations(
    frames: &[[u8; WRAM_SIZE]],
    cartridge: &[u8],
    initial: &MetroidObservations,
    prior_wram: [u8; WRAM_SIZE],
) -> Result<(Vec<MetroidObservations>, [u8; WRAM_SIZE]), MachineError> {
    decode_action_observations_with_policy(
        frames,
        cartridge,
        initial,
        prior_wram,
        MetroidTerminalPolicy::Legacy,
    )
}

fn decode_action_observations_with_policy(
    frames: &[[u8; WRAM_SIZE]],
    cartridge: &[u8],
    initial: &MetroidObservations,
    mut prior_wram: [u8; WRAM_SIZE],
    policy: MetroidTerminalPolicy,
) -> Result<(Vec<MetroidObservations>, [u8; WRAM_SIZE]), MachineError> {
    let boss_defeats = decode_boss_defeats(cartridge)?;
    let mut prior_state = initial.decoded;
    let mut observations = Vec::new();
    let mut tourian_events = TourianEvents::default();
    for (offset, wram) in frames.iter().enumerate() {
        let state = decode_state(wram, cartridge)?;
        let frame_count = initial.frame_count + u64::try_from(offset).unwrap_or(u64::MAX) + 1;
        tourian_events.observe(state, wram[0x98]);
        let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
            || policy.is_dead(state) != policy.is_dead(prior_state);
        if boundary || offset + 1 == frames.len() {
            let mut observation = MetroidTarget::make_observation(
                frame_count,
                state,
                wram,
                &prior_wram,
                boss_defeats,
                tourian_events,
            );
            observation.dead = policy.is_dead(state);
            observations.push(observation);
            prior_wram = *wram;
        }
        prior_state = state;
        if policy.is_dead(state) {
            break;
        }
    }
    Ok((observations, prior_wram))
}

/// Map column and row of the screen Samus occupies.
///
/// The engine's map position tracks the scroll rather than Samus: along
/// the scroll axis it names the screen ahead in the scroll direction, and
/// it changes the moment the direction flips even while Samus stands
/// still. The picture shows the name table selected by [`PPU_CONTROL`]
/// from the scroll offset onward and then the other name table, so when
/// the offset is nonzero the selected table is the one behind (above or
/// left of) the other. Samus is on the screen ahead when the offset is
/// zero, or when she occupies the ahead table; otherwise her screen is
/// one back toward the scroll origin.
fn samus_screen(wram: &[u8]) -> Result<(u8, u8), MachineError> {
    let direction = read_byte(wram, SCROLL_DIRECTION)?;
    let map_x = read_byte(wram, MAP_X)?;
    let map_y = read_byte(wram, MAP_Y)?;
    let vertical = direction == SCROLL_UP || direction == SCROLL_DOWN;
    let backward = direction == SCROLL_UP || direction == SCROLL_LEFT;
    let scroll = read_byte(wram, if vertical { SCROLL_Y } else { SCROLL_X })?;
    let selected_table = u8::from(read_byte(wram, PPU_CONTROL)? & PPU_NAME_TABLE_MASK != 0);
    let samus_in_selected = read_byte(wram, SAMUS_NAME_TABLE)? == selected_table;
    let ahead = scroll == 0 || samus_in_selected == backward;
    let step = |map: u8| {
        if ahead {
            map
        } else if backward {
            map.wrapping_add(1)
        } else {
            map.wrapping_sub(1)
        }
    };
    Ok(if vertical {
        (map_x, step(map_y))
    } else {
        (step(map_x), map_y)
    })
}

/// Coarse location used by observation emission.
#[must_use]
pub fn spatial_bucket(state: MetroidMechanicalState) -> (u8, u8, u8, u8, u8, u8, u8) {
    (
        state.area,
        state.map_x,
        state.map_y,
        state.x / 32,
        state.y / 32,
        state.items(),
        state.collectibles(),
    )
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use crate::metroid::progress::NamedProgress;

    #[test]
    fn bcd_underflow_is_terminal_without_rewriting_raw_health() {
        let cartridge = [0; 8192];
        let mut start = [0; WRAM_SIZE];
        start[GAME_MODE] = GAME_MODE_PLAYING;
        start[HEALTH_LOW] = 0x37;
        let initial = MetroidTarget::make_observation(
            0,
            decode_state(&start, &cartridge).unwrap(),
            &start,
            &start,
            BossDefeats::default(),
            TourianEvents::default(),
        );
        let mut underflow = start;
        underflow[HEALTH_LOW] = 0;
        underflow[HEALTH_HIGH] = 0x98;
        let mut cleared = underflow;
        cleared[HEALTH_HIGH] = 0;
        let (legacy, _) = decode_action_observations_with_policy(
            &[underflow],
            &cartridge,
            &initial,
            start,
            MetroidTerminalPolicy::Legacy,
        )
        .unwrap();
        assert!(!legacy[0].dead);
        let (corrected, _) = decode_action_observations_with_policy(
            &[underflow, cleared],
            &cartridge,
            &initial,
            start,
            MetroidTerminalPolicy::BcdUnderflow,
        )
        .unwrap();
        assert_eq!(corrected.len(), 1);
        assert!(corrected[0].dead);
        assert_eq!(corrected[0].frame_count, 1);
        assert_eq!(corrected[0].decoded.health, 9800);
        for (high, low) in [(0x69, 0x99), (0x19, 0x99), (0, 0x12)] {
            let mut live = start;
            live[HEALTH_HIGH] = high;
            live[HEALTH_LOW] = low;
            let (observations, _) = decode_action_observations_with_policy(
                &[live],
                &cartridge,
                &initial,
                start,
                MetroidTerminalPolicy::BcdUnderflow,
            )
            .unwrap();
            assert!(!observations[0].dead, "valid health was rejected");
        }
    }

    #[test]
    fn terminal_policy_identifiers_are_strict() {
        for policy in [
            MetroidTerminalPolicy::Legacy,
            MetroidTerminalPolicy::BcdUnderflow,
        ] {
            assert_eq!(
                MetroidTerminalPolicy::parse(policy.identifier()).unwrap(),
                policy
            );
        }
        assert!(MetroidTerminalPolicy::parse("death_or_ending_v999").is_err());
    }

    #[test]
    fn transient_escape_survives_stationary_held_action_without_extra_events() {
        let cartridge = [0; 8192];
        for (area, health, expected) in [(0x13, 3, true), (0x11, 3, false), (0x13, 0, false)] {
            let mut wram = [0; WRAM_SIZE];
            wram[GAME_MODE] = GAME_MODE_PLAYING;
            wram[0x107] = health;
            wram[0x74] = area;
            let initial = MetroidTarget::make_observation(
                400,
                decode_state(&wram, &cartridge).unwrap(),
                &wram,
                &wram,
                BossDefeats::default(),
                TourianEvents::default(),
            );
            let frames: Vec<_> = [5, 6, 0]
                .into_iter()
                .map(|status| {
                    let mut frame = wram;
                    frame[0x98] = status;
                    frame
                })
                .collect();
            let (observations, _) =
                decode_action_observations(&frames, &cartridge, &initial, wram).unwrap();
            let mut progress = NamedProgress::default();
            if expected {
                // The old event filter produces just the endpoint, at state 0.
                assert_eq!(observations.len(), 1);
                assert_eq!(observations[0].mother_brain_status, 0);
                assert_eq!(observations[0].frame_count, 403);
            }
            for observation in &observations {
                progress.observe(observation, 7, observations.last().unwrap().frame_count);
            }
            assert_eq!(progress.first_seen["escape_started"].is_some(), expected);
            assert_eq!(
                progress.first_seen["mother_brain_defeated"].is_some(),
                expected
            );
        }
    }
}
