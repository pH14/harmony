// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::{io::Write, path::Path};

#[cfg(unix)]
use machine::quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine};
use machine::{Machine, MachineError, SnapId, StopConditions, nes};
use serde::{Deserialize, Serialize};

use crate::{
    nes_backend::NesBackend,
    target::{ExitKind, Target},
};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

pub type PacManInput = crate::search::archive::Input<ButtonChord>;

pub const MAZES: u8 = 32;
pub const DOTS: u8 = 192;
pub const START_LIVES: u8 = 3;
pub const MAZES_PER_LEVEL: u8 = 4;
pub const LEVELS: u8 = 6;

pub const MODE_TITLE: u8 = 0x00;
pub const MODE_READY: u8 = 0x02;
pub const MODE_PLAY: u8 = 0x04;
pub const MODE_GHOST_EATEN: u8 = 0x06;
pub const MODE_DEATH: u8 = 0x08;
pub const MODE_GAME_OVER: u8 = 0x0a;
pub const MODE_MAZE_CLEAR: u8 = 0x0c;

pub const UP: u8 = 0x10;
pub const DOWN: u8 = 0x20;
pub const LEFT: u8 = 0x40;
pub const RIGHT: u8 = 0x80;

const PAC_X: usize = 0x1a;
const PAC_Y: usize = 0x1c;
const MODE: usize = 0x3f;
const PLAYERS: usize = 0x47;
const DEMO: usize = 0x48;
const DIRECTION: usize = 0x51;
const LIVES: usize = 0x67;
const LEVEL: usize = 0x68;
const DOTS_LEFT: usize = 0x6a;
const SCORE: usize = 0x70;
const SCORE_DIGITS: usize = 6;
const POWER: usize = 0x88;
const POWER_SECONDS: usize = 0x89;
const POWER_FRAMES: usize = 0x8a;
const PAUSED: usize = 0x60f;

const LEVEL_UNSET: u8 = 0xff;
const FRAMES_PER_SECOND: u16 = 60;
const SCREEN_LEFT: i32 = 12;
const SCREEN_TOP: i32 = 4;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PacManMechanicalState {
    pub mode: u8,
    pub demo: u8,
    pub lives: u8,
    pub level: u8,
    pub dots: u8,
    pub score: u32,
    pub x: u8,
    pub y: u8,
    pub direction: u8,
    pub power: u8,
    pub power_frames: u16,
    pub paused: bool,
}

impl PacManMechanicalState {
    #[must_use]
    pub fn mazes_cleared(self) -> u8 {
        if self.level == LEVEL_UNSET {
            return 0;
        }
        self.level
            .saturating_add(u8::from(self.dots == 0))
            .min(MAZES)
    }

    #[must_use]
    pub fn dots_eaten(self) -> u8 {
        DOTS.saturating_sub(self.dots)
    }

    #[must_use]
    pub fn in_game(self) -> bool {
        self.demo == 0 && self.level != LEVEL_UNSET
    }

    #[must_use]
    pub fn on_maze(self) -> bool {
        self.in_game() && matches!(self.mode, MODE_PLAY | MODE_GHOST_EATEN | MODE_DEATH)
    }

    #[must_use]
    pub fn playing(self) -> bool {
        self.mode == MODE_PLAY && self.in_game() && !self.paused && self.dots != 0
    }

    #[must_use]
    pub fn dying(self) -> bool {
        self.mode == MODE_DEATH && self.in_game()
    }

    #[must_use]
    pub fn game_over(self) -> bool {
        self.mode == MODE_GAME_OVER
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.game_over() || self.demo != 0
    }

    #[must_use]
    pub fn empowered(self) -> bool {
        self.power != 0
    }

    #[must_use]
    pub fn cell(self) -> (u8, u8) {
        (self.x >> 3, self.y >> 3)
    }

    #[must_use]
    pub fn screen(self) -> (u16, u16) {
        let x = (i32::from(self.x) - SCREEN_LEFT).clamp(0, 255);
        let y = (i32::from(self.y) - SCREEN_TOP).clamp(0, 223);
        (
            u16::try_from(x).unwrap_or_default(),
            u16::try_from(y).unwrap_or_default(),
        )
    }

    #[must_use]
    pub fn pose(self) -> u16 {
        let column = match self.direction & 3 {
            0 => 2,
            1 => 1,
            2 => 3,
            _ => 0,
        };
        let (x, y) = self.screen();
        column << 1 | ((x + y) >> 2 & 1)
    }

    #[must_use]
    pub fn level_group(self) -> u8 {
        (self.mazes_cleared().min(MAZES.saturating_sub(1)) / MAZES_PER_LEVEL).min(LEVELS - 1)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManObservations {
    pub frame_count: u64,
    pub decoded: PacManMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: PacManMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: PacManObservations,
    pub failed: bool,
}

impl PacManSnapshot {
    #[must_use]
    pub fn state(&self) -> PacManMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct PacManTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: PacManObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: PacManObservations,
    action_observations: Vec<PacManObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> PacManTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = PacManObservations {
            frame_count: 0,
            decoded: state,
            terminal: false,
        };
        Ok(Self {
            machine,
            genesis,
            current: genesis,
            genesis_observation: observation.clone(),
            genesis_wram: wram,
            current_wram: wram,
            observation: observation.clone(),
            action_observations: vec![observation],
            failed: false,
            snapshot_base: None,
            execution_work: 0,
        })
    }

    pub fn from_power_on(mut machine: M) -> Result<Self, MachineError> {
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&setup_tape()))?;
        let run = machine.run(StopConditions::default(), None)?;
        if !matches!(run, machine::StopReason::Quiescent { .. }) {
            return Err(MachineError::Backend(
                "Pac-Man setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> PacManMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.observation.decoded.terminal()
    }

    #[must_use]
    pub fn won(&self) -> bool {
        false
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[PacManObservations] {
        &self.action_observations
    }

    pub fn snapshot_of_state(
        &mut self,
        state: &[u8],
        frame_count: u64,
    ) -> Result<PacManSnapshot, Box<dyn Error>> {
        let probe = PacManSnapshot {
            emulator_state: state.to_owned(),
            observation: self.genesis_observation.clone(),
            failed: false,
        };
        self.restore(&probe)?;
        let decoded = decode_state(&self.current_wram)?;
        self.observation = Self::make_observation(frame_count, decoded);
        self.action_observations = vec![self.observation.clone()];
        self.snapshot()
            .ok_or_else(|| "failed to snapshot the restored state".into())
    }

    fn make_observation(frame_count: u64, state: PacManMechanicalState) -> PacManObservations {
        PacManObservations {
            frame_count,
            decoded: state,
            terminal: state.terminal(),
        }
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    fn apply_internal(&mut self, action: &ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.is_terminal() {
            return;
        }
        let initial_wram = self.current_wram;
        let initial_state = self.observation.decoded;
        let start = self.current;
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            return self.fail();
        }
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(
            run,
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        ) {
            return self.fail();
        }
        let produced_frames = self.machine.frames().len();
        if produced_frames == 0 {
            return self.fail();
        }
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(produced_frames).unwrap_or(u64::MAX));
        let terminal_index = self
            .machine
            .frames()
            .iter()
            .position(|wram| decode_state(wram).is_ok_and(PacManMechanicalState::terminal));
        let logical_frames =
            terminal_index.map_or(produced_frames, |index| index.saturating_add(1));
        let endpoint_wram = self
            .machine
            .frames()
            .get(logical_frames.saturating_sub(1))
            .copied()
            .unwrap_or(initial_wram);
        let Ok(endpoint_state) = decode_state(&endpoint_wram) else {
            return self.fail();
        };
        let mut prior_state = initial_state;
        let mut emitted = Vec::new();
        for (offset, wram) in self
            .machine
            .frames()
            .iter()
            .take(logical_frames)
            .enumerate()
        {
            let Ok(state) = decode_state(wram) else {
                return self.fail();
            };
            if boundary_fields(state) != boundary_fields(prior_state) {
                let frame_count = self
                    .observation
                    .frame_count
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
                emitted.push(Self::make_observation(frame_count, state));
                prior_state = state;
            }
        }
        let endpoint_frame = self
            .observation
            .frame_count
            .saturating_add(u64::try_from(logical_frames).unwrap_or(u64::MAX));
        if emitted
            .last()
            .is_none_or(|observation| observation.frame_count != endpoint_frame)
        {
            emitted.push(Self::make_observation(endpoint_frame, endpoint_state));
        }
        self.action_observations = emitted;
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
        if let Some(terminal_index) = terminal_index {
            let Ok(terminal_hold) = u8::try_from(terminal_index.saturating_add(1)) else {
                return self.fail();
            };
            if self.machine.replay(start).is_err()
                || self
                    .machine
                    .branch(
                        start,
                        &nes::reproducer(std::slice::from_ref(&ButtonChord::new(
                            action.buttons,
                            terminal_hold,
                        ))),
                    )
                    .is_err()
            {
                return self.fail();
            }
            let replay = self.machine.run(StopConditions::default(), None);
            let replayed_wram = read_wram(&self.machine);
            if !matches!(
                replay,
                Ok(machine::StopReason::Quiescent { .. }
                    | machine::StopReason::SnapshotPoint { .. })
            ) || self.machine.frames().len() != logical_frames
                || replayed_wram.as_ref().ok() != Some(&endpoint_wram)
            {
                return self.fail();
            }
        }
        let Ok(next) = self.machine.snapshot() else {
            return self.fail();
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            return self.fail();
        }
        self.current = next;
    }
}

#[cfg(unix)]
impl PacManTarget<QuickNesMachine> {
    pub fn from_rom_bytes_headless_with_identity(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_power_on(QuickNesMachine::from_rom_bytes_with_identity(
            rom,
            core_path,
            core_sha256,
            execution_sha256,
        )?)
    }

    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_power_on(QuickNesMachine::from_rom_bytes(
            rom,
            core_path,
            core_sha256,
        )?)
    }

    pub fn render_input(
        &mut self,
        input: &PacManInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<PacManVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Pac-Man gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            let mut skip = skip_frames;
            for action in &input.actions {
                self.render_action(
                    *action,
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
            }
            let endpoint = decode_state(&read_wram(&self.machine)?)?;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                self.render_action(
                    ButtonChord::new(0, u8::try_from(hold)?),
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                remaining -= hold;
            }
            let mut metadata = metadata.ok_or("QuickNES produced no video frames")?;
            if metadata.audio_frames == 0 {
                return Err("QuickNES produced no audio samples".into());
            }
            metadata.input_endpoint = endpoint;
            metadata.skipped_frames = skip_frames - skip;
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
        metadata: &mut Option<PacManVideoMetadata>,
        skip: &mut u64,
    ) -> Result<(), Box<dyn Error>> {
        let start = self.machine.snapshot()?;
        self.machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))?;
        self.machine.drop_snapshot(start)?;
        for _ in 0..action.bounded_hold_frames() {
            if !self.run_one_frame()? {
                break;
            }
            let frame = self
                .machine
                .take_video_frame()
                .ok_or("QuickNES omitted a requested video frame")?;
            if *skip > 0 {
                *skip -= 1;
                self.machine.take_audio_samples();
                continue;
            }
            match metadata {
                Some(existing)
                    if (existing.width, existing.height) != (frame.width, frame.height) =>
                {
                    return Err("QuickNES video geometry changed during replay".into());
                }
                Some(existing) => existing.frames = existing.frames.saturating_add(1),
                None => {
                    *metadata = Some(PacManVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: PacManMechanicalState::default(),
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
            if let Some(existing) = metadata {
                existing.audio_frames = existing.audio_frames.saturating_add(u64::try_from(
                    audio.len() / usize::from(QUICKNES_AUDIO_CHANNELS),
                )?);
            }
        }
        Ok(())
    }

    fn run_one_frame(&mut self) -> Result<bool, Box<dyn Error>> {
        let deadline = machine::Moment(self.machine.now().0.saturating_add(1));
        match self.machine.run(
            StopConditions {
                deadline: Some(deadline),
                on: machine::StopMask::NONE,
            },
            None,
        )? {
            machine::StopReason::Deadline { .. } => Ok(true),
            machine::StopReason::Quiescent { .. } => Ok(false),
            _ => Err("QuickNES film stopped unexpectedly".into()),
        }
    }
}

impl<M: NesBackend<Vec<u8>>> Target for PacManTarget<M> {
    type Action = ButtonChord;
    type Observations = PacManObservations;
    type Snapshot = PacManSnapshot;

    fn reset(&mut self) {
        let mut failed = false;
        if self.current != self.genesis {
            if self.machine.drop_snapshot(self.current).is_err() {
                failed = true;
            } else {
                self.current = self.genesis;
            }
        }
        if self.machine.replay(self.genesis).is_err() {
            failed = true;
        }
        self.failed = failed;
        self.snapshot_base = None;
        self.current_wram = self.genesis_wram;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.apply_internal(action);
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        (u64::from(state.mode) << 56)
            | (u64::from(state.level) << 48)
            | (u64::from(state.lives) << 44)
            | (u64::from(state.dots) << 36)
            | (u64::from(state.x) << 28)
            | (u64::from(state.y) << 20)
            | (u64::from(state.direction) << 18)
            | (u64::from(state.power != 0) << 17)
            | u64::from(state.score & 0x1ffff)
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
        let Ok(emulator_state) = self
            .machine
            .export_nes(self.current, self.snapshot_base.as_ref())
        else {
            self.failed = true;
            return None;
        };
        self.snapshot_base = Some(emulator_state.clone());
        Some(PacManSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let imported = self
            .machine
            .import_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        let restored_wram = match self
            .machine
            .replay(imported)
            .and_then(|()| read_wram(&self.machine))
        {
            Ok(wram) => wram,
            Err(error) => {
                self.failed = true;
                let _ = self.machine.drop_snapshot(imported);
                return Err(error.to_string().into());
            }
        };
        if self.current != self.genesis
            && let Err(error) = self.machine.drop_snapshot(self.current)
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(imported);
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

impl<M: NesBackend<Vec<u8>>> Drop for PacManTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

const START: u8 = 0x08;

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    wait(&mut tape, 300);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, 100);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, GENESIS_WAIT_FRAMES);
    tape
}

const GENESIS_WAIT_FRAMES: usize = 252;

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Pac-Man RAM address {address:#x} is absent")))
}

pub fn decode_state(wram: &[u8]) -> Result<PacManMechanicalState, MachineError> {
    let mut score = 0u32;
    for digit in (0..SCORE_DIGITS).rev() {
        score = score * 10 + u32::from(byte(wram, SCORE + digit)?.min(9));
    }
    let power = byte(wram, POWER)?;
    let seconds = byte(wram, POWER_SECONDS)?;
    let power_frames = if power == 0 || seconds >= 0x80 {
        0
    } else {
        u16::from(seconds) * FRAMES_PER_SECOND
            + FRAMES_PER_SECOND.saturating_sub(u16::from(byte(wram, POWER_FRAMES)?))
    };
    let players = byte(wram, PLAYERS)?;
    let _ = players;
    Ok(PacManMechanicalState {
        mode: byte(wram, MODE)?,
        demo: byte(wram, DEMO)?,
        lives: byte(wram, LIVES)?,
        level: byte(wram, LEVEL)?,
        dots: byte(wram, DOTS_LEFT)?,
        score: score * 10,
        x: byte(wram, PAC_X)?,
        y: byte(wram, PAC_Y)?,
        direction: byte(wram, DIRECTION)? & 3,
        power,
        power_frames,
        paused: byte(wram, PAUSED)? != 0,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Pac-Man system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: PacManMechanicalState) -> Result<(), MachineError> {
    let valid = state.mode == MODE_PLAY
        && state.in_game()
        && state.lives == START_LIVES
        && state.level == 0
        && state.dots >= DOTS - 1
        && state.score <= 10
        && state.power == 0
        && !state.paused;
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Pac-Man setup did not reach the first maze: {state:?}"
        )))
    }
}

pub const GRID_SHIFT: u8 = 3;

#[must_use]
pub fn boundary_fields(state: PacManMechanicalState) -> [u16; 10] {
    [
        u16::from(state.mode),
        u16::from(state.demo),
        u16::from(state.lives),
        u16::from(state.level),
        u16::from(state.dots),
        u16::from(state.x >> GRID_SHIFT),
        u16::from(state.y >> GRID_SHIFT),
        u16::from(state.direction),
        u16::from(state.power != 0),
        u16::from(state.paused),
    ]
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for PacManSnapshot {
    type Codec = searcher::search::snapshot_codec::PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.reserve(std::mem::size_of::<Self>())?;
        budget.array::<u8>(self.emulator_state.capacity())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[MODE] = MODE_PLAY;
        wram[LIVES] = START_LIVES;
        wram[DOTS_LEFT] = DOTS;
        wram[PAC_X] = 95;
        wram[PAC_Y] = 168;
        wram[DIRECTION] = 1;
        wram
    }

    #[test]
    fn the_decoder_reads_the_score_digits_and_the_power_timer() {
        let mut wram = playing_wram();
        wram[SCORE..SCORE + SCORE_DIGITS].copy_from_slice(&[3, 2, 1, 0, 0, 0]);
        wram[POWER] = 1;
        wram[POWER_SECONDS] = 4;
        wram[POWER_FRAMES] = 50;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.score, 1230);
        assert_eq!(state.power_frames, 4 * 60 + 10);
        assert!(state.empowered());
        wram[POWER] = 0;
        assert_eq!(decode_state(&wram).unwrap().power_frames, 0);
    }

    #[test]
    fn the_screen_position_is_the_top_left_of_the_sprite() {
        let state = decode_state(&playing_wram()).unwrap();
        assert_eq!(state.screen(), (83, 164));
        let mut corner = playing_wram();
        corner[PAC_X] = 3;
        corner[PAC_Y] = 1;
        assert_eq!(decode_state(&corner).unwrap().screen(), (0, 0));
    }

    #[test]
    fn a_cleared_maze_counts_once_before_and_after_the_level_counter_moves() {
        let mut wram = playing_wram();
        wram[DOTS_LEFT] = 0;
        wram[MODE] = MODE_MAZE_CLEAR;
        assert_eq!(decode_state(&wram).unwrap().mazes_cleared(), 1);
        wram[LEVEL] = 1;
        wram[DOTS_LEFT] = DOTS;
        wram[MODE] = MODE_READY;
        assert_eq!(decode_state(&wram).unwrap().mazes_cleared(), 1);
        wram[LEVEL] = LEVEL_UNSET;
        wram[DOTS_LEFT] = 0;
        assert_eq!(decode_state(&wram).unwrap().mazes_cleared(), 0);
    }

    #[test]
    fn mazes_group_four_to_a_level() {
        let mut state = decode_state(&playing_wram()).unwrap();
        assert_eq!(state.level_group(), 0);
        state.level = 3;
        state.dots = 0;
        assert_eq!(state.level_group(), 1);
        state.level = 200;
        assert_eq!(state.level_group(), LEVELS - 1);
    }

    #[test]
    fn genesis_requires_the_first_maze_in_play_with_three_lives() {
        let state = decode_state(&playing_wram()).unwrap();
        assert!(validate_genesis(state).is_ok());
        for spoil in [
            |s: &mut PacManMechanicalState| s.mode = MODE_READY,
            |s: &mut PacManMechanicalState| s.lives = 2,
            |s: &mut PacManMechanicalState| s.level = 1,
            |s: &mut PacManMechanicalState| s.demo = 1,
            |s: &mut PacManMechanicalState| s.dots = 100,
        ] {
            let mut spoiled = state;
            spoil(&mut spoiled);
            assert!(validate_genesis(spoiled).is_err());
        }
    }

    #[test]
    fn only_game_over_and_the_attract_demo_are_terminal() {
        let mut wram = playing_wram();
        assert!(!decode_state(&wram).unwrap().terminal());
        wram[MODE] = MODE_DEATH;
        assert!(!decode_state(&wram).unwrap().terminal());
        wram[MODE] = MODE_GAME_OVER;
        assert!(decode_state(&wram).unwrap().terminal());
        wram[MODE] = MODE_PLAY;
        wram[DEMO] = 1;
        assert!(decode_state(&wram).unwrap().terminal());
    }

    #[test]
    fn a_new_tile_or_a_new_life_is_a_boundary_but_a_pixel_step_is_not() {
        let mut state = decode_state(&playing_wram()).unwrap();
        state.x = 96;
        let mut step = state;
        step.x += 1;
        assert_eq!(boundary_fields(state), boundary_fields(step));
        step.x += 8;
        assert_ne!(boundary_fields(state), boundary_fields(step));
        let mut hurt = state;
        hurt.lives -= 1;
        assert_ne!(boundary_fields(state), boundary_fields(hurt));
    }

    #[test]
    fn poses_cover_four_headings_and_two_mouth_phases() {
        let mut state = decode_state(&playing_wram()).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for direction in 0..4 {
            state.direction = direction;
            for x in [0, 4] {
                state.x = 20 + x;
                seen.insert(state.pose());
            }
        }
        assert_eq!(seen.len(), 8);
        assert!(seen.iter().all(|pose| *pose < 8));
    }

    #[test]
    fn setup_tape_only_taps_start() {
        let tape = setup_tape();
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            2
        );
    }
}
