// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QuickNesMachine, VideoFrame},
};
use serde::{Deserialize, Serialize};

use super::progress::{BossDefeats, TourianEvents};
use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const GAME_MODE: usize = 0x1e;
const GAME_MODE_PLAYING: u8 = 0x03;
const SCROLL_DIRECTION: usize = 0x49;
const SCROLL_UP: u8 = 0;
const SCROLL_DOWN: u8 = 1;
const SCROLL_LEFT: u8 = 2;
const MAP_Y: usize = 0x4f;
const MAP_X: usize = 0x50;
const SCROLL_Y: usize = 0xfc;
const SCROLL_X: usize = 0xfd;
const PPU_CONTROL: usize = 0xff;
const PPU_NAME_TABLE_MASK: u8 = 0x03;
const SAMUS_NAME_TABLE: usize = 0x30c;
const SAMUS_Y: usize = 0x30d;
const SAMUS_X: usize = 0x30e;
const DOOR_STATE: usize = 0x56;
const AREA: usize = 0x74;
const AREA_BRINSTAR: u8 = 0x10;
const POSE: usize = 0x300;
const POSE_UNMOVED: u8 = 0xff;
const POSE_STANDING: u8 = 0x00;
const POSE_RUNNING: u8 = 0x01;
const POSE_AIRBORNE: u8 = 0x02;
pub const POSTURE_GROUNDED: u8 = 0;
pub const POSTURE_AIRBORNE: u8 = 1;
pub const POSTURE_OTHER: u8 = 2;

const ENEMY_SLOT_BASE: usize = 0x400;
const ENEMY_SLOT_STRIDE: usize = 0x10;
const ENEMY_SLOTS: usize = 6;
const ENEMY_HIT_POINTS: usize = 0x0b;
const ENEMY_SPECIAL_ATTRIBUTES: usize = 0x0f;
const ENEMY_MINI_BOSS_BIT: u8 = 1 << 6;
const ENEMY_HIT_POINTS_ABSENT: u8 = 0xff;

const HEALTH_HIGH: usize = 0x107;
const HEALTH_LOW: usize = 0x106;
pub const STARTING_HEALTH_TENTHS: u16 = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenesisDepth {
    NewGame,
    Rooted,
}

impl GenesisDepth {
    fn accepts(self, health: u16) -> bool {
        match self {
            Self::NewGame => health == STARTING_HEALTH_TENTHS,
            Self::Rooted => health > 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MetroidTerminalPolicy {
    Legacy,
    #[default]
    BcdUnderflow,
}

impl MetroidTerminalPolicy {
    #[must_use]
    pub fn identifier(self) -> &'static str {
        match self {
            Self::Legacy => "death_or_ending_v2",
            Self::BcdUnderflow => "death_or_bcd_underflow_or_ending_v3",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "death_or_ending_v2" => Ok(Self::Legacy),
            "death_or_bcd_underflow_or_ending_v3" => Ok(Self::BcdUnderflow),
            _ => Err("unknown Metroid terminal policy".into()),
        }
    }

    #[must_use]
    pub fn is_dead(self, state: MetroidMechanicalState) -> bool {
        state.is_dead() || (self == Self::BcdUnderflow && state.health >= BCD_BORROW_HEALTH)
    }
}

pub const CARTRIDGE_RAM_BASE: u64 = 0x6000;
const EQUIPMENT: usize = 0x878;
const MISSILES: usize = 0x879;
const MISSILE_CAPACITY: usize = 0x87a;
const KRAID_STATUS: usize = 0x87b;
const RIDLEY_STATUS: usize = 0x87c;
const KRAID_DEFEATED_BIT: u8 = 1;
const RIDLEY_DEFEATED_BIT: u8 = 2;
const ENDING: usize = 0x883;
const ENERGY_TANKS: usize = 0x877;

const JOYPAD_START: u8 = 1 << 3;

pub type MetroidInput = crate::search::archive::Input<ButtonChord>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidMechanicalState {
    pub area: u8,
    pub map_x: u8,
    pub map_y: u8,
    pub x: u8,
    pub y: u8,
    pub pose: u8,
    pub door: u8,
    pub mode: u8,
    pub health: u16,
    pub equipment: u8,
    pub missiles: u8,
    pub missile_capacity: u8,
    pub energy_tanks: u8,
    pub boss_health: u8,
    pub bosses: u8,
    pub ending: bool,
}

impl MetroidMechanicalState {
    #[must_use]
    pub fn items(self) -> u8 {
        u8::try_from(self.equipment.count_ones())
            .unwrap_or(u8::MAX)
            .saturating_add(self.bosses)
    }

    #[must_use]
    pub fn collectibles(self) -> u8 {
        let awarded = u16::from(BOSS_MISSILE_AWARD) * u16::from(self.bosses);
        let from_tanks = u16::from(self.missile_capacity).saturating_sub(awarded);
        u8::try_from(from_tanks / u16::from(MISSILE_TANK_STEP))
            .unwrap_or(u8::MAX)
            .saturating_add(self.energy_tanks)
    }

    #[must_use]
    pub fn is_victory(self) -> bool {
        self.ending
    }

    #[must_use]
    pub fn posture(self) -> u8 {
        match self.pose {
            POSE_UNMOVED | POSE_STANDING | POSE_RUNNING => POSTURE_GROUNDED,
            POSE_AIRBORNE => POSTURE_AIRBORNE,
            _ => POSTURE_OTHER,
        }
    }

    #[must_use]
    pub fn is_dead(self) -> bool {
        self.health == 0
    }

    #[must_use]
    pub fn in_play(self) -> bool {
        self.mode == GAME_MODE_PLAYING
    }
}

const MISSILE_TANK_STEP: u8 = 5;
const MAX_ENERGY_TANKS: u8 = 6;
const CARTRIDGE_RAM_SIZE: usize = 8192;

fn health_cap(energy_tanks: u8) -> u16 {
    (u16::from(energy_tanks) + 1) * 1000 - 1
}

fn to_bcd(value: u16) -> u8 {
    u8::try_from((value / 10) * 16 + value % 10).unwrap_or(u8::MAX)
}
const BOSS_MISSILE_AWARD: u8 = 75;
const BCD_BORROW_HEALTH: u16 = 8000;

fn bcd(byte: u8) -> u16 {
    u16::from(byte >> 4) * 10 + u16::from(byte & 0x0f)
}

fn mini_boss_health(wram: &[u8]) -> Result<u8, MachineError> {
    for slot in 0..ENEMY_SLOTS {
        let base = ENEMY_SLOT_BASE + slot * ENEMY_SLOT_STRIDE;
        if read_byte(wram, base + ENEMY_SPECIAL_ATTRIBUTES)? & ENEMY_MINI_BOSS_BIT == 0 {
            continue;
        }
        let health = read_byte(wram, base + ENEMY_HIT_POINTS)?;
        if health != ENEMY_HIT_POINTS_ABSENT {
            return Ok(health);
        }
    }
    Ok(0)
}

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
        boss_health: mini_boss_health(wram)?,
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

#[must_use]
pub fn preference_tuple(state: MetroidMechanicalState) -> (u8, u8, u16, u8) {
    (
        state.items(),
        state.collectibles(),
        state.health,
        state.missiles,
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidObservations {
    pub frame_count: u64,
    pub decoded: MetroidMechanicalState,
    pub boss_defeats: BossDefeats,
    pub mother_brain_status: u8,
    pub tourian_events: TourianEvents,
    pub changed_indices: Vec<u16>,
    pub dead: bool,
    pub log_line: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidSnapshot {
    emulator_state: Vec<u8>,
    observation: MetroidObservations,
    failed: bool,
}

impl MetroidSnapshot {
    #[cfg(test)]
    pub(crate) fn for_census_tests(decoded: MetroidMechanicalState) -> Self {
        Self {
            emulator_state: Vec::new(),
            observation: MetroidObservations {
                frame_count: 0,
                decoded,
                boss_defeats: BossDefeats::default(),
                mother_brain_status: 0,
                tourian_events: TourianEvents::default(),
                changed_indices: Vec::new(),
                dead: false,
                log_line: String::new(),
            },
            failed: false,
        }
    }

    #[must_use]
    pub fn state(&self) -> MetroidMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.len()
    }
}

const TITLE_SETTLE_FRAMES: u32 = 300;
const MENU_SETTLE_FRAMES: u32 = 180;
const BOOT_PRESSES: usize = 2;
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
    execution_work: u64,
    terminal_policy: MetroidTerminalPolicy,
}

impl MetroidTarget {
    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_after(rom, core_path, core_sha256, &power_on_walk())
    }

    pub fn from_rom_bytes_capturing(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let image = nes::with_cartridge_ram(rom)?;
        let mut machine = QuickNesMachine::from_rom_bytes(&image, core_path, core_sha256)?;
        machine.set_video_capture(true);
        machine.set_audio_capture(true);
        Self::from_machine(machine, &power_on_walk(), GenesisDepth::NewGame)
    }

    #[must_use]
    pub fn current_wram(&self) -> &[u8] {
        &self.current_wram
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

    pub fn from_rom_bytes_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_rooted(rom, core_path, core_sha256, prefix, GenesisDepth::NewGame)
    }

    pub fn from_rom_bytes_rooted(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: &[ButtonChord],
        depth: GenesisDepth,
    ) -> Result<Self, MachineError> {
        let image = nes::with_cartridge_ram(rom)?;
        Self::from_machine(
            QuickNesMachine::from_rom_bytes(&image, core_path, core_sha256)?,
            prefix,
            depth,
        )
    }

    fn from_machine(
        mut machine: QuickNesMachine,
        prefix: &[ButtonChord],
        depth: GenesisDepth,
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
            if state.in_play() && depth.accepts(state.health) {
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
            execution_work: 0,
            terminal_policy: MetroidTerminalPolicy::default(),
        })
    }

    #[must_use]
    pub fn with_terminal_policy(mut self, policy: MetroidTerminalPolicy) -> Self {
        self.terminal_policy = policy;
        self.observation.dead = policy.is_dead(self.observation.decoded);
        self.genesis_observation.dead = policy.is_dead(self.genesis_observation.decoded);
        self.action_observations = vec![self.observation.clone()];
        self
    }

    #[must_use]
    pub fn genesis_prefix(&self) -> &[ButtonChord] {
        &self.genesis_prefix
    }

    #[must_use]
    pub fn mechanical_state(&self) -> MetroidMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.dead
    }

    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.observation.decoded.is_victory()
    }

    #[must_use]
    pub fn genesis_holdings(&self) -> (u8, u8) {
        let state = self.genesis_observation.decoded;
        (state.items(), state.collectibles())
    }

    #[must_use]
    pub fn gained_an_item(&self) -> bool {
        let (items, tanks) = self.genesis_holdings();
        let state = self.observation.decoded;
        state.items() > items || state.collectibles() > tanks
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
    pub fn diagnostic_enemy_slots(&self) -> Vec<(u8, u8, u8, u8, u8)> {
        (0..ENEMY_SLOTS)
            .map(|slot| {
                let base = ENEMY_SLOT_BASE + slot * ENEMY_SLOT_STRIDE;
                (
                    self.current_wram[base],
                    self.current_wram[base + 1],
                    self.current_wram[base + ENEMY_HIT_POINTS],
                    self.current_wram[base + ENEMY_SPECIAL_ATTRIBUTES],
                    self.current_wram[0x30],
                )
            })
            .collect()
    }

    pub fn diagnostic_set_resources(
        &mut self,
        health: u16,
        missiles: u8,
    ) -> Result<(), Box<dyn Error>> {
        let state = self.mechanical_state();
        if self.failed
            || !state.in_play()
            || self.is_dead()
            || self.is_victory()
            || state.energy_tanks > MAX_ENERGY_TANKS
            || state.health == 0
            || state.health > health_cap(state.energy_tanks)
            || state.missiles > state.missile_capacity
            || health == 0
            || health > health_cap(state.energy_tanks)
            || missiles > state.missile_capacity
        {
            return Err("resource intervention exceeds the live state's earned capacities".into());
        }
        let wram = self.machine.read_wram()?;
        let cartridge = self.cartridge()?;
        if decode_state(&wram, &cartridge)? != state || wram != self.current_wram {
            return Err("resource intervention requires a consistent paused boundary".into());
        }
        let before = self
            .snapshot()
            .ok_or("resource intervention snapshot failed")?;
        let clock = self.frames_clocked();
        let mut expected_wram = wram;
        expected_wram[HEALTH_LOW] = to_bcd(health % 100);
        expected_wram[HEALTH_HIGH] = to_bcd(health / 100);
        let mut expected_cartridge = cartridge.clone();
        expected_cartridge[MISSILES] = missiles;
        let changes = [
            (wram[HEALTH_LOW], expected_wram[HEALTH_LOW]),
            (wram[HEALTH_HIGH], expected_wram[HEALTH_HIGH]),
            (cartridge[MISSILES], missiles),
        ];
        let applied = (|| -> Result<(), Box<dyn Error>> {
            self.machine
                .poke_wram(HEALTH_LOW, expected_wram[HEALTH_LOW]);
            self.machine
                .poke_wram(HEALTH_HIGH, expected_wram[HEALTH_HIGH]);
            self.machine.write_save_ram(MISSILES, &[missiles])?;
            if self.machine.read_wram()? != expected_wram
                || self.cartridge()? != expected_cartridge
                || self.frames_clocked() != clock
            {
                return Err("resource intervention changed another RAM byte or the clock".into());
            }
            let mut expected_state = state;
            expected_state.health = health;
            expected_state.missiles = missiles;
            if decode_state(&expected_wram, &expected_cartridge)? != expected_state {
                return Err("resource intervention changed another mechanical field".into());
            }
            let after = self.snapshot().ok_or("intervened snapshot failed")?;
            if !only_resource_bytes_changed(&before.emulator_state, &after.emulator_state, &changes)
            {
                return Err("resource intervention changed unexpected serialized bytes".into());
            }
            self.current_wram = expected_wram;
            self.observation.decoded = expected_state;
            self.action_observations = vec![self.observation.clone()];
            Ok(())
        })();
        if let Err(error) = applied {
            self.machine.poke_wram(HEALTH_LOW, wram[HEALTH_LOW]);
            self.machine.poke_wram(HEALTH_HIGH, wram[HEALTH_HIGH]);
            self.machine
                .write_save_ram(MISSILES, &[cartridge[MISSILES]])?;
            self.restore(&before)?;
            return Err(error);
        }
        Ok(())
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[MetroidObservations] {
        &self.action_observations
    }

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
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        let Ok(cartridge) = self.cartridge() else {
            self.failed = true;
            return;
        };
        match decode_action_observations(
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

fn decode_action_observations(
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

fn resource_ram_payload(bytes: &[u8], cartridge: bool) -> Option<usize> {
    if bytes.get(..8)? != b"HQNESST2"
        || bytes.get(120..128)? != b"NESS\xff\xff\xff\xff"
        || u64::from_le_bytes(bytes.get(112..120)?.try_into().ok()?) != (bytes.len() - 120) as u64
    {
        return None;
    }
    let mut offset = 128_usize;
    let mut found = None;
    while offset < bytes.len() {
        let end = offset.checked_add(8)?;
        let tag = bytes.get(offset..offset.checked_add(4)?)?;
        let size = usize::try_from(u32::from_le_bytes(
            bytes.get(offset + 4..end)?.try_into().ok()?,
        ))
        .ok()?;
        let next = end.checked_add(size)?;
        if next > bytes.len() {
            return None;
        }
        let matches = if cartridge {
            tag == b"SRAM"
        } else {
            tag == b"LRAM" || tag == b"WRAM"
        };
        if matches {
            if found.is_some()
                || size
                    != if cartridge {
                        CARTRIDGE_RAM_SIZE
                    } else {
                        WRAM_SIZE
                    }
            {
                return None;
            }
            found = Some(end);
        }
        offset = next;
    }
    found
}

fn only_resource_bytes_changed(before: &[u8], after: &[u8], changes: &[(u8, u8)]) -> bool {
    if before.len() != after.len() || changes.len() != 3 {
        return false;
    }
    let mut expected = before.to_vec();
    for (index, &(old, new)) in changes.iter().enumerate() {
        if old == new {
            continue;
        }
        let Some(payload) = resource_ram_payload(before, index == 2) else {
            return false;
        };
        let address = payload + [HEALTH_LOW, HEALTH_HIGH, MISSILES][index];
        if expected[address] != old {
            return false;
        }
        expected[address] = new;
    }
    expected == after
}

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

    fn resource_fixture() -> MetroidTarget {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).unwrap();
        machine.write_save_ram(0, &[0; CARTRIDGE_RAM_SIZE]).unwrap();
        machine.poke_wram(GAME_MODE, GAME_MODE_PLAYING);
        machine.poke_wram(HEALTH_HIGH, 3);
        machine.write_save_ram(ENERGY_TANKS, &[1]).unwrap();
        machine.write_save_ram(MISSILE_CAPACITY, &[20]).unwrap();
        let mut target = MetroidTarget::from_machine(machine, &[], GenesisDepth::NewGame).unwrap();
        target.diagnostic_set_resources(79, 0).unwrap();
        target
    }

    #[test]
    fn a_rooted_genesis_accepts_any_live_health_and_a_new_game_does_not() {
        assert!(GenesisDepth::NewGame.accepts(STARTING_HEALTH_TENTHS));
        assert!(!GenesisDepth::NewGame.accepts(STARTING_HEALTH_TENTHS - 1));
        assert!(!GenesisDepth::NewGame.accepts(0));
        assert!(GenesisDepth::Rooted.accepts(STARTING_HEALTH_TENTHS));
        assert!(GenesisDepth::Rooted.accepts(1));
        assert!(!GenesisDepth::Rooted.accepts(0));
    }

    #[test]
    fn resource_intervention_preserves_noop_and_rejects_unserialized_changes() {
        let mut target = resource_fixture();
        let before = target.snapshot().unwrap();
        let clock = target.frames_clocked();
        target.diagnostic_set_resources(79, 0).unwrap();
        assert_eq!(target.snapshot().unwrap(), before);
        target.diagnostic_set_resources(1999, 0).unwrap();
        let mut expected = before.state();
        expected.health = 1999;
        assert_eq!(target.mechanical_state(), expected);
        assert_eq!(target.frames_clocked(), clock);
        target.restore(&before).unwrap();
        assert!(target.diagnostic_set_resources(1999, 20).is_err());
        assert_eq!(target.snapshot().unwrap(), before);
        assert_eq!(target.cartridge().unwrap()[MISSILES], 0);
        for (health, missiles) in [(0, 0), (2000, 0), (1999, 21)] {
            assert!(target.diagnostic_set_resources(health, missiles).is_err());
            assert_eq!(target.snapshot().unwrap(), before);
        }
        target.machine.poke_wram(HEALTH_LOW, 0);
        target.machine.poke_wram(HEALTH_HIGH, 0x20);
        target.current_wram = target.machine.read_wram().unwrap();
        target.observation.decoded.health = 2000;
        let invalid = target.snapshot().unwrap();
        assert!(target.diagnostic_set_resources(1999, 0).is_err());
        assert_eq!(target.snapshot().unwrap(), invalid);
    }

    #[test]
    fn resource_snapshot_guard_rejects_extra_missing_or_malformed_bytes() {
        let mut target = resource_fixture();
        let before = target.snapshot().unwrap().emulator_state;
        target.diagnostic_set_resources(1999, 0).unwrap();
        let after = target.snapshot().unwrap().emulator_state;
        let changes = [(0x79, 0x99), (0, 0x19), (0, 0)];
        assert!(only_resource_bytes_changed(&before, &after, &changes));
        assert!(!only_resource_bytes_changed(&before, &before, &changes));
        assert!(!only_resource_bytes_changed(
            &before,
            &after[..after.len() - 1],
            &changes
        ));
        let mut wrong_address = after.clone();
        let payload = resource_ram_payload(&before, false).unwrap();
        wrong_address[payload + HEALTH_HIGH] = 0;
        wrong_address[payload + HEALTH_HIGH + 1] = 0x19;
        assert!(!only_resource_bytes_changed(
            &before,
            &wrong_address,
            &changes
        ));
        let mut malformed = before.clone();
        malformed[112] ^= 1;
        assert!(resource_ram_payload(&malformed, false).is_none());
    }

    #[test]
    fn the_default_terminal_policy_rejects_the_bcd_borrow_value() {
        assert_eq!(
            MetroidTerminalPolicy::default(),
            MetroidTerminalPolicy::BcdUnderflow
        );
        let underflowed = MetroidMechanicalState {
            health: 9990,
            ..MetroidMechanicalState::default()
        };
        assert!(MetroidTerminalPolicy::default().is_dead(underflowed));
        assert!(!MetroidTerminalPolicy::Legacy.is_dead(underflowed));
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
        let (legacy, _) = decode_action_observations(
            &[underflow],
            &cartridge,
            &initial,
            start,
            MetroidTerminalPolicy::Legacy,
        )
        .unwrap();
        assert!(!legacy[0].dead);
        let (corrected, stopped_wram) = decode_action_observations(
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
        assert_eq!(stopped_wram, underflow);
        assert_ne!(stopped_wram, cleared);
        for (high, low) in [(0x69, 0x99), (0x19, 0x99), (0, 0x12)] {
            let mut live = start;
            live[HEALTH_HIGH] = high;
            live[HEALTH_LOW] = low;
            let (observations, _) = decode_action_observations(
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
            let (observations, _) = decode_action_observations(
                &frames,
                &cartridge,
                &initial,
                wram,
                MetroidTerminalPolicy::Legacy,
            )
            .unwrap();
            let mut progress = NamedProgress::default();
            if expected {
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
