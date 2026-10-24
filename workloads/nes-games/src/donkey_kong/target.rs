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

pub type DonkeyKongInput = crate::search::archive::Input<ButtonChord>;

pub const STAGES_PER_ROUND: u8 = 3;
pub const GOAL_ROUNDS: u8 = 8;
pub const TRACKED_STAGES: usize = GOAL_ROUNDS as usize * STAGES_PER_ROUND as usize;
pub const STAGE_BARRELS: u8 = 1;
pub const STAGE_ELEVATORS: u8 = 3;
pub const STAGE_RIVETS: u8 = 4;
pub const STAGE_ORDER: [u8; STAGES_PER_ROUND as usize] =
    [STAGE_BARRELS, STAGE_ELEVATORS, STAGE_RIVETS];
pub const RIVETS: u8 = 8;
pub const STATUS_DEAD: u8 = 0xff;
pub const START_X: u8 = 0x35;
pub const START_Y: u8 = 0xcc;
pub const START_LIVES: u8 = 2;
pub const START_BONUS: u8 = 0x49;
pub const FLOOR_TOP: u8 = 7;

const BONUS: usize = 0x2e;
const STAGE: usize = 0x53;
const ROUND: usize = 0x54;
const LIVES: usize = 0x55;
const DEMO: usize = 0x58;
const FLOOR: usize = 0x59;
const GROUNDED: usize = 0x5a;
const MARIO_X: usize = 0x46;
const MARIO_Y: usize = 0x47;
const STATUS: usize = 0x96;
const FACING: usize = 0x202;
const SCORE: usize = 0x20;
const RIVET_FLAGS: usize = 0xc1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DonkeyKongMechanicalState {
    pub stage: u8,
    pub round: u8,
    pub lives: u8,
    pub demo: u8,
    pub floor: u8,
    pub grounded: u8,
    pub bonus: u8,
    pub status: u8,
    pub facing: u8,
    pub x: u8,
    pub y: u8,
    pub score: u32,
    pub rivets: u8,
}

impl DonkeyKongMechanicalState {
    #[must_use]
    pub fn stage_index(self) -> u8 {
        match self.stage {
            STAGE_RIVETS => 2,
            STAGE_ELEVATORS => 1,
            _ => 0,
        }
    }

    #[must_use]
    pub fn room(self) -> u8 {
        self.stage_index() + STAGES_PER_ROUND * self.round.min(1)
    }

    #[must_use]
    pub fn stages_cleared(self) -> u8 {
        self.round
            .saturating_mul(STAGES_PER_ROUND)
            .saturating_add(self.stage_index())
    }

    #[must_use]
    pub fn height(self) -> u8 {
        if self.y == 0 {
            0
        } else {
            START_Y.saturating_sub(self.y)
        }
    }

    #[must_use]
    pub fn in_play(self) -> bool {
        self.demo == 0 && self.y != 0 && self.status != 0 && self.status != STATUS_DEAD
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.status == STATUS_DEAD || self.demo != 0
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.round >= GOAL_ROUNDS
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }

    #[must_use]
    pub fn in_transition(self) -> bool {
        !self.in_play()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongObservations {
    pub frame_count: u64,
    pub decoded: DonkeyKongMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: DonkeyKongMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: DonkeyKongObservations,
    pub failed: bool,
}

impl DonkeyKongSnapshot {
    #[must_use]
    pub fn state(&self) -> DonkeyKongMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct DonkeyKongTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: DonkeyKongObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: DonkeyKongObservations,
    action_observations: Vec<DonkeyKongObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> DonkeyKongTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = DonkeyKongObservations {
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
                "DonkeyKong setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> DonkeyKongMechanicalState {
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
    pub fn last_action_observations(&self) -> &[DonkeyKongObservations] {
        &self.action_observations
    }

    pub fn snapshot_of_state(
        &mut self,
        state: &Vec<u8>,
        frame_count: u64,
    ) -> Result<DonkeyKongSnapshot, Box<dyn Error>> {
        let imported = self
            .machine
            .import_nes(state)
            .map_err(|error| error.to_string())?;
        let wram = match self
            .machine
            .replay(imported)
            .and_then(|()| read_wram(&self.machine))
        {
            Ok(wram) => wram,
            Err(error) => {
                let _ = self.machine.drop_snapshot(imported);
                return Err(error.to_string().into());
            }
        };
        let decoded = match decode_state(&wram) {
            Ok(decoded) => decoded,
            Err(error) => {
                let _ = self.machine.drop_snapshot(imported);
                return Err(error.to_string().into());
            }
        };
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        self.current = imported;
        self.snapshot_base = Some(state.clone());
        self.failed = false;
        self.current_wram = wram;
        self.observation = Self::make_observation(frame_count, decoded);
        self.action_observations = vec![self.observation.clone()];
        self.snapshot()
            .ok_or_else(|| "failed to snapshot the restored state".into())
    }

    fn make_observation(
        frame_count: u64,
        state: DonkeyKongMechanicalState,
    ) -> DonkeyKongObservations {
        DonkeyKongObservations {
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
            .position(|wram| decode_state(wram).is_ok_and(DonkeyKongMechanicalState::terminal));
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
impl DonkeyKongTarget<QuickNesMachine> {
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
        input: &DonkeyKongInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<DonkeyKongVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore DonkeyKong gameplay genesis for film".into());
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
        metadata: &mut Option<DonkeyKongVideoMetadata>,
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
                    *metadata = Some(DonkeyKongVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: DonkeyKongMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for DonkeyKongTarget<M> {
    type Action = ButtonChord;
    type Observations = DonkeyKongObservations;
    type Snapshot = DonkeyKongSnapshot;

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
        (u64::from(state.stage) << 56)
            | (u64::from(state.round) << 48)
            | (u64::from(state.x) << 40)
            | (u64::from(state.y) << 32)
            | (u64::from(state.status) << 24)
            | (u64::from(state.lives) << 16)
            | (u64::from(state.rivets) << 8)
            | u64::from(state.floor)
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
        Some(DonkeyKongSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for DonkeyKongTarget<M> {
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
    vec![
        ButtonChord::new(0, 100),
        ButtonChord::new(START, 3),
        ButtonChord::new(0, 120),
        ButtonChord::new(0, 120),
        ButtonChord::new(0, 120),
        ButtonChord::new(0, 120),
        ButtonChord::new(0, 60),
    ]
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Donkey Kong RAM address {address:#x} is absent"))
    })
}

fn bcd(value: u8) -> u32 {
    u32::from(value >> 4) * 10 + u32::from(value & 0x0f)
}

fn score_of(wram: &[u8]) -> Result<u32, MachineError> {
    Ok(bcd(byte(wram, SCORE)?)
        + bcd(byte(wram, SCORE + 2)?) * 100
        + bcd(byte(wram, SCORE + 1)?) * 10_000)
}

pub fn decode_state(wram: &[u8]) -> Result<DonkeyKongMechanicalState, MachineError> {
    let stage = byte(wram, STAGE)?;
    let mut rivets = 0u8;
    if stage == STAGE_RIVETS {
        for slot in 0..usize::from(RIVETS) {
            if byte(wram, RIVET_FLAGS + slot)? != 0 {
                rivets += 1;
            }
        }
    }
    Ok(DonkeyKongMechanicalState {
        stage,
        round: byte(wram, ROUND)?,
        lives: byte(wram, LIVES)?,
        demo: byte(wram, DEMO)?,
        floor: byte(wram, FLOOR)?,
        grounded: byte(wram, GROUNDED)?,
        bonus: byte(wram, BONUS)?,
        status: byte(wram, STATUS)?,
        facing: byte(wram, FACING)?,
        x: byte(wram, MARIO_X)?,
        y: byte(wram, MARIO_Y)?,
        score: score_of(wram)?,
        rivets,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Donkey Kong system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: DonkeyKongMechanicalState) -> Result<(), MachineError> {
    let valid = state.in_play()
        && state.stage == STAGE_BARRELS
        && state.round == 0
        && state.lives == START_LIVES
        && state.bonus == START_BONUS
        && state.score == 0
        && (state.x, state.y) == (START_X, START_Y);
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Donkey Kong setup did not reach the first barrel stage: {state:?}"
        )))
    }
}

pub const GRID_SHIFT: u8 = 4;

#[must_use]
pub fn boundary_fields(state: DonkeyKongMechanicalState) -> [u16; 9] {
    [
        u16::from(state.stage),
        u16::from(state.round),
        u16::from(state.lives),
        u16::from(state.status),
        u16::from(state.floor),
        u16::from(state.x >> GRID_SHIFT),
        u16::from(state.y >> GRID_SHIFT),
        u16::from(state.rivets),
        u16::try_from(state.score).unwrap_or(u16::MAX),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[STAGE] = STAGE_BARRELS;
        wram[LIVES] = START_LIVES;
        wram[BONUS] = START_BONUS;
        wram[FLOOR] = 1;
        wram[STATUS] = 1;
        wram[MARIO_X] = START_X;
        wram[MARIO_Y] = START_Y;
        wram
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = DonkeyKongSnapshot {
            emulator_state,
            observation: DonkeyKongObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<DonkeyKongSnapshot>() + snapshot.emulator_state.capacity();
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
    fn the_decoder_reads_stage_round_lives_position_and_score() {
        let mut wram = playing_wram();
        wram[SCORE + 2] = 0x13;
        wram[SCORE + 1] = 0x02;
        wram[ROUND] = 1;
        wram[STAGE] = STAGE_ELEVATORS;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.score, 21_300);
        assert_eq!((state.x, state.y), (START_X, START_Y));
        assert_eq!(state.stages_cleared(), 4);
        assert_eq!(state.lives, START_LIVES);
    }

    #[test]
    fn rivets_count_only_on_the_rivet_stage() {
        let mut wram = playing_wram();
        wram[RIVET_FLAGS] = 1;
        wram[RIVET_FLAGS + 5] = 2;
        assert_eq!(decode_state(&wram).unwrap().rivets, 0);
        wram[STAGE] = STAGE_RIVETS;
        assert_eq!(decode_state(&wram).unwrap().rivets, 2);
    }

    #[test]
    fn genesis_requires_the_first_barrel_stage_at_the_start_position() {
        assert!(validate_genesis(decode_state(&playing_wram()).unwrap()).is_ok());
        let mut wram = playing_wram();
        wram[MARIO_X] = 0x40;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[STATUS] = 0;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[STAGE] = STAGE_ELEVATORS;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn a_dead_mario_the_attract_demo_and_the_goal_round_are_terminal() {
        let mut wram = playing_wram();
        assert!(!decode_state(&wram).unwrap().terminal());
        wram[STATUS] = STATUS_DEAD;
        let dead = decode_state(&wram).unwrap();
        assert!(dead.dead() && dead.terminal() && !dead.victory());
        let mut wram = playing_wram();
        wram[DEMO] = 1;
        assert!(decode_state(&wram).unwrap().terminal());
        let mut wram = playing_wram();
        wram[ROUND] = 1;
        wram[STAGE] = STAGE_BARRELS;
        let later = decode_state(&wram).unwrap();
        assert!(!later.victory() && !later.terminal());
        wram[ROUND] = GOAL_ROUNDS;
        let won = decode_state(&wram).unwrap();
        assert!(won.victory() && won.terminal() && !won.dead());
    }

    #[test]
    fn the_intro_has_no_mario_and_counts_as_a_transition() {
        let mut wram = playing_wram();
        wram[MARIO_Y] = 0;
        wram[STATUS] = 0;
        let intro = decode_state(&wram).unwrap();
        assert!(intro.in_transition() && !intro.terminal());
        assert!(!decode_state(&playing_wram()).unwrap().in_transition());
    }

    #[test]
    fn stages_cleared_counts_three_stages_per_round() {
        let mut wram = playing_wram();
        let cleared = |wram: &[u8]| decode_state(wram).unwrap().stages_cleared();
        assert_eq!(cleared(&wram), 0);
        wram[STAGE] = STAGE_ELEVATORS;
        assert_eq!(cleared(&wram), 1);
        wram[STAGE] = STAGE_RIVETS;
        assert_eq!(cleared(&wram), 2);
        wram[ROUND] = 2;
        wram[STAGE] = STAGE_BARRELS;
        assert_eq!(cleared(&wram), 6);
    }

    #[test]
    fn later_rounds_use_their_own_three_rooms() {
        let mut wram = playing_wram();
        let room = |wram: &[u8]| decode_state(wram).unwrap().room();
        assert_eq!(room(&wram), 0);
        wram[STAGE] = STAGE_RIVETS;
        assert_eq!(room(&wram), 2);
        wram[ROUND] = 1;
        wram[STAGE] = STAGE_BARRELS;
        assert_eq!(room(&wram), 3);
        wram[ROUND] = 5;
        wram[STAGE] = STAGE_RIVETS;
        assert_eq!(room(&wram), 5);
    }

    #[test]
    fn height_grows_as_mario_climbs() {
        let mut wram = playing_wram();
        assert_eq!(decode_state(&wram).unwrap().height(), 0);
        wram[MARIO_Y] = 0x40;
        assert_eq!(decode_state(&wram).unwrap().height(), 0x8c);
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

impl searcher::search::snapshot_codec::CheckpointSnapshot for DonkeyKongSnapshot {
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
        let snapshot = DonkeyKongSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: DonkeyKongObservations {
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
            SnapshotCheckpoint::<DonkeyKongSnapshot>::from_bytes(&bytes, &checkpoint.format)
                .unwrap(),
            checkpoint
        );
        let old = postcard::to_allocvec(&(
            "donkey_kong-quicknes-snapshot-checkpoint-v0",
            &checkpoint.entries,
        ))
        .unwrap();
        assert!(
            SnapshotCheckpoint::<DonkeyKongSnapshot>::from_bytes(&old, &checkpoint.format).is_err()
        );
    }
}
