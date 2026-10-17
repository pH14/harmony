// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::{io::Write, path::Path};

#[cfg(unix)]
use machine::quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine};
use machine::{Machine, MachineError, SnapId, StopConditions, nes};
use serde::{Deserialize, Serialize};

use crate::{
    famidash::levels::{self, Menu, Side, Stage},
    nes_backend::{NesBackend, NesMemory},
    target::{ExitKind, Target},
};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

pub type FamidashInput = crate::search::archive::Input<ButtonChord>;

pub const A_BUTTON: u8 = 0x01;
pub const RIGHT_BUTTON: u8 = 0x80;
pub const GAME_STATE_PLAYING: u8 = 2;
pub const GAME_STATE_COMPLETE: u8 = 3;
pub const GAME_STATE_LEVEL_SELECT: u8 = 6;
pub const LEVEL: usize = 0x047b;
pub const SCREEN_HEIGHT: i32 = 240;

const GAMEMODE: usize = 0x7a;
const CUBE_DATA: usize = 0x7b;
const DUAL: usize = 0x96;
const PLAYER_X: usize = 0x043d;
const PLAYER_Y: usize = 0x0441;
const PLAYER_VELOCITY_Y: usize = 0x0449;
const PLAYER_GRAVITY: usize = 0x044d;
const PLAYER_MINI: usize = 0x044f;
const GAME_STATE: usize = 0x0498;
const SCROLL_X: usize = 0x04a2;
const SCROLL_Y: usize = 0x04a6;
const SCROLL_Y_SUBPIXEL: usize = 0x04a8;
const SPEED: usize = 0x045e;
const PAUSED: usize = 0x05b5;
const DEATH: u8 = 0x01;
const DEATH_CHECK_SCREEN_X: u8 = 0x20;
const LEVEL_LENGTH_UNIT: i32 = 100;
const LEVEL_DONE_POLL_FRAMES: u32 = 15;
const LEVEL_DONE_POLLS: u32 = 40;
const MENU_SETTLE_FRAMES: u32 = 20;
const LEVEL_START_FRAMES: u32 = 40;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FamidashGameplayState {
    pub x: i32,
    pub y: i32,
    pub scroll_x: i32,
    pub camera_y: i32,
    pub camera_subpixel: u8,
    pub velocity_y: i16,
    pub gamemode: u8,
    pub gravity: bool,
    pub mini: bool,
    pub dual: bool,
    pub speed: u8,
}

impl FamidashGameplayState {
    #[must_use]
    pub fn vertical_motion(self) -> u8 {
        match self.velocity_y.signum() {
            -1 => 1,
            1 => 2,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FamidashMechanicalState {
    pub game_state: u8,
    pub level: u8,
    pub paused: bool,
    pub dead: bool,
    pub gameplay: Option<FamidashGameplayState>,
    pub side: Side,
    pub goal: u8,
}

impl FamidashMechanicalState {
    #[must_use]
    pub fn playing(self) -> bool {
        self.game_state == GAME_STATE_PLAYING
    }

    #[must_use]
    pub fn complete(self) -> bool {
        self.game_state == GAME_STATE_COMPLETE
    }

    #[must_use]
    pub fn finished(self) -> bool {
        self.complete() && self.level >= self.goal
    }

    #[must_use]
    pub fn advancing(self) -> bool {
        self.complete() && self.level < self.goal
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead || self.finished()
    }

    #[must_use]
    pub fn cleared(self) -> u8 {
        self.level.saturating_add(u8::from(self.complete()))
    }

    #[must_use]
    pub fn percent(self) -> u8 {
        if self.complete() {
            100
        } else {
            self.gameplay.map_or(0, |gameplay| {
                levels::percent(self.side, self.level, gameplay.scroll_x)
            })
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashObservations {
    pub frame_count: u64,
    pub decoded: FamidashMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: FamidashMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: FamidashObservations,
    pub failed: bool,
}

impl FamidashSnapshot {
    #[must_use]
    pub fn state(&self) -> FamidashMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct FamidashTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    stage: Stage,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: FamidashObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: FamidashObservations,
    action_observations: Vec<FamidashObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>> + NesMemory> FamidashTarget<M> {
    pub fn from_power_on(mut machine: M, stage: Stage) -> Result<Self, MachineError> {
        let stage =
            Stage::new(stage.side, stage.level, stage.goal).map_err(MachineError::Backend)?;
        let power_on = machine.snapshot()?;
        machine.branch(
            power_on,
            &nes::reproducer(&level_select_tape(stage.group().menu)),
        )?;
        run_setup(&mut machine)?;
        machine.drop_snapshot(power_on)?;
        if byte(&read_wram(&machine)?, LEVEL)? != stage.level {
            machine.poke_wram(LEVEL, stage.level)?;
        }
        let level_select = machine.snapshot()?;
        machine.branch(level_select, &nes::reproducer(&start_level_tape()))?;
        run_setup(&mut machine)?;
        machine.drop_snapshot(level_select)?;
        Self::from_machine(machine, stage)
    }
}

fn run_setup<M: Machine>(machine: &mut M) -> Result<(), MachineError> {
    if matches!(
        machine.run(StopConditions::default(), None)?,
        machine::StopReason::Quiescent { .. }
    ) {
        Ok(())
    } else {
        Err(MachineError::Backend(
            "Famidash setup did not quiesce".to_owned(),
        ))
    }
}

impl<M: NesBackend<Vec<u8>>> FamidashTarget<M> {
    pub fn from_machine(mut machine: M, stage: Stage) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram, stage)?;
        validate_genesis(state, stage.level)?;
        let genesis = machine.snapshot()?;
        let observation = FamidashObservations {
            frame_count: 0,
            decoded: state,
            terminal: false,
        };
        Ok(Self {
            machine,
            stage,
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

    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    #[must_use]
    pub fn mechanical_state(&self) -> FamidashMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.observation.decoded.terminal()
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        self.observation.decoded.finished()
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[FamidashObservations] {
        &self.action_observations
    }

    fn make_observation(frame_count: u64, state: FamidashMechanicalState) -> FamidashObservations {
        FamidashObservations {
            frame_count,
            decoded: state,
            terminal: state.terminal(),
        }
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    fn press(&mut self, chord: ButtonChord) -> Option<u64> {
        let start = self.machine.snapshot().ok()?;
        let branched = self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&chord)));
        let _ = self.machine.drop_snapshot(start);
        branched.ok()?;
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(
            run,
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        ) {
            return None;
        }
        let frames = u64::try_from(self.machine.frames().len()).ok()?;
        self.execution_work = self.execution_work.saturating_add(frames);
        (frames == u64::from(chord.bounded_hold_frames())).then_some(frames)
    }

    fn advance_level<F>(&mut self, level: u8, mut press: F) -> Option<u64>
    where
        F: FnMut(&mut Self, ButtonChord) -> Option<u64>,
    {
        let mut frames = 0_u64;
        let mut chords = |target: &mut Self, chords: &[ButtonChord]| -> Option<()> {
            for chord in chords {
                frames += press(target, *chord)?;
            }
            Some(())
        };
        let state =
            |target: &Self| decode_state(&read_wram(&target.machine).ok()?, target.stage).ok();
        for _ in 0..LEVEL_DONE_POLLS {
            if state(self)?.game_state == GAME_STATE_LEVEL_SELECT {
                break;
            }
            chords(self, &[ButtonChord::new(A_BUTTON, 1)])?;
            chords(self, &idle(LEVEL_DONE_POLL_FRAMES))?;
        }
        if state(self)?.game_state != GAME_STATE_LEVEL_SELECT {
            return None;
        }
        chords(self, &idle(MENU_SETTLE_FRAMES))?;
        chords(self, &[ButtonChord::new(RIGHT_BUTTON, 1)])?;
        chords(self, &idle(MENU_SETTLE_FRAMES))?;
        let next = level.checked_add(1)?;
        if state(self)?.level != next {
            return None;
        }
        chords(self, &[ButtonChord::new(A_BUTTON, 1)])?;
        chords(self, &idle(LEVEL_START_FRAMES))?;
        let arrived = state(self)?;
        (arrived.level == next && arrived.gameplay.is_some()).then_some(frames)
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
        let stage = self.stage;
        let stop_index = self.machine.frames().iter().position(|wram| {
            decode_state(wram, stage).is_ok_and(|state| state.terminal() || state.advancing())
        });
        let logical_frames = stop_index.map_or(produced_frames, |index| index.saturating_add(1));
        let endpoint_wram = self
            .machine
            .frames()
            .get(logical_frames.saturating_sub(1))
            .copied()
            .unwrap_or(initial_wram);
        let Ok(endpoint_state) = decode_state(&endpoint_wram, stage) else {
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
            let Ok(state) = decode_state(wram, stage) else {
                return self.fail();
            };
            if spatial_bucket(state) != spatial_bucket(prior_state)
                || boundary_fields(state) != boundary_fields(prior_state)
            {
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
        if let Some(observation) = emitted.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
        if let Some(stop_index) = stop_index {
            let Ok(stop_hold) = u8::try_from(stop_index.saturating_add(1)) else {
                return self.fail();
            };
            if self.machine.replay(start).is_err()
                || self
                    .machine
                    .branch(
                        start,
                        &nes::reproducer(std::slice::from_ref(&ButtonChord::new(
                            action.buttons,
                            stop_hold,
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
        if endpoint_state.advancing() {
            let Some(driven) = self.advance_level(endpoint_state.level, Self::press) else {
                return self.fail();
            };
            let Ok(wram) = read_wram(&self.machine) else {
                return self.fail();
            };
            let Ok(state) = decode_state(&wram, stage) else {
                return self.fail();
            };
            let observation = Self::make_observation(endpoint_frame.saturating_add(driven), state);
            emitted.push(observation.clone());
            self.observation = observation;
            self.current_wram = wram;
        }
        self.action_observations = emitted;
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
impl FamidashTarget<QuickNesMachine> {
    pub fn from_rom_bytes_headless_with_identity(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
        stage: Stage,
    ) -> Result<Self, MachineError> {
        Self::from_power_on(
            QuickNesMachine::from_rom_bytes_with_identity(
                rom,
                core_path,
                core_sha256,
                execution_sha256,
            )?,
            stage,
        )
    }

    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        stage: Stage,
    ) -> Result<Self, MachineError> {
        Self::from_power_on(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            stage,
        )
    }

    pub fn render_input(
        &mut self,
        input: &FamidashInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<FamidashVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Famidash gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            let mut skip = skip_frames;
            for action in &input.actions {
                self.render_action(
                    *action,
                    true,
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                let state = decode_state(&read_wram(&self.machine)?, self.stage)?;
                if state.advancing() {
                    self.advance_level(state.level, |target, chord| {
                        target
                            .render_action(
                                chord,
                                false,
                                video_output,
                                audio_output,
                                &mut metadata,
                                &mut skip,
                            )
                            .ok()?;
                        Some(u64::from(chord.bounded_hold_frames()))
                    })
                    .ok_or("Famidash film could not advance to the next level")?;
                }
            }
            let endpoint = decode_state(&read_wram(&self.machine)?, self.stage)?;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                self.render_action(
                    ButtonChord::new(0, u8::try_from(hold)?),
                    false,
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
        stop_at_advance: bool,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
        metadata: &mut Option<FamidashVideoMetadata>,
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
                    *metadata = Some(FamidashVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: FamidashMechanicalState::default(),
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
            if stop_at_advance && decode_state(&read_wram(&self.machine)?, self.stage)?.advancing()
            {
                break;
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

impl<M: NesBackend<Vec<u8>>> Target for FamidashTarget<M> {
    type Action = ButtonChord;
    type Observations = FamidashObservations;
    type Snapshot = FamidashSnapshot;

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
        let (x, y, mode) = state.gameplay.map_or((0, 0, 0), |gameplay| {
            (
                gameplay.x.div_euclid(32),
                gameplay.y.div_euclid(32),
                gameplay.gamemode,
            )
        });
        let x = u64::from(u32::from_ne_bytes(x.to_ne_bytes()));
        let y = u64::from(u32::from_ne_bytes(y.to_ne_bytes()));
        (u64::from(state.game_state) << 56)
            | (u64::from(state.level) << 48)
            | (u64::from(mode) << 40)
            | ((x & 0xff_ffff) << 16)
            | (y & 0xffff)
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
        Some(FamidashSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for FamidashTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

fn idle(mut frames: u32) -> Vec<ButtonChord> {
    let mut chords = Vec::new();
    while frames > 0 {
        let hold = frames.min(u32::from(MAX_HOLD_FRAMES));
        chords.push(ButtonChord::new(
            0,
            u8::try_from(hold).unwrap_or(MAX_HOLD_FRAMES),
        ));
        frames -= hold;
    }
    chords
}

fn wait(tape: &mut Vec<ButtonChord>, frames: u32) {
    tape.extend(idle(frames));
}

#[must_use]
pub fn level_select_tape(menu: Menu) -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    wait(&mut tape, 22);
    tape.push(ButtonChord::new(A_BUTTON, 1));
    wait(&mut tape, 936);
    if menu == Menu::CommunityLevels {
        tape.push(ButtonChord::new(RIGHT_BUTTON, 1));
        wait(&mut tape, MENU_SETTLE_FRAMES);
    }
    tape.push(ButtonChord::new(A_BUTTON, 1));
    wait(&mut tape, 90);
    tape
}

#[must_use]
pub fn start_level_tape() -> Vec<ButtonChord> {
    let mut tape = vec![ButtonChord::new(A_BUTTON, 1)];
    wait(&mut tape, LEVEL_START_FRAMES);
    tape
}

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = level_select_tape(Menu::MainLevels);
    tape.extend(start_level_tape());
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Famidash RAM address {address:#x} is absent"))
    })
}

fn word(wram: &[u8], address: usize) -> Result<u16, MachineError> {
    Ok(u16::from_le_bytes([
        byte(wram, address)?,
        byte(wram, address + 1)?,
    ]))
}

fn long(wram: &[u8], address: usize) -> Result<i32, MachineError> {
    Ok(i32::from_le_bytes([
        byte(wram, address)?,
        byte(wram, address + 1)?,
        byte(wram, address + 2)?,
        byte(wram, address + 3)?,
    ]))
}

pub fn decode_state(wram: &[u8], stage: Stage) -> Result<FamidashMechanicalState, MachineError> {
    let game_state = byte(wram, GAME_STATE)?;
    let level = byte(wram, LEVEL)?;
    let paused = byte(wram, PAUSED)? != 0;
    let playing = game_state == GAME_STATE_PLAYING;
    let scroll_x = long(wram, SCROLL_X)?;
    let past_end = stage
        .side
        .level_length(level)
        .is_some_and(|length| scroll_x > i32::from(length) * LEVEL_LENGTH_UNIT);
    let dead = playing
        && (past_end
            || byte(wram, PLAYER_X + 1)? > DEATH_CHECK_SCREEN_X
                && (byte(wram, CUBE_DATA)? & DEATH != 0
                    || byte(wram, CUBE_DATA + 1)? & DEATH != 0));
    let gameplay = if playing && !dead && !paused {
        let [fine_y, screen_y] = word(wram, SCROLL_Y)?.to_le_bytes();
        let camera_y = i32::from(screen_y) * SCREEN_HEIGHT + i32::from(fine_y);
        Some(FamidashGameplayState {
            x: scroll_x.saturating_add(i32::from(byte(wram, PLAYER_X + 1)?)),
            y: camera_y + i32::from(byte(wram, PLAYER_Y + 1)?),
            scroll_x,
            camera_y,
            camera_subpixel: byte(wram, SCROLL_Y_SUBPIXEL)?,
            velocity_y: i16::from_le_bytes(word(wram, PLAYER_VELOCITY_Y)?.to_le_bytes()),
            gamemode: byte(wram, GAMEMODE)?,
            gravity: byte(wram, PLAYER_GRAVITY)? != 0,
            mini: byte(wram, PLAYER_MINI)? != 0,
            dual: byte(wram, DUAL)? != 0,
            speed: byte(wram, SPEED)?,
        })
    } else {
        None
    };
    Ok(FamidashMechanicalState {
        game_state,
        level,
        paused,
        dead,
        gameplay,
        side: stage.side,
        goal: stage.goal,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Famidash system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: FamidashMechanicalState, level: u8) -> Result<(), MachineError> {
    if state.gameplay.is_some() && state.level == level && state.percent() == 0 {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Famidash setup did not reach level {level} gameplay: {state:?}"
        )))
    }
}

#[must_use]
pub fn spatial_bucket(state: FamidashMechanicalState) -> Option<(i32, i32)> {
    state
        .gameplay
        .map(|gameplay| (gameplay.x.div_euclid(16), gameplay.y.div_euclid(16)))
}

#[must_use]
pub fn boundary_fields(state: FamidashMechanicalState) -> (u8, bool, u8, bool, bool, bool) {
    let gameplay = state.gameplay.unwrap_or_default();
    (
        state.game_state,
        state.dead,
        gameplay.gamemode,
        gameplay.gravity,
        gameplay.mini,
        gameplay.dual,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main() -> Stage {
        Stage::through_group(Side::Main, 0).unwrap()
    }

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_STATE] = GAME_STATE_PLAYING;
        wram[PLAYER_X + 1] = 80;
        wram[PLAYER_Y + 1] = 178;
        wram[SCROLL_Y] = 0xef;
        wram[SCROLL_Y + 1] = 2;
        wram
    }

    #[test]
    fn world_position_adds_the_scroll_to_the_screen_position() {
        let mut wram = playing_wram();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&1_000_i32.to_le_bytes());
        let gameplay = decode_state(&wram, main()).unwrap().gameplay.unwrap();
        assert_eq!(gameplay.x, 1_080);
        assert_eq!(gameplay.camera_y, 2 * 240 + 0xef);
        assert_eq!(gameplay.y, 2 * 240 + 0xef + 178);
    }

    #[test]
    fn a_negative_scroll_before_the_level_moves_is_signed() {
        let mut wram = playing_wram();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&(-240_i32).to_le_bytes());
        let state = decode_state(&wram, main()).unwrap();
        assert_eq!(state.gameplay.unwrap().x, -160);
        assert_eq!(state.percent(), 0);
    }

    #[test]
    fn either_player_death_bit_is_a_terminal_death() {
        for player in 0..2 {
            let mut wram = playing_wram();
            wram[CUBE_DATA + player] = DEATH;
            let state = decode_state(&wram, main()).unwrap();
            assert!(state.dead);
            assert!(state.terminal());
            assert!(state.gameplay.is_none());
        }
    }

    #[test]
    fn a_death_bit_left_of_the_death_check_column_is_not_a_death() {
        let mut wram = playing_wram();
        wram[PLAYER_X + 1] = DEATH_CHECK_SCREEN_X;
        wram[CUBE_DATA] = DEATH;
        let state = decode_state(&wram, main()).unwrap();
        assert!(!state.dead);
        assert!(!state.terminal());
        assert!(state.gameplay.is_some());
        wram[PLAYER_X + 1] = DEATH_CHECK_SCREEN_X + 1;
        assert!(decode_state(&wram, main()).unwrap().dead);
    }

    #[test]
    fn only_the_last_level_complete_state_is_terminal() {
        let mut wram = playing_wram();
        wram[GAME_STATE] = GAME_STATE_COMPLETE;
        wram[CUBE_DATA] = DEATH;
        let state = decode_state(&wram, main()).unwrap();
        assert!(state.complete());
        assert!(!state.dead);
        assert!(state.advancing());
        assert!(!state.terminal());
        assert_eq!(state.percent(), 100);
        assert_eq!(state.cleared(), 1);
        wram[LEVEL] = 21;
        let state = decode_state(&wram, main()).unwrap();
        assert!(state.finished());
        assert!(!state.advancing());
        assert!(state.terminal());
        assert_eq!(state.cleared(), 22);
        wram[LEVEL] = 30;
        let single = Stage::single(Side::B, 30).unwrap();
        let state = decode_state(&wram, single).unwrap();
        assert!(state.finished());
        assert_eq!(state.side, Side::B);
    }

    #[test]
    fn the_percent_uses_the_side_level_length() {
        let mut wram = playing_wram();
        wram[LEVEL] = 23;
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&(732_i32 * 40).to_le_bytes());
        let stage = Stage::single(Side::C, 23).unwrap();
        assert_eq!(decode_state(&wram, stage).unwrap().percent(), 40);
        let stage = Stage::single(Side::Main, 23).unwrap();
        assert!(decode_state(&wram, stage).unwrap().dead);
    }

    #[test]
    fn a_scroll_past_the_level_length_is_a_dead_end() {
        let mut wram = playing_wram();
        wram[LEVEL] = 17;
        let stage = Stage::single(Side::C, 17).unwrap();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&15_400_i32.to_le_bytes());
        let state = decode_state(&wram, stage).unwrap();
        assert!(!state.dead);
        assert_eq!(state.percent(), 99);
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&15_401_i32.to_le_bytes());
        let state = decode_state(&wram, stage).unwrap();
        assert!(state.dead);
        assert!(state.terminal());
        assert!(state.gameplay.is_none());
    }

    #[test]
    fn a_paused_level_has_no_gameplay() {
        let mut wram = playing_wram();
        wram[PAUSED] = 1;
        let state = decode_state(&wram, main()).unwrap();
        assert!(state.gameplay.is_none());
        assert!(!state.terminal());
    }

    #[test]
    fn genesis_requires_gameplay_in_the_selected_level() {
        assert!(validate_genesis(decode_state(&playing_wram(), main()).unwrap(), 0).is_ok());
        let mut wram = playing_wram();
        wram[LEVEL] = 1;
        assert!(validate_genesis(decode_state(&wram, main()).unwrap(), 0).is_err());
        assert!(validate_genesis(decode_state(&wram, main()).unwrap(), 1).is_ok());
        wram[LEVEL] = 0;
        wram[GAME_STATE] = GAME_STATE_LEVEL_SELECT;
        assert!(validate_genesis(decode_state(&wram, main()).unwrap(), 0).is_err());
    }

    #[test]
    fn setup_presses_only_a() {
        let tape = setup_tape();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 3);
        assert!(tape.iter().all(|chord| chord.buttons & !A_BUTTON == 0));
        let frames: u32 = tape
            .iter()
            .map(|chord| u32::from(chord.bounded_hold_frames()))
            .sum();
        assert_eq!(frames, 1_091);
        assert!(tape.starts_with(&level_select_tape(Menu::MainLevels)));
    }

    #[test]
    fn the_community_menu_is_one_right_press_from_the_main_menu() {
        let main = level_select_tape(Menu::MainLevels);
        let community = level_select_tape(Menu::CommunityLevels);
        let pressed: Vec<u8> = community
            .iter()
            .filter(|chord| chord.buttons != 0)
            .map(|chord| chord.buttons)
            .collect();
        assert_eq!(pressed, [A_BUTTON, RIGHT_BUTTON, A_BUTTON]);
        let frames = |tape: &[ButtonChord]| -> u32 {
            tape.iter()
                .map(|chord| u32::from(chord.bounded_hold_frames()))
                .sum()
        };
        assert_eq!(frames(&community), frames(&main) + 1 + MENU_SETTLE_FRAMES);
    }
}
