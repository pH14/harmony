// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova the Squirrel memory decoder and machine-backed target adapter.
//!
//! This module is the game-knowledge boundary. The generic search code sees
//! controller actions, opaque keys, observations, and snapshots; every Nova
//! address and interpretation stays here.

use std::{error::Error, io::Write, mem::size_of, path::Path, sync::Arc};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const PLAYER_X_LOW: usize = 0x25;
const PLAYER_X_HIGH: usize = 0x26;
const PLAYER_Y_HIGH: usize = 0x27;
const PLAYER_Y_LOW: usize = 0x28;
const PLAYER_HEALTH: usize = 0x4b;
const LEVEL_NUMBER: usize = 0xa7;
const STARTED_LEVEL_NUMBER: usize = 0xa8;
const NEED_LEVEL_RELOAD: usize = 0xa9;
const CHIP_COUNT: usize = 0x508;
const CHIPS_NEEDED: usize = 0x509;
const SAVE_RAM_BASE: usize = 0x6000;
const PLAYER_ABILITY: usize = 0x7200 - SAVE_RAM_BASE;
const LEVEL_CLEARED: usize = 0x7f1f - SAVE_RAM_BASE;
const LEVEL_AVAILABLE: usize = 0x7f27 - SAVE_RAM_BASE;
const COLLECTIBLE_BITS: usize = 0x7f2f - SAVE_RAM_BASE;
const PERSISTENT_BITMAP_LEN: usize = 8;
const STATE_CHUNK_SIZE: usize = 512;

/// Number of ordinary world levels exposed by Nova's source-defined campaign.
pub const NOVA_CAMPAIGN_LEVEL_COUNT: u8 = 40;

/// A one-based, source-defined Nova campaign level.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaLevel(u8);

impl NovaLevel {
    /// Validate a one-based level number from `1` through
    /// [`NOVA_CAMPAIGN_LEVEL_COUNT`].
    pub fn from_number(number: u8) -> Result<Self, MachineError> {
        if (1..=NOVA_CAMPAIGN_LEVEL_COUNT).contains(&number) {
            Ok(Self(number))
        } else {
            Err(MachineError::Backend(format!(
                "Nova campaign level must be 1..={NOVA_CAMPAIGN_LEVEL_COUNT}, got {number}"
            )))
        }
    }

    /// The one-based level number shown to operators.
    #[must_use]
    pub fn number(self) -> u8 {
        self.0
    }

    fn index(self) -> u8 {
        self.0 - 1
    }
}

impl Default for NovaLevel {
    fn default() -> Self {
        Self(1)
    }
}

fn level_prefix_bitmap(count: u8) -> [u8; PERSISTENT_BITMAP_LEN] {
    let mut bitmap = [0_u8; PERSISTENT_BITMAP_LEN];
    for index in 0..usize::from(count) {
        bitmap[index / 8] |= 1 << (index % 8);
    }
    bitmap
}

/// A Nova input replayed from the sealed gameplay genesis.
pub type NovaInput = crate::search::archive::Input<ButtonChord>;

/// Source-derived mechanical state at one emulator frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaMechanicalState {
    /// Current internal map number.
    pub level: u8,
    /// Player-selected campaign level number.
    pub started_level: u8,
    /// Player X in whole pixels, decoded from 12.4 fixed point.
    pub x: u16,
    /// Player Y in whole pixels, decoded from 12.4 fixed point.
    pub y: u16,
    /// Current health in half-hearts.
    pub health: u8,
    /// Puzzle chips currently carried.
    pub chips: u8,
    /// Puzzle chips required by the current map.
    pub chips_needed: u8,
    /// Current copied ability.
    pub ability: u8,
    /// Whether the engine requested an internal map reload.
    pub level_reload_pending: bool,
    /// Persistent cleared-level bitmap.
    pub levels_cleared: [u8; PERSISTENT_BITMAP_LEN],
    /// Persistent available-level bitmap.
    pub levels_available: [u8; PERSISTENT_BITMAP_LEN],
    /// Persistent collectible bitmap.
    pub collectibles: [u8; PERSISTENT_BITMAP_LEN],
}

impl NovaMechanicalState {
    /// Count durable completed levels.
    #[must_use]
    pub fn cleared_count(self) -> u8 {
        self.levels_cleared
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    /// Count currently unlocked levels.
    #[must_use]
    pub fn available_count(self) -> u8 {
        self.levels_available
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    /// Count durable collectibles.
    #[must_use]
    pub fn collectible_count(self) -> u8 {
        self.collectibles
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            .try_into()
            .unwrap_or(u8::MAX)
    }
}

/// Mechanical evidence emitted at a changed spatial/resource boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaObservations {
    /// Frames emulated since the sealed gameplay genesis.
    pub frame_count: u64,
    /// Decoded source-grounded mechanical state.
    pub decoded: NovaMechanicalState,
    /// Sorted system-RAM indices changed since the prior emitted event.
    pub changed_indices: Vec<u16>,
    /// Whether health first reached zero at this event.
    pub dead: bool,
    /// Compact game-neutral mechanical log line.
    pub log_line: String,
}

/// Geometry and frame count of one rendered replay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaVideoMetadata {
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
    pub input_endpoint: NovaMechanicalState,
}

#[derive(Debug, Eq, PartialEq)]
struct SharedStateInner {
    chunks: Vec<Arc<[u8; STATE_CHUNK_SIZE]>>,
    len: usize,
}

/// Copy-on-write emulator bytes with the same Serde form as `Vec<u8>`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedState(Arc<SharedStateInner>);

impl SharedState {
    fn from_bytes(bytes: Vec<u8>, base: Option<&Self>) -> Self {
        let mut chunks = Vec::with_capacity(bytes.len().div_ceil(STATE_CHUNK_SIZE));
        for (index, source) in bytes.chunks(STATE_CHUNK_SIZE).enumerate() {
            let mut chunk = [0_u8; STATE_CHUNK_SIZE];
            chunk[..source.len()].copy_from_slice(source);
            let shared = base
                .and_then(|state| state.0.chunks.get(index))
                .filter(|existing| existing.as_ref() == &chunk)
                .cloned()
                .unwrap_or_else(|| Arc::new(chunk));
            chunks.push(shared);
        }
        Self(Arc::new(SharedStateInner {
            chunks,
            len: bytes.len(),
        }))
    }

    fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.0.len);
        for chunk in &self.0.chunks {
            let remaining = self.0.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(STATE_CHUNK_SIZE)]);
        }
        bytes
    }
}

impl Serialize for SharedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len))?;
        for index in 0..self.0.len {
            sequence.serialize_element(
                &self.0.chunks[index / STATE_CHUNK_SIZE][index % STATE_CHUNK_SIZE],
            )?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for SharedState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(bytes, None))
    }
}

/// Complete state needed to resume a Nova prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaSnapshot {
    emulator_state: SharedState,
    observation: NovaObservations,
    failed: bool,
}

impl NovaSnapshot {
    /// Decoded endpoint state carried by this snapshot.
    #[must_use]
    pub fn state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }

    pub(crate) fn resident_memory_charge(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.emulator_state.0.len)
            .saturating_add(
                self.emulator_state
                    .0
                    .chunks
                    .len()
                    .saturating_mul(size_of::<Arc<[u8; STATE_CHUNK_SIZE]>>()),
            )
            .saturating_add(
                self.observation
                    .changed_indices
                    .len()
                    .saturating_mul(size_of::<u16>()),
            )
            .saturating_add(self.observation.log_line.len())
    }
}

const JOYPAD_A: u8 = 1 << 0;
const JOYPAD_START: u8 = 1 << 3;
const JOYPAD_UP: u8 = 1 << 4;

/// Fixed title path ending at the main menu, after save-file initialization.
const BOOT_TO_MAIN_MENU: [ButtonChord; 3] = [
    ButtonChord {
        buttons: 0,
        hold_frames: 60,
    },
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 114,
    },
];

/// Fixed main-menu/level-select/pre-level path. Search inputs begin afterward.
const MAIN_MENU_TO_GAMEPLAY: [ButtonChord; 12] = [
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
    ButtonChord {
        buttons: JOYPAD_A,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
    ButtonChord {
        buttons: JOYPAD_A,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 60,
    },
];

/// QuickNES-backed target used by Nova campaigns.
#[derive(Debug)]
pub struct NovaTarget {
    machine: QuickNesMachine,
    genesis: SnapId,
    observation: NovaObservations,
    action_observations: Vec<NovaObservations>,
    failed: bool,
    snapshot_base: Option<SharedState>,
    genesis_cleared: u8,
}

impl NovaTarget {
    fn from_machine(
        mut machine: QuickNesMachine,
        selected_level: NovaLevel,
    ) -> Result<Self, MachineError> {
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&BOOT_TO_MAIN_MENU))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(power_on)?;

        // Nova initializes and validates its save file before the main menu.
        // Construct the state a normal sequential playthrough would have at
        // this boundary: prior levels are cleared and the requested level is
        // the highest available one. The game's own level-select code then
        // chooses and launches that level through ordinary controller input.
        let cleared = level_prefix_bitmap(selected_level.index());
        let available = level_prefix_bitmap(selected_level.number());
        machine.write_save_ram(LEVEL_CLEARED, &cleared)?;
        machine.write_save_ram(LEVEL_AVAILABLE, &available)?;

        let main_menu = machine.snapshot()?;
        machine.branch(main_menu, &nes::reproducer(&MAIN_MENU_TO_GAMEPLAY))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(main_menu)?;
        let genesis = machine.snapshot()?;
        let (wram, save_ram) = read_memory(&machine)?;
        let state = decode_state(&wram, &save_ram)?;
        if state.health == 0
            || state.x == 0
            || state.y == 0
            || state.started_level != selected_level.index()
        {
            return Err(MachineError::Backend(format!(
                "Nova setup did not reach requested level {}: health={} x={} y={} started_level={}",
                selected_level.number(),
                state.health,
                state.x,
                state.y,
                state.started_level,
            )));
        }
        let observation = NovaObservations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            dead: false,
            log_line: "frame=0 changed=[]".to_owned(),
        };
        Ok(Self {
            machine,
            genesis,
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            snapshot_base: None,
            genesis_cleared: state.cleared_count(),
        })
    }

    /// Load Nova and seal gameplay genesis through the pinned QuickNES core.
    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_headless_at_level(rom, core_path, core_sha256, NovaLevel::default())
    }

    /// Load Nova and seal genesis at one independently selected campaign
    /// level through the game's normal menus.
    pub fn from_rom_bytes_headless_at_level(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        selected_level: NovaLevel,
    ) -> Result<Self, MachineError> {
        Self::from_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            selected_level,
        )
    }

    /// Current decoded state.
    #[must_use]
    pub fn mechanical_state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }

    /// Whether the current state has no health.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.decoded.health == 0
    }

    /// Whether this input durably cleared a level beyond sealed genesis.
    #[must_use]
    pub fn cleared_a_level(&self) -> bool {
        self.observation.decoded.cleared_count() > self.genesis_cleared
    }

    /// Total deterministic frames this instance has emulated.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    /// Observer events emitted by the most recent action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[NovaObservations] {
        &self.action_observations
    }

    /// Test one fixed continuation and restore the caller's state afterward.
    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return false;
        }
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return false;
        };
        let mut actions = Vec::new();
        let mut remaining = frames;
        while remaining > 0 {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            let Ok(hold) = u8::try_from(hold) else {
                self.failed = true;
                let _ = self.machine.drop_snapshot(start);
                return false;
            };
            actions.push(ButtonChord::new(buttons, hold));
            remaining -= u16::from(hold);
        }
        if self
            .machine
            .branch(start, &nes::reproducer(&actions))
            .is_err()
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(start);
            return false;
        }
        let mut survived = true;
        while let Ok(advanced) = self.run_one_frame() {
            if !advanced {
                break;
            }
            let Ok((wram, save_ram)) = read_memory(&self.machine) else {
                self.failed = true;
                survived = false;
                break;
            };
            let Ok(state) = decode_state(&wram, &save_ram) else {
                self.failed = true;
                survived = false;
                break;
            };
            if state.health == 0 {
                survived = false;
                break;
            }
        }
        let restore = self.machine.replay(start);
        let drop = self.machine.drop_snapshot(start);
        if restore.is_err() || drop.is_err() {
            self.failed = true;
            false
        } else {
            survived
        }
    }

    /// Replay an input from gameplay genesis and write RGB24 video and S16LE
    /// stereo audio.
    ///
    /// Video is a replay-only observer. Search workers never enable it, and
    /// the recorded headless campaign identity is unchanged.
    pub fn render_input(
        &mut self,
        input: &NovaInput,
        tail_frames: u32,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<NovaVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Nova gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            for action in &input.actions {
                self.render_action(*action, video_output, audio_output, &mut metadata)?;
            }
            let (endpoint_wram, endpoint_save_ram) = read_memory(&self.machine)?;
            let input_endpoint = decode_state(&endpoint_wram, &endpoint_save_ram)?;
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
        metadata: &mut Option<NovaVideoMetadata>,
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
                    *metadata = Some(NovaVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        input_endpoint: NovaMechanicalState::default(),
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
        state: NovaMechanicalState,
        wram: &[u8; WRAM_SIZE],
        prior_wram: &[u8; WRAM_SIZE],
    ) -> NovaObservations {
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
        NovaObservations {
            frame_count,
            decoded: state,
            changed_indices: changed_indices.clone(),
            dead: state.health == 0,
            log_line: format!("frame={frame_count} changed={changed_indices:?}"),
        }
    }
}

impl Target for NovaTarget {
    type Action = ButtonChord;
    type Observations = NovaObservations;
    type Snapshot = NovaSnapshot;

    fn reset(&mut self) {
        self.failed = self.machine.replay(self.genesis).is_err();
        self.snapshot_base = None;
        if let Ok((wram, save_ram)) = read_memory(&self.machine)
            && let Ok(state) = decode_state(&wram, &save_ram)
        {
            self.observation = self.make_observation(0, state, &wram, &[0; WRAM_SIZE]);
        } else {
            self.failed = true;
        }
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return;
        }
        let Ok((mut prior_wram, prior_save)) = read_memory(&self.machine) else {
            self.failed = true;
            return;
        };
        let Ok(mut prior_state) = decode_state(&prior_wram, &prior_save) else {
            self.failed = true;
            return;
        };
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return;
        };
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(start);
            return;
        }
        let _ = self.machine.drop_snapshot(start);
        let mut executed_frames = 0_u64;
        for _ in 0..action.bounded_hold_frames() {
            match self.run_one_frame() {
                Ok(true) => {}
                Ok(false) | Err(()) => break,
            }
            executed_frames = executed_frames.saturating_add(1);
            let Ok((wram, save_ram)) = read_memory(&self.machine) else {
                self.failed = true;
                break;
            };
            let Ok(state) = decode_state(&wram, &save_ram) else {
                self.failed = true;
                break;
            };
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
                || state.level_reload_pending != prior_state.level_reload_pending;
            if boundary {
                let observation = self.make_observation(
                    self.observation.frame_count.saturating_add(executed_frames),
                    state,
                    &wram,
                    &prior_wram,
                );
                prior_wram = wram;
                prior_state = state;
                self.action_observations.push(observation);
            }
            if state.health == 0 || state.cleared_count() > self.genesis_cleared {
                break;
            }
        }
        let endpoint_frame = self.observation.frame_count.saturating_add(executed_frames);
        if !self
            .action_observations
            .last()
            .is_some_and(|observation| observation.frame_count == endpoint_frame)
            && let Ok((wram, save_ram)) = read_memory(&self.machine)
            && let Ok(state) = decode_state(&wram, &save_ram)
        {
            self.action_observations.push(self.make_observation(
                endpoint_frame,
                state,
                &wram,
                &prior_wram,
            ));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        (u64::from(state.started_level) << 40)
            | (u64::from(state.level) << 32)
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
        let snap = match self.machine.snapshot() {
            Ok(snap) => snap,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        let bytes = match self.machine.take_snapshot(snap) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        let emulator_state = SharedState::from_bytes(bytes, self.snapshot_base.as_ref());
        self.snapshot_base = Some(emulator_state.clone());
        Some(NovaSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        self.machine
            .restore_bytes(&snapshot.emulator_state.materialize())
            .map_err(|error| error.to_string())?;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

fn read_memory(machine: &QuickNesMachine) -> Result<([u8; WRAM_SIZE], Vec<u8>), MachineError> {
    Ok((machine.read_wram()?, machine.read_save_ram()?))
}

fn read_byte(bytes: &[u8], address: usize) -> Result<u8, MachineError> {
    bytes
        .get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Nova RAM address {address:#x} is absent")))
}

fn read_bitmap(bytes: &[u8], address: usize) -> Result<[u8; PERSISTENT_BITMAP_LEN], MachineError> {
    bytes
        .get(address..address.saturating_add(PERSISTENT_BITMAP_LEN))
        .ok_or_else(|| MachineError::Backend(format!("Nova bitmap at {address:#x} is absent")))?
        .try_into()
        .map_err(|_| MachineError::Backend(format!("Nova bitmap at {address:#x} is truncated")))
}

fn fixed_point_pixels(high: u8, low: u8) -> u16 {
    u16::from(high) * 16 + u16::from(low >> 4)
}

/// Decode Nova's work/save-RAM observations from bounded external slices.
pub fn decode_state(wram: &[u8], save_ram: &[u8]) -> Result<NovaMechanicalState, MachineError> {
    Ok(NovaMechanicalState {
        level: read_byte(wram, LEVEL_NUMBER)?,
        started_level: read_byte(wram, STARTED_LEVEL_NUMBER)?,
        x: fixed_point_pixels(
            read_byte(wram, PLAYER_X_HIGH)?,
            read_byte(wram, PLAYER_X_LOW)?,
        ),
        y: fixed_point_pixels(
            read_byte(wram, PLAYER_Y_HIGH)?,
            read_byte(wram, PLAYER_Y_LOW)?,
        ),
        health: read_byte(wram, PLAYER_HEALTH)?,
        chips: read_byte(wram, CHIP_COUNT)?,
        chips_needed: read_byte(wram, CHIPS_NEEDED)?,
        ability: read_byte(save_ram, PLAYER_ABILITY)?,
        level_reload_pending: read_byte(wram, NEED_LEVEL_RELOAD)? != 0,
        levels_cleared: read_bitmap(save_ram, LEVEL_CLEARED)?,
        levels_available: read_bitmap(save_ram, LEVEL_AVAILABLE)?,
        collectibles: read_bitmap(save_ram, COLLECTIBLE_BITS)?,
    })
}

/// Coarse location used by observation emission and archive cells.
#[must_use]
pub fn spatial_bucket(state: NovaMechanicalState) -> (u8, u8, u16, u16) {
    (state.started_level, state.level, state.x / 32, state.y / 32)
}

/// Adapter-owned lexicographic preference, opaque to the search coordinator.
#[must_use]
pub fn preference_tuple(state: NovaMechanicalState) -> (u8, u8, u8, bool, u8, u8) {
    (
        state.cleared_count(),
        state.collectible_count(),
        state.available_count(),
        state.ability != 0,
        state.health,
        state.chips,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_fixture_uses_one_based_campaign_levels() {
        assert!(NovaLevel::from_number(0).is_err());
        assert!(NovaLevel::from_number(NOVA_CAMPAIGN_LEVEL_COUNT + 1).is_err());
        let first = NovaLevel::from_number(1).expect("first level");
        let last = NovaLevel::from_number(40).expect("last level");
        assert_eq!((first.number(), first.index()), (1, 0));
        assert_eq!((last.number(), last.index()), (40, 39));
        assert_eq!(level_prefix_bitmap(0), [0; PERSISTENT_BITMAP_LEN]);
        assert_eq!(level_prefix_bitmap(1)[0], 0x01);
        assert_eq!(&level_prefix_bitmap(9)[..2], &[0xff, 0x01]);
        assert_eq!(&level_prefix_bitmap(40)[..5], &[0xff; 5]);
    }

    #[test]
    fn decoder_reads_source_mapped_state() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[PLAYER_X_HIGH] = 0x34;
        wram[PLAYER_X_LOW] = 0xa0;
        wram[PLAYER_Y_HIGH] = 0x0b;
        wram[PLAYER_Y_LOW] = 0x80;
        wram[PLAYER_HEALTH] = 4;
        wram[LEVEL_NUMBER] = 9;
        wram[STARTED_LEVEL_NUMBER] = 7;
        wram[CHIP_COUNT] = 3;
        wram[CHIPS_NEEDED] = 5;
        let mut save = vec![0_u8; 8 * 1024];
        save[PLAYER_ABILITY] = 6;
        save[LEVEL_CLEARED] = 0b1011;
        save[LEVEL_AVAILABLE] = 0xff;
        save[COLLECTIBLE_BITS + 7] = 0x80;
        let state = decode_state(&wram, &save).expect("decode fixture");
        assert_eq!((state.x, state.y), (0x34a, 0x0b8));
        assert_eq!((state.level, state.started_level), (9, 7));
        assert_eq!((state.health, state.chips, state.chips_needed), (4, 3, 5));
        assert_eq!((state.cleared_count(), state.available_count()), (3, 8));
        assert_eq!(state.collectible_count(), 1);
    }

    #[test]
    fn decoder_rejects_short_untrusted_regions() {
        assert!(decode_state(&[], &[0; 8 * 1024]).is_err());
        assert!(decode_state(&[0; WRAM_SIZE], &[]).is_err());
        assert!(decode_state(&[0; CHIP_COUNT], &[0; 8 * 1024]).is_err());
        assert!(decode_state(&[0; WRAM_SIZE], &[0; COLLECTIBLE_BITS + 7]).is_err());
    }
}
