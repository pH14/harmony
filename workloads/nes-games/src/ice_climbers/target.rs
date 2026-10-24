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

pub type IceClimbersInput = crate::search::archive::Input<ButtonChord>;

pub const MOUNTAINS: u8 = 32;
pub const FLOORS: u8 = 8;
pub const FLOOR_HEIGHT: u16 = 48;
pub const START_LIVES: u8 = 3;
pub const SCREEN_CROP: u16 = 8;
pub const MAP_PAD: u16 = 194;
pub const BONUS_FLOOR: u8 = 9;
pub const START_X: u8 = 0x38;
pub const START_Y: u8 = 0xc0;
pub const START_CAMERA: u8 = 0x9f;
pub const START_TOP: u16 = 444;
pub const CAMERA_WRAP: u16 = 240;
pub const FLOOR_SCROLL: u16 = 48;
pub const BONUS_BASE: u16 = 384;
pub const WORLD_BOTTOM: u16 = START_TOP + START_Y as u16;

const LIVES: usize = 0x20;
const CAMERA: usize = 0x13;
const LEVEL: usize = 0x59;
const PHASE: usize = 0x5a;
const PLAYER_X: usize = 0x64;
const PLAYER_Y: usize = 0x66;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IceClimbersMechanicalState {
    pub level: u8,
    pub lives: u8,
    pub phase: u8,
    pub x: u8,
    pub y: u8,
    pub camera: u8,
}

impl IceClimbersMechanicalState {
    #[must_use]
    pub fn bonus(self) -> bool {
        self.phase >= BONUS_FLOOR
    }

    #[must_use]
    pub fn scrolled(self) -> u16 {
        let base = (u16::from(START_CAMERA) + CAMERA_WRAP - u16::from(self.camera)) % CAMERA_WRAP;
        if self.phase >= BONUS_FLOOR - 1 && base < FLOOR_SCROLL {
            base + CAMERA_WRAP
        } else {
            base
        }
    }

    #[must_use]
    pub fn world_y(self) -> u16 {
        (START_TOP + u16::from(self.y)).saturating_sub(self.scrolled())
    }

    #[must_use]
    pub fn map_y(self) -> u16 {
        if self.bonus() {
            u16::from(self.y).saturating_sub(SCREEN_CROP - 1)
        } else {
            (self.world_y() + MAP_PAD).saturating_sub(SCREEN_CROP - 1)
        }
    }

    #[must_use]
    pub fn altitude(self) -> u16 {
        if self.bonus() {
            BONUS_BASE + 255 - u16::from(self.y)
        } else {
            WORLD_BOTTOM.saturating_sub(self.world_y())
        }
    }

    #[must_use]
    pub fn floor(self) -> u8 {
        u8::try_from(self.altitude() / FLOOR_HEIGHT)
            .unwrap_or(u8::MAX)
            .min(FLOORS)
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.lives < START_LIVES
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.level >= MOUNTAINS
    }

    #[must_use]
    pub fn in_transition(self) -> bool {
        self.phase == 0
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersObservations {
    pub frame_count: u64,
    pub decoded: IceClimbersMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: IceClimbersMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: IceClimbersObservations,
    pub failed: bool,
}

impl IceClimbersSnapshot {
    #[must_use]
    pub fn state(&self) -> IceClimbersMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct IceClimbersTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: IceClimbersObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: IceClimbersObservations,
    action_observations: Vec<IceClimbersObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> IceClimbersTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = IceClimbersObservations {
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
                "IceClimbers setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> IceClimbersMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.observation.decoded.terminal()
    }

    #[must_use]
    pub fn won(&self) -> bool {
        self.observation.decoded.victory()
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[IceClimbersObservations] {
        &self.action_observations
    }

    fn make_observation(
        frame_count: u64,
        state: IceClimbersMechanicalState,
    ) -> IceClimbersObservations {
        IceClimbersObservations {
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
        let terminal_index =
            self.machine.frames().iter().position(|wram| {
                decode_state(wram).is_ok_and(IceClimbersMechanicalState::terminal)
            });
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
impl IceClimbersTarget<QuickNesMachine> {
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
        input: &IceClimbersInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<IceClimbersVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore IceClimbers gameplay genesis for film".into());
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
        metadata: &mut Option<IceClimbersVideoMetadata>,
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
                    *metadata = Some(IceClimbersVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: IceClimbersMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for IceClimbersTarget<M> {
    type Action = ButtonChord;
    type Observations = IceClimbersObservations;
    type Snapshot = IceClimbersSnapshot;

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
        (u64::from(state.level) << 56)
            | (u64::from(state.lives) << 48)
            | (u64::from(state.phase) << 40)
            | (u64::from(state.x) << 32)
            | (u64::from(state.y) << 24)
            | u64::from(state.camera)
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
        Some(IceClimbersSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for IceClimbersTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

const START: u8 = 0x08;
const WAIT_FRAMES: usize = 780;

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    wait(&mut tape, 120);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, WAIT_FRAMES);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Ice Climbers RAM address {address:#x} is absent"))
    })
}

pub fn decode_state(wram: &[u8]) -> Result<IceClimbersMechanicalState, MachineError> {
    Ok(IceClimbersMechanicalState {
        level: byte(wram, LEVEL)?,
        lives: byte(wram, LIVES)?,
        phase: byte(wram, PHASE)?,
        x: byte(wram, PLAYER_X)?,
        y: byte(wram, PLAYER_Y)?,
        camera: byte(wram, CAMERA)?,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Ice Climbers system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: IceClimbersMechanicalState) -> Result<(), MachineError> {
    let valid = state.level == 0
        && state.lives == START_LIVES
        && state.x == START_X
        && state.y == START_Y
        && state.camera == START_CAMERA;
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Ice Climbers setup did not reach the first mountain: {state:?}"
        )))
    }
}

pub const GRID_SHIFT: u8 = 5;

#[must_use]
pub fn boundary_fields(state: IceClimbersMechanicalState) -> [u16; 6] {
    [
        u16::from(state.level),
        u16::from(state.lives),
        u16::from(state.phase),
        state.altitude() / 16,
        u16::from(state.x >> GRID_SHIFT),
        u16::from(state.floor()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[LIVES] = START_LIVES;
        wram[PHASE] = 1;
        wram[PLAYER_X] = START_X;
        wram[PLAYER_Y] = START_Y;
        wram[CAMERA] = START_CAMERA;
        wram
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = IceClimbersSnapshot {
            emulator_state,
            observation: IceClimbersObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<IceClimbersSnapshot>() + snapshot.emulator_state.capacity();
        let mut budget = ImageBudget::new(exact);
        snapshot.checkpoint_image_charge(&mut budget).unwrap();
        assert_eq!(budget.remaining(), 0);
        assert!(
            snapshot
                .checkpoint_image_charge(&mut ImageBudget::new(exact - 1))
                .is_err()
        );
        let frozen = snapshot.freeze_checkpoint().unwrap();
        let expected = postcard::to_allocvec(&snapshot).unwrap();
        snapshot.emulator_state[0] = 99;
        snapshot.observation.frame_count += 1;
        assert_eq!(postcard::to_allocvec(&frozen).unwrap(), expected);
        assert_ne!(frozen, snapshot);
    }

    #[test]
    fn the_start_position_is_altitude_zero_and_each_floor_is_48_pixels() {
        let start = decode_state(&start_wram()).unwrap();
        assert_eq!(start.altitude(), 0);
        assert_eq!(start.floor(), 0);
        let mut wram = start_wram();
        wram[PLAYER_Y] = START_Y - 48;
        assert_eq!(decode_state(&wram).unwrap().floor(), 1);
        wram[PLAYER_Y] = START_Y - 47;
        assert_eq!(decode_state(&wram).unwrap().floor(), 0);
        wram[PHASE] = 7;
        wram[CAMERA] = 207;
        wram[PLAYER_Y] = 0x60;
        assert_eq!(decode_state(&wram).unwrap().altitude(), 288);
        wram[PHASE] = 8;
        wram[CAMERA] = START_CAMERA;
        assert_eq!(decode_state(&wram).unwrap().altitude(), 336);
        wram[PHASE] = BONUS_FLOOR;
        wram[PLAYER_Y] = 0x31;
        let bonus = decode_state(&wram).unwrap();
        assert!(bonus.altitude() > 384 && bonus.floor() == 8);
    }

    #[test]
    fn genesis_requires_the_first_mountain_with_three_lives() {
        assert!(validate_genesis(decode_state(&start_wram()).unwrap()).is_ok());
        let mut wram = start_wram();
        wram[LIVES] = 2;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = start_wram();
        wram[PLAYER_X] = 0x40;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn a_lost_life_is_terminal_and_the_last_mountain_is_victory() {
        let mut wram = start_wram();
        assert!(!decode_state(&wram).unwrap().terminal());
        wram[LIVES] = 2;
        let dead = decode_state(&wram).unwrap();
        assert!(dead.dead() && dead.terminal() && !dead.victory());
        wram[LIVES] = START_LIVES;
        wram[LEVEL] = MOUNTAINS;
        assert!(decode_state(&wram).unwrap().victory());
    }

    #[test]
    fn setup_tape_presses_only_start() {
        let tape = setup_tape();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            1
        );
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for IceClimbersSnapshot {
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
mod checkpoint_tests {
    use super::*;
    use searcher::search::campaign::{SnapshotCheckpoint, SnapshotCheckpointEntry};

    #[test]
    fn framed_snapshot_round_trip_refuses_a_flat_format() {
        let snapshot = IceClimbersSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: IceClimbersObservations {
                frame_count: 17,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let checkpoint = SnapshotCheckpoint {
            format: super::super::campaign::SNAPSHOT_CHECKPOINT_FORMAT.into(),
            entries: vec![SnapshotCheckpointEntry { id: 7, snapshot }],
        };
        let bytes = checkpoint.to_bytes().unwrap();
        assert_eq!(
            SnapshotCheckpoint::<IceClimbersSnapshot>::from_bytes(&bytes, &checkpoint.format)
                .unwrap(),
            checkpoint
        );
        let old = postcard::to_allocvec(&(
            "ice-climbers-quicknes-snapshot-checkpoint-v0",
            &checkpoint.entries,
        ))
        .unwrap();
        assert!(
            SnapshotCheckpoint::<IceClimbersSnapshot>::from_bytes(&old, &checkpoint.format)
                .is_err()
        );
    }
}
