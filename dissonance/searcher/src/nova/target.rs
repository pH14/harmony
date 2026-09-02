// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova the Squirrel memory decoder and machine-backed target adapter.
//!
//! This module is the game-knowledge boundary. The generic search code sees
//! controller actions, opaque keys, observations, and snapshots; every Nova
//! address and interpretation stays here.

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

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
const SAVE_RAM_SIZE: usize = 0x2000;
const PLAYER_ABILITY: usize = 0x7200 - SAVE_RAM_BASE;
const LEVEL_CLEARED: usize = 0x7f1f - SAVE_RAM_BASE;
const LEVEL_AVAILABLE: usize = 0x7f27 - SAVE_RAM_BASE;
const COLLECTIBLE_BITS: usize = 0x7f2f - SAVE_RAM_BASE;
const PERSISTENT_BITMAP_LEN: usize = 8;

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

/// Complete state needed to resume a Nova prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaSnapshot<P = machine::SharedState> {
    pub(crate) emulator_state: P,
    pub(crate) observation: NovaObservations,
    pub(crate) wram: Vec<u8>,
    pub(crate) failed: bool,
}

impl<P> NovaSnapshot<P> {
    /// Decoded endpoint state carried by this snapshot.
    #[must_use]
    pub fn state(&self) -> NovaMechanicalState {
        self.observation.decoded
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

/// Machine-backed target used by Nova campaigns.
#[derive(Debug)]
pub struct NovaTarget<M: Machine = QuickNesMachine> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: NovaObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: NovaObservations,
    action_observations: Vec<NovaObservations>,
    failed: bool,
    snapshot_base: Option<M::Portable>,
    genesis_cleared: u8,
}

impl<M: Machine> NovaTarget<M> {
    /// Seal a machine that is already stopped at Nova gameplay genesis.
    ///
    /// The constructor reads the two NES memory windows through the machine
    /// boundary, validates that they contain a live player state, and retains
    /// one snapshot handle for deterministic reset. The machine must already
    /// have completed all title, menu, and level-select setup.
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let (wram, save_ram) = read_memory(&machine)?;
        let state = decode_state(&wram, &save_ram)?;
        if state.health == 0 || state.x == 0 || state.y == 0 {
            return Err(MachineError::Backend(
                "Nova machine is not at live gameplay genesis".to_owned(),
            ));
        }
        let power_on = machine.snapshot()?;
        let observation = NovaObservations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            dead: false,
            log_line: "frame=0 changed=[]".to_owned(),
        };
        Ok(Self {
            machine,
            genesis: power_on,
            current: power_on,
            genesis_observation: observation.clone(),
            genesis_wram: wram,
            current_wram: wram,
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            snapshot_base: None,
            genesis_cleared: state.cleared_count(),
        })
    }
}

impl NovaTarget<QuickNesMachine> {
    fn from_quicknes_machine(
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
        Self::from_machine(machine)
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
        Self::from_quicknes_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            selected_level,
        )
    }
}

impl<M: Machine> NovaTarget<M> {
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
        if frames == 0 {
            return false;
        }
        let mut actions = Vec::new();
        let mut remaining = frames;
        while remaining > 0 {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            let Ok(hold) = u8::try_from(hold) else {
                self.failed = true;
                return false;
            };
            actions.push(ButtonChord::new(buttons, hold));
            remaining -= u16::from(hold);
        }
        let current = self.current;
        if self
            .machine
            .branch(current, &nes::reproducer(&actions))
            .is_err()
        {
            self.failed = true;
            return false;
        }
        let requested_frames = usize::from(frames);
        let mut observed_frames = 0_usize;
        let mut survived = true;
        while observed_frames < requested_frames {
            let stop = self.machine.run(StopConditions::default(), None);
            let produced = self.machine.frames();
            if produced.is_empty() {
                survived = false;
                break;
            }
            let remaining = requested_frames.saturating_sub(observed_frames);
            let take = produced.len().min(remaining);
            let save_ram = match self
                .machine
                .read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)
            {
                Ok(save_ram) => save_ram,
                Err(_) => {
                    self.failed = true;
                    survived = false;
                    Vec::new()
                }
            };
            if !survived {
                break;
            }
            let acceptable_stop = matches!(
                stop,
                Ok(machine::StopReason::SnapshotPoint { .. }
                    | machine::StopReason::Deadline { .. }
                    | machine::StopReason::Quiescent { .. })
            );
            if !acceptable_stop {
                self.failed = true;
                survived = false;
                break;
            }
            for wram in produced.iter().take(take) {
                match decode_state(wram, &save_ram) {
                    Ok(state) if state.health != 0 => {}
                    Ok(_) => {
                        survived = false;
                        break;
                    }
                    Err(_) => {
                        self.failed = true;
                        survived = false;
                        break;
                    }
                }
            }
            observed_frames = observed_frames.saturating_add(take);
            if !survived || observed_frames >= requested_frames {
                break;
            }
            match stop {
                Ok(
                    machine::StopReason::SnapshotPoint { .. }
                    | machine::StopReason::Deadline { .. },
                ) => {}
                Ok(machine::StopReason::Quiescent { .. }) | Ok(_) | Err(_) => {
                    survived = false;
                    break;
                }
            }
        }
        if self.machine.replay(current).is_err() {
            self.failed = true;
            return false;
        }
        survived
    }
}

impl NovaTarget<QuickNesMachine> {
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
}

impl<M: Machine> NovaTarget<M> {
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

impl<M: Machine> Target for NovaTarget<M> {
    type Action = ButtonChord;
    type Observations = NovaObservations;
    type Snapshot = NovaSnapshot<M::Portable>;

    fn reset(&mut self) {
        let mut handle_error = false;
        if self.current != self.genesis {
            if self.machine.drop_snapshot(self.current).is_err() {
                handle_error = true;
            }
            if !handle_error {
                self.current = self.genesis;
            }
        }
        let replay_error = self.machine.replay(self.genesis).is_err();
        self.failed = handle_error || replay_error;
        self.snapshot_base = None;
        self.current_wram = self.genesis_wram;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return;
        }
        let prior_wram = self.current_wram;
        let prior_state = self.observation.decoded;
        let start = self.current;
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            self.failed = true;
            return;
        }
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(
            run,
            Ok(machine::StopReason::Deadline { .. }
                | machine::StopReason::Quiescent { .. }
                | machine::StopReason::SnapshotPoint { .. })
        ) {
            self.failed = true;
            return;
        }

        let save_ram = match self
            .machine
            .read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)
        {
            Ok(save_ram) => save_ram,
            Err(_) => {
                self.failed = true;
                return;
            }
        };
        let frames = self.machine.frames();
        if frames.is_empty() {
            self.failed = true;
            return;
        }
        let mut prior_wram = prior_wram;
        let mut prior_state = prior_state;
        let mut emitted = false;
        for (offset, wram) in frames.iter().enumerate() {
            let Ok(state) = decode_state(wram, &save_ram) else {
                self.failed = true;
                return;
            };
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
                || state.level_reload_pending != prior_state.level_reload_pending;
            if boundary {
                let frame_count = self
                    .observation
                    .frame_count
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
                self.action_observations.push(self.make_observation(
                    frame_count,
                    state,
                    wram,
                    &prior_wram,
                ));
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
            || !self
                .action_observations
                .last()
                .is_some_and(|observation| observation.frame_count == endpoint_frame)
        {
            let endpoint_state = decode_state(&endpoint_wram, &save_ram);
            let Ok(endpoint_state) = endpoint_state else {
                self.failed = true;
                return;
            };
            self.action_observations.push(self.make_observation(
                endpoint_frame,
                endpoint_state,
                &endpoint_wram,
                &prior_wram,
            ));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
        let next = match self.machine.snapshot() {
            Ok(next) => next,
            Err(_) => {
                self.failed = true;
                return;
            }
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            self.failed = true;
            return;
        }
        self.current = next;
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
        if self.failed {
            return None;
        }
        let emulator_state = match self
            .machine
            .export(self.current, self.snapshot_base.as_ref())
        {
            Ok(state) => state,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        self.snapshot_base = Some(emulator_state.clone());
        Some(NovaSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            wram: self.current_wram.to_vec(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let restored_wram: [u8; WRAM_SIZE] = snapshot
            .wram
            .clone()
            .try_into()
            .map_err(|_| "Nova snapshot work RAM has an invalid length")?;
        let imported = self
            .machine
            .import(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.machine.replay(imported) {
            let _ = self.machine.drop_snapshot(imported);
            let _ = self.machine.replay(self.current);
            return Err(error.to_string().into());
        }
        if self.current != self.genesis
            && let Err(error) = self.machine.drop_snapshot(self.current)
        {
            let _ = self.machine.drop_snapshot(imported);
            let _ = self.machine.replay(self.current);
            return Err(error.to_string().into());
        }
        self.current = imported;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.current_wram = restored_wram;
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<M: Machine> Drop for NovaTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

fn read_memory<M: Machine>(machine: &M) -> Result<([u8; WRAM_SIZE], Vec<u8>), MachineError> {
    let wram = machine.read(0, WRAM_SIZE as u32)?;
    let wram = wram.try_into().map_err(|_| {
        MachineError::Backend("Nova work RAM window has an invalid length".to_owned())
    })?;
    let save_ram = machine.read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)?;
    if save_ram.len() != SAVE_RAM_SIZE {
        return Err(MachineError::Backend(
            "Nova save RAM window has an invalid length".to_owned(),
        ));
    }
    Ok((wram, save_ram))
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
    use std::{
        cell::Cell,
        collections::{BTreeMap, VecDeque},
    };

    use super::*;

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FakePortable(Vec<u8>);

    #[derive(Debug)]
    struct FakeMachine {
        state: Vec<u8>,
        snapshots: BTreeMap<u64, Vec<u8>>,
        next_snapshot: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
        vtime: u64,
        snapshot_calls: usize,
        drop_calls: usize,
        branch_calls: usize,
        replay_calls: usize,
        run_calls: usize,
        read_calls: Cell<usize>,
        export_calls: usize,
        export_base_calls: usize,
        import_calls: usize,
        run_stops: VecDeque<machine::StopReason>,
        max_chords_per_run: Option<usize>,
        append_sentinel: bool,
        zero_frames: bool,
        fail_next_drop: bool,
        lifecycle: Vec<&'static str>,
    }

    impl FakeMachine {
        fn new() -> Self {
            let mut state = vec![0_u8; SAVE_RAM_BASE + SAVE_RAM_SIZE];
            state[PLAYER_X_HIGH] = 1;
            state[PLAYER_Y_HIGH] = 1;
            state[PLAYER_HEALTH] = 4;
            state[LEVEL_NUMBER] = 1;
            state[STARTED_LEVEL_NUMBER] = 0;
            Self {
                state,
                snapshots: BTreeMap::new(),
                next_snapshot: 0,
                staged: Vec::new(),
                frames: Vec::new(),
                vtime: 0,
                snapshot_calls: 0,
                drop_calls: 0,
                branch_calls: 0,
                replay_calls: 0,
                run_calls: 0,
                read_calls: Cell::new(0),
                export_calls: 0,
                export_base_calls: 0,
                import_calls: 0,
                run_stops: VecDeque::new(),
                max_chords_per_run: None,
                append_sentinel: false,
                zero_frames: false,
                fail_next_drop: false,
                lifecycle: Vec::new(),
            }
        }

        fn insert_snapshot(&mut self, bytes: Vec<u8>) -> SnapId {
            let id = SnapId(self.next_snapshot);
            self.next_snapshot = self.next_snapshot.saturating_add(1);
            self.snapshots.insert(id.0, bytes);
            id
        }
    }

    impl Machine for FakeMachine {
        type Portable = FakePortable;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            self.lifecycle.push("snapshot");
            self.snapshot_calls = self.snapshot_calls.saturating_add(1);
            Ok(self.insert_snapshot(self.state.clone()))
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.lifecycle.push("drop");
            self.drop_calls = self.drop_calls.saturating_add(1);
            if self.fail_next_drop {
                self.fail_next_drop = false;
                return Err(MachineError::Backend("injected drop failure".to_owned()));
            }
            self.snapshots
                .remove(&snap.0)
                .map(|_| ())
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn branch(&mut self, snap: SnapId, env: &machine::Reproducer) -> Result<(), MachineError> {
            self.lifecycle.push("branch");
            self.branch_calls = self.branch_calls.saturating_add(1);
            self.state = self
                .snapshots
                .get(&snap.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged = nes::actions_of(env)?;
            if self.append_sentinel {
                self.staged.push(ButtonChord::new(0, 1));
            }
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.replay_calls = self.replay_calls.saturating_add(1);
            self.state = self
                .snapshots
                .get(&snap.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged.clear();
            Ok(())
        }

        fn run(
            &mut self,
            _until: StopConditions,
            _resolve: Option<&machine::Answer>,
        ) -> Result<machine::StopReason, MachineError> {
            self.lifecycle.push("run");
            self.run_calls = self.run_calls.saturating_add(1);
            self.frames.clear();
            let chord_count = self
                .max_chords_per_run
                .unwrap_or(self.staged.len())
                .min(self.staged.len());
            for action in self.staged.drain(..chord_count).collect::<Vec<_>>() {
                for _ in 0..action.bounded_hold_frames() {
                    self.state[0] = self.state[0].wrapping_add(1);
                    if !self.zero_frames {
                        let mut wram = [0_u8; WRAM_SIZE];
                        wram.copy_from_slice(
                            self.state
                                .get(..WRAM_SIZE)
                                .ok_or(MachineError::Backend("short fake state".to_owned()))?,
                        );
                        self.frames.push(wram);
                    }
                    self.vtime = self.vtime.saturating_add(1);
                }
            }
            Ok(self
                .run_stops
                .pop_front()
                .unwrap_or(machine::StopReason::Quiescent {
                    vtime: machine::Moment(self.vtime),
                }))
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            self.read_calls.set(self.read_calls.get().saturating_add(1));
            let end = addr
                .checked_add(u64::from(len))
                .ok_or(MachineError::ReadOutOfBounds)?;
            let (start, finish) = if addr == 0 && end == WRAM_SIZE as u64 {
                (0, WRAM_SIZE)
            } else if addr == SAVE_RAM_BASE as u64 && end == (SAVE_RAM_BASE + SAVE_RAM_SIZE) as u64
            {
                (SAVE_RAM_BASE, SAVE_RAM_BASE + SAVE_RAM_SIZE)
            } else {
                return Err(MachineError::ReadOutOfBounds);
            };
            self.state
                .get(start..finish)
                .map(ToOwned::to_owned)
                .ok_or(MachineError::ReadOutOfBounds)
        }

        fn export(
            &mut self,
            snap: SnapId,
            base: Option<&Self::Portable>,
        ) -> Result<Self::Portable, MachineError> {
            self.export_calls = self.export_calls.saturating_add(1);
            if base.is_some() {
                self.export_base_calls = self.export_base_calls.saturating_add(1);
            }
            self.snapshots
                .get(&snap.0)
                .cloned()
                .map(FakePortable)
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
            self.import_calls = self.import_calls.saturating_add(1);
            Ok(self.insert_snapshot(portable.0.clone()))
        }

        fn portable_memory_charge(portable: &Self::Portable) -> usize {
            portable.0.len()
        }

        fn now(&self) -> machine::Moment {
            machine::Moment(self.vtime)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    #[test]
    fn generic_action_runs_once_and_observes_returned_frames() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        let action = ButtonChord::new(0x81, 3);

        target.apply(&action);
        assert_eq!(target.machine.branch_calls, 1);
        assert_eq!(target.machine.run_calls, 1);
        assert_eq!(target.machine.snapshot_calls, 2);
        assert_eq!(target.machine.drop_calls, 0);
        assert_eq!(target.machine.read_calls.get(), 3);
        assert_eq!(target.observe().frame_count, 3);
        assert_eq!(target.observe().changed_indices, vec![0]);

        let before = (
            target.machine.branch_calls,
            target.machine.run_calls,
            target.machine.snapshot_calls,
            target.machine.drop_calls,
            target.machine.read_calls.get(),
        );
        target.apply(&action);
        assert_eq!(target.machine.branch_calls, before.0 + 1);
        assert_eq!(target.machine.run_calls, before.1 + 1);
        assert_eq!(target.machine.snapshot_calls, before.2 + 1);
        assert_eq!(target.machine.drop_calls, before.3 + 1);
        assert_eq!(target.observe().frame_count, 6);
        assert_eq!(target.machine.snapshots.len(), 2);

        let before_probe = (
            target.machine.snapshot_calls,
            target.machine.drop_calls,
            target.machine.branch_calls,
            target.machine.run_calls,
            target.machine.replay_calls,
            target.machine.snapshots.len(),
        );
        assert!(target.survives_probe(0, 2));
        assert_eq!(target.machine.snapshot_calls, before_probe.0);
        assert_eq!(target.machine.drop_calls, before_probe.1);
        assert_eq!(target.machine.branch_calls, before_probe.2 + 1);
        assert_eq!(target.machine.run_calls, before_probe.3 + 1);
        assert_eq!(target.machine.replay_calls, before_probe.4 + 1);
        assert_eq!(target.machine.snapshots.len(), before_probe.5);
    }

    #[test]
    fn generic_snapshot_restore_and_reset_keep_handles_bounded() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x01, 2));
        let snapshot = target.snapshot().expect("portable snapshot");
        assert_eq!(target.machine.export_base_calls, 0);
        let same = target.snapshot().expect("shared portable snapshot");
        assert_eq!(target.machine.export_calls, 2);
        assert_eq!(target.machine.export_base_calls, 1);
        assert_eq!(
            <FakeMachine as Machine>::portable_memory_charge(&snapshot.emulator_state),
            <FakeMachine as Machine>::portable_memory_charge(&same.emulator_state)
        );
        assert_eq!(target.machine.snapshots.len(), 2);

        target.apply(&ButtonChord::new(0x02, 2));
        assert_eq!(target.machine.snapshots.len(), 2);
        target
            .restore(&snapshot)
            .expect("restore portable snapshot");
        assert_eq!(target.machine.import_calls, 1);
        assert_eq!(target.machine.replay_calls, 1);
        assert_eq!(target.machine.snapshots.len(), 2);
        assert_eq!(target.observe().frame_count, 2);

        target.reset();
        assert_eq!(target.machine.snapshots.len(), 1);
        assert_eq!(target.machine.drop_calls, 3);
        assert_eq!(target.observe().frame_count, 0);
    }

    #[test]
    fn child_is_sealed_before_derive_parent_is_dropped() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x01, 2));
        target.machine.lifecycle.clear();

        target.apply(&ButtonChord::new(0x02, 2));

        assert_eq!(
            target.machine.lifecycle,
            ["branch", "run", "snapshot", "drop"]
        );
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.machine.snapshots.len(), 2);
    }

    #[test]
    fn snapshot_point_is_a_successful_action_and_probe_boundary() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target
            .machine
            .run_stops
            .push_back(machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(3),
            });
        target.apply(&ButtonChord::new(0x81, 3));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.observe().frame_count, 3);

        let mut probe = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        probe
            .machine
            .run_stops
            .push_back(machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(2),
            });
        assert!(probe.survives_probe(0, 2));
        assert_eq!(probe.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn zero_frame_action_and_probe_are_rejected() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.machine.zero_frames = true;
        target.apply(&ButtonChord::new(0x81, 3));
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert_eq!(target.machine.run_calls, 1);
        assert_eq!(target.machine.snapshots.len(), 1);

        target.reset();
        assert!(!target.survives_probe(0, 3));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.machine.run_calls, 2);
        assert_eq!(target.machine.snapshots.len(), 1);
    }

    #[test]
    fn probe_spans_chords_without_consuming_following_sentinel() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.machine.max_chords_per_run = Some(1);
        target.machine.append_sentinel = true;
        target.machine.run_stops.extend([
            machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(120),
            },
            machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(121),
            },
        ]);
        assert!(target.survives_probe(0, u16::from(MAX_HOLD_FRAMES) + 1));
        assert_eq!(target.machine.run_calls, 2);
        assert_eq!(target.machine.replay_calls, 1);
        assert_eq!(target.machine.state[0], 0);
        assert_eq!(target.machine.staged.len(), 0);
    }

    #[test]
    fn reset_replays_genesis_even_when_current_drop_fails() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x81, 2));
        let replay_calls = target.machine.replay_calls;
        target.machine.fail_next_drop = true;
        target.reset();
        assert_eq!(target.machine.replay_calls, replay_calls + 1);
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert_eq!(target.observe().frame_count, 0);
        assert_eq!(target.machine.snapshots.len(), 2);
    }

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
