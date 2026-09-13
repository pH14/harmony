// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

pub type ThwaiteInput = crate::search::archive::Input<ButtonChord>;

pub const NUM_BUILDINGS: usize = 12;
pub const BUILDING_SILO0: usize = 2;
pub const BUILDING_SILO1: usize = 9;
pub const HOURS_PER_DAY: u8 = 5;
pub const NUM_MADE_DAYS: u8 = 7;
pub const INITIAL_SILO_MISSILES: u8 = 15;

pub const STATE_INACTIVE: u8 = 0;
pub const STATE_NEW_LEVEL: u8 = 1;
pub const STATE_ACTIVE: u8 = 2;
pub const STATE_LEVEL_REWARD: u8 = 3;
pub const STATE_GAMEOVER: u8 = 7;

const GAME_STATE: usize = 0x38;
const NUM_PLAYERS: usize = 0x39;
const IS_PRACTICE: usize = 0x4f;
const ENEMY_MISSILES_LEFT: usize = 0x305;
const HOUSES_STANDING: usize = 0x3cc;
const BUILDINGS_DESTROYED_THIS_LEVEL: usize = 0x3d8;
const SCORE_100S: usize = 0x3d9;
const SCORE_1S: usize = 0x3da;
const GAME_DAY: usize = 0x3dd;
const GAME_HOUR: usize = 0x3de;
const GAME_MINUTE: usize = 0x3df;
const GAME_SECOND: usize = 0x3e0;
const SILO_MISSILES_LEFT: usize = 0x3e6;
const MISSILE_Y_HI: usize = 0x412;
const CROSSHAIR_X_HI: usize = 0x4e0;
const CROSSHAIR_Y_HI: usize = 0x4e4;
const NUM_MISSILES: usize = 20;
const FIRST_ENEMY_MISSILE: usize = 4;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ThwaiteTownState {
    pub buildings: [u8; NUM_BUILDINGS],
    pub buildings_standing: u8,
    pub silos_standing: u8,
    pub silo_missiles: [u8; 2],
    pub enemy_missiles_left: u8,
    pub enemy_missiles_in_flight: u8,
    pub buildings_destroyed_this_level: u8,
    pub crosshair_x: u8,
    pub crosshair_y: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ThwaiteMechanicalState {
    pub game_state: u8,
    pub num_players: u8,
    pub practice: bool,
    pub score: u16,
    pub game_day: u8,
    pub game_hour: u8,
    pub game_minute: u8,
    pub game_second: u8,
    pub town: Option<ThwaiteTownState>,
}

impl ThwaiteMechanicalState {
    #[must_use]
    pub fn level_index(self) -> u8 {
        self.game_day
            .saturating_mul(HOURS_PER_DAY)
            .saturating_add(self.game_hour)
    }

    #[must_use]
    pub fn game_over(self) -> bool {
        self.game_state == STATE_GAMEOVER
    }

    #[must_use]
    pub fn abandoned(self) -> bool {
        self.game_state == STATE_INACTIVE
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.game_over() || self.abandoned()
    }

    #[must_use]
    pub fn survived_all_days(self) -> bool {
        self.abandoned() && self.game_day >= NUM_MADE_DAYS
    }

    #[must_use]
    pub fn town_valid(self) -> bool {
        self.town.is_some()
    }

    #[must_use]
    pub fn buildings_standing(self) -> u8 {
        self.town.map_or(0, |town| town.buildings_standing)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteDefenceEvidence {
    pub buildings_lost: u16,
    pub levels_cleared: u16,
    pub perfect_levels: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteObservations {
    pub frame_count: u64,
    pub decoded: ThwaiteMechanicalState,
    pub changed_indices: Vec<u16>,
    #[serde(default)]
    pub evidence: ThwaiteDefenceEvidence,
    #[serde(default)]
    pub terminal: bool,
    pub log_line: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub input_endpoint: ThwaiteMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteSnapshot<P = machine::SharedState> {
    pub(crate) emulator_state: P,
    pub(crate) observation: ThwaiteObservations,
    pub(crate) wram: Vec<u8>,
    pub(crate) failed: bool,
    #[serde(default)]
    pub(crate) evidence: ThwaiteDefenceEvidence,
}

impl<P> ThwaiteSnapshot<P> {
    #[must_use]
    pub fn state(&self) -> ThwaiteMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct ThwaiteTarget<M: Machine = QuickNesMachine> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: ThwaiteObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: ThwaiteObservations,
    action_observations: Vec<ThwaiteObservations>,
    evidence: ThwaiteDefenceEvidence,
    failed: bool,
    snapshot_base: Option<M::Portable>,
    execution_work: u64,
}

impl<M: Machine> ThwaiteTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = ThwaiteObservations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            evidence: ThwaiteDefenceEvidence::default(),
            terminal: false,
            log_line: "frame=0 changed=[]".to_owned(),
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
            evidence: ThwaiteDefenceEvidence::default(),
            failed: false,
            snapshot_base: None,
            execution_work: 0,
        })
    }

    #[must_use]
    pub fn mechanical_state(&self) -> ThwaiteMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn defence_evidence(&self) -> ThwaiteDefenceEvidence {
        self.observation.evidence
    }

    #[must_use]
    pub fn is_game_over(&self) -> bool {
        self.observation.decoded.terminal()
    }

    #[must_use]
    pub fn defended_a_perfect_hour(&self) -> bool {
        self.observation.evidence.perfect_levels > 0
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[ThwaiteObservations] {
        &self.action_observations
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_game_over() || frames == 0 {
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
        let result = (|| {
            self.machine.branch(current, &nes::reproducer(&actions))?;
            self.machine.run(StopConditions::default(), None)
        })();
        let survived = if matches!(result, Ok(machine::StopReason::Quiescent { .. })) {
            match read_wram(&self.machine).and_then(|wram| decode_state(&wram)) {
                Ok(state) => !state.terminal(),
                Err(_) => false,
            }
        } else {
            false
        };
        if self.machine.replay(current).is_err()
            || read_wram(&self.machine).as_ref().ok() != Some(&self.current_wram)
        {
            self.failed = true;
            return false;
        }
        survived
    }
}

impl ThwaiteTarget<QuickNesMachine> {
    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&setup_tape()))?;
        let run = machine.run(StopConditions::default(), None)?;
        if !matches!(run, machine::StopReason::Quiescent { .. }) {
            return Err(MachineError::Backend(
                "Thwaite setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    pub fn render_input(
        &mut self,
        input: &ThwaiteInput,
        tail_frames: u32,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<ThwaiteVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Thwaite gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            for action in &input.actions {
                self.render_action(*action, video_output, audio_output, &mut metadata)?;
                if decode_state(&read_wram(&self.machine)?)?.terminal() {
                    break;
                }
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
                )?;
                remaining -= hold;
            }
            let mut metadata = metadata.ok_or("QuickNES produced no video frames")?;
            if metadata.audio_frames == 0 {
                return Err("QuickNES produced no audio samples".into());
            }
            metadata.input_endpoint = endpoint;
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
        metadata: &mut Option<ThwaiteVideoMetadata>,
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
            match metadata {
                Some(existing)
                    if (existing.width, existing.height) != (frame.width, frame.height) =>
                {
                    return Err("QuickNES video geometry changed during replay".into());
                }
                Some(existing) => existing.frames = existing.frames.saturating_add(1),
                None => {
                    *metadata = Some(ThwaiteVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        input_endpoint: ThwaiteMechanicalState::default(),
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

impl<M: Machine> ThwaiteTarget<M> {
    fn make_observation(
        &self,
        frame_count: u64,
        state: ThwaiteMechanicalState,
        wram: &[u8; WRAM_SIZE],
        prior_wram: &[u8; WRAM_SIZE],
        evidence: ThwaiteDefenceEvidence,
    ) -> ThwaiteObservations {
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
        ThwaiteObservations {
            frame_count,
            decoded: state,
            changed_indices: changed_indices.clone(),
            evidence,
            terminal: state.terminal(),
            log_line: format!(
                "frame={frame_count} changed={changed_indices:?} standing={} lost={} cleared={} perfect={}",
                state.buildings_standing(),
                evidence.buildings_lost,
                evidence.levels_cleared,
                evidence.perfect_levels,
            ),
        }
    }

    fn apply_internal(&mut self, action: &ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.is_game_over() {
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
            self.failed = true;
            return;
        }
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(
            run,
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        ) {
            self.failed = true;
            return;
        }
        let produced_frames = self.machine.frames().len();
        if produced_frames == 0 {
            self.failed = true;
            return;
        }
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(produced_frames).unwrap_or(u64::MAX));
        let terminal_index = self
            .machine
            .frames()
            .iter()
            .position(|wram| decode_state(wram).is_ok_and(ThwaiteMechanicalState::terminal));
        let logical_frames =
            terminal_index.map_or(produced_frames, |index| index.saturating_add(1));
        let (endpoint_wram, endpoint_state, emitted, prior_wram, evidence) = {
            let frames = self.machine.frames();
            let endpoint_wram = frames
                .get(logical_frames.saturating_sub(1))
                .copied()
                .unwrap_or(initial_wram);
            let endpoint_state = match decode_state(&endpoint_wram) {
                Ok(state) => state,
                Err(_) => {
                    self.failed = true;
                    return;
                }
            };
            let mut prior_wram = initial_wram;
            let mut prior_frame_state = initial_state;
            let mut prior_emitted_state = initial_state;
            let mut emitted = false;
            let mut evidence = self.evidence;
            for (offset, wram) in frames.iter().take(logical_frames).enumerate() {
                let Ok(state) = decode_state(wram) else {
                    self.failed = true;
                    return;
                };
                accumulate_evidence(&mut evidence, prior_frame_state, state);
                prior_frame_state = state;
                let boundary = defence_bucket(state) != defence_bucket(prior_emitted_state)
                    || state.game_state != prior_emitted_state.game_state
                    || state.score != prior_emitted_state.score;
                if boundary {
                    let frame_count = self.observation.frame_count.saturating_add(
                        u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1),
                    );
                    self.action_observations.push(self.make_observation(
                        frame_count,
                        state,
                        wram,
                        &prior_wram,
                        evidence,
                    ));
                    prior_wram = *wram;
                    prior_emitted_state = state;
                    emitted = true;
                }
            }
            (endpoint_wram, endpoint_state, emitted, prior_wram, evidence)
        };
        let endpoint_frame = self
            .observation
            .frame_count
            .saturating_add(u64::try_from(logical_frames).unwrap_or(u64::MAX));
        if !emitted
            || !self
                .action_observations
                .last()
                .is_some_and(|observation| observation.frame_count == endpoint_frame)
        {
            self.action_observations.push(self.make_observation(
                endpoint_frame,
                endpoint_state,
                &endpoint_wram,
                &prior_wram,
                evidence,
            ));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
        self.evidence = evidence;
        let next = if let Some(terminal_index) = terminal_index {
            let terminal_hold = match u8::try_from(terminal_index.saturating_add(1)) {
                Ok(hold) => hold,
                Err(_) => {
                    self.failed = true;
                    return;
                }
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
                self.failed = true;
                return;
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
                self.failed = true;
                return;
            }
            match self.machine.snapshot() {
                Ok(next) => next,
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        } else {
            match self.machine.snapshot() {
                Ok(next) => next,
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            self.failed = true;
            return;
        }
        self.current = next;
    }
}

impl<M: Machine> Target for ThwaiteTarget<M> {
    type Action = ButtonChord;
    type Observations = ThwaiteObservations;
    type Snapshot = ThwaiteSnapshot<M::Portable>;

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
        self.evidence = self.genesis_observation.evidence;
    }

    fn apply(&mut self, action: &Self::Action) {
        self.apply_internal(action);
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        let (standing, silo_total, enemy_left, crosshair_x, crosshair_y) =
            state.town.map_or((0, 0, 0, 0, 0), |town| {
                (
                    town.buildings_standing,
                    town.silo_missiles[0].saturating_add(town.silo_missiles[1]),
                    town.enemy_missiles_left,
                    town.crosshair_x,
                    town.crosshair_y,
                )
            });
        (u64::from(state.game_state) << 56)
            | (u64::from(state.level_index()) << 48)
            | (u64::from(standing) << 40)
            | (u64::from(silo_total) << 32)
            | (u64::from(enemy_left) << 24)
            | (u64::from(state.score) << 8)
            | (u64::from(crosshair_x >> 4) << 4)
            | u64::from(crosshair_y >> 4)
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
        Some(ThwaiteSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            wram: self.current_wram.to_vec(),
            failed: self.failed,
            evidence: self.evidence,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let restored_wram: [u8; WRAM_SIZE] = snapshot
            .wram
            .clone()
            .try_into()
            .map_err(|_| "Thwaite snapshot work RAM has an invalid length")?;
        let restored_state = decode_state(&restored_wram)?;
        if restored_state.num_players != self.genesis_observation.decoded.num_players
            || restored_state.practice != self.genesis_observation.decoded.practice
        {
            return Err("Thwaite snapshot belongs to a different workload".into());
        }
        let imported = self
            .machine
            .import(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.machine.replay(imported) {
            self.failed = true;
            let _ = self.machine.drop_snapshot(imported);
            return Err(error.to_string().into());
        }
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
        self.evidence = snapshot.evidence;
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<M: Machine> Drop for ThwaiteTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

pub const SETUP_LEAD_FRAMES: usize = 60;
pub const SETUP_PRESS_FRAMES: usize = 2;
pub const SETUP_RELEASE_FRAMES: usize = 58;
pub const SETUP_PRESSES: usize = 5;
pub const SETUP_TAIL_FRAMES: usize = 12;

pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    tape.extend((0..SETUP_LEAD_FRAMES).map(|_| ButtonChord::new(0, 1)));
    for press in 0..SETUP_PRESSES {
        tape.extend((0..SETUP_PRESS_FRAMES).map(|_| ButtonChord::new(0x01, 1)));
        let release = if press + 1 == SETUP_PRESSES {
            SETUP_TAIL_FRAMES
        } else {
            SETUP_RELEASE_FRAMES
        };
        tape.extend((0..release).map(|_| ButtonChord::new(0, 1)));
    }
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Thwaite RAM address {address:#x} is absent")))
}

fn slice(wram: &[u8], address: usize, len: usize) -> Result<&[u8], MachineError> {
    wram.get(address..address.saturating_add(len))
        .ok_or_else(|| {
            MachineError::Backend(format!("Thwaite RAM window {address:#x} is out of range"))
        })
}

pub fn decode_state(wram: &[u8]) -> Result<ThwaiteMechanicalState, MachineError> {
    let game_state = byte(wram, GAME_STATE)?;
    let buildings: [u8; NUM_BUILDINGS] = slice(wram, HOUSES_STANDING, NUM_BUILDINGS)?
        .try_into()
        .map_err(|_| MachineError::Backend("Thwaite building window is malformed".to_owned()))?;
    let enemy_missiles = slice(wram, MISSILE_Y_HI, NUM_MISSILES)?;
    let town = if game_state != STATE_INACTIVE && game_state != STATE_GAMEOVER {
        Some(ThwaiteTownState {
            buildings,
            buildings_standing: u8::try_from(
                buildings.iter().filter(|building| **building != 0).count(),
            )
            .unwrap_or(u8::MAX),
            silos_standing: u8::from(buildings[BUILDING_SILO0] != 0)
                + u8::from(buildings[BUILDING_SILO1] != 0),
            silo_missiles: [
                byte(wram, SILO_MISSILES_LEFT)?,
                byte(wram, SILO_MISSILES_LEFT + 1)?,
            ],
            enemy_missiles_left: byte(wram, ENEMY_MISSILES_LEFT)?,
            enemy_missiles_in_flight: u8::try_from(
                enemy_missiles[FIRST_ENEMY_MISSILE..]
                    .iter()
                    .filter(|position| **position != 0)
                    .count(),
            )
            .unwrap_or(u8::MAX),
            buildings_destroyed_this_level: byte(wram, BUILDINGS_DESTROYED_THIS_LEVEL)?,
            crosshair_x: byte(wram, CROSSHAIR_X_HI)?,
            crosshair_y: byte(wram, CROSSHAIR_Y_HI)?,
        })
    } else {
        None
    };
    Ok(ThwaiteMechanicalState {
        game_state,
        num_players: byte(wram, NUM_PLAYERS)?,
        practice: byte(wram, IS_PRACTICE)? != 0,
        score: u16::from(byte(wram, SCORE_100S)?)
            .saturating_mul(100)
            .saturating_add(u16::from(byte(wram, SCORE_1S)?)),
        game_day: byte(wram, GAME_DAY)?,
        game_hour: byte(wram, GAME_HOUR)?,
        game_minute: byte(wram, GAME_MINUTE)?,
        game_second: byte(wram, GAME_SECOND)?,
        town,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Thwaite system RAM window has invalid length".to_owned())
    })
}

pub fn accumulate_evidence(
    evidence: &mut ThwaiteDefenceEvidence,
    prior: ThwaiteMechanicalState,
    state: ThwaiteMechanicalState,
) {
    if let (Some(before), Some(after)) = (prior.town, state.town) {
        let lost = before
            .buildings
            .iter()
            .zip(after.buildings)
            .filter(|(before, after)| **before != 0 && *after == 0)
            .count();
        evidence.buildings_lost = evidence
            .buildings_lost
            .saturating_add(u16::try_from(lost).unwrap_or(u16::MAX));
        if prior.game_state == STATE_ACTIVE && state.game_state == STATE_LEVEL_REWARD {
            evidence.levels_cleared = evidence.levels_cleared.saturating_add(1);
            if before.buildings_destroyed_this_level == 0 {
                evidence.perfect_levels = evidence.perfect_levels.saturating_add(1);
            }
        }
    }
}

fn validate_genesis(state: ThwaiteMechanicalState) -> Result<(), MachineError> {
    let Some(town) = state.town else {
        return Err(MachineError::Backend(format!(
            "Thwaite setup did not reach a live town: state={} players={}",
            state.game_state, state.num_players,
        )));
    };
    if state.game_state != STATE_ACTIVE
        || state.num_players != 1
        || state.practice
        || state.level_index() != 0
        || state.score != 0
        || town.buildings_standing != u8::try_from(NUM_BUILDINGS).unwrap_or(u8::MAX)
        || town.silo_missiles != [INITIAL_SILO_MISSILES; 2]
        || town.enemy_missiles_left == 0
        || town.enemy_missiles_in_flight != 0
        || town.buildings_destroyed_this_level != 0
    {
        return Err(MachineError::Backend(format!(
            "Thwaite setup did not reach the first hour intact: state={} players={} practice={} level={} score={} standing={} silos={:?} enemy_left={} in_flight={} destroyed={}",
            state.game_state,
            state.num_players,
            state.practice,
            state.level_index(),
            state.score,
            town.buildings_standing,
            town.silo_missiles,
            town.enemy_missiles_left,
            town.enemy_missiles_in_flight,
            town.buildings_destroyed_this_level,
        )));
    }
    Ok(())
}

pub type ThwaiteDefenceBucket = (u8, u8, u8, u8, u8);

#[must_use]
pub fn defence_bucket(state: ThwaiteMechanicalState) -> Option<ThwaiteDefenceBucket> {
    state.town.map(|town| {
        (
            town.buildings_standing,
            town.enemy_missiles_left,
            town.enemy_missiles_in_flight,
            town.silo_missiles[0],
            town.silo_missiles[1],
        )
    })
}

#[must_use]
pub fn crosshair_cell(state: ThwaiteMechanicalState) -> Option<(u8, u8)> {
    state
        .town
        .map(|town| (town.crosshair_x / 16, town.crosshair_y / 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    fn gameplay_wram() -> [u8; WRAM_SIZE] {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[GAME_STATE] = STATE_ACTIVE;
        wram[NUM_PLAYERS] = 1;
        for index in 0..NUM_BUILDINGS {
            wram[HOUSES_STANDING + index] = 1;
        }
        wram[SILO_MISSILES_LEFT] = INITIAL_SILO_MISSILES;
        wram[SILO_MISSILES_LEFT + 1] = INITIAL_SILO_MISSILES;
        wram[ENEMY_MISSILES_LEFT] = 10;
        wram[CROSSHAIR_X_HI] = 64;
        wram[CROSSHAIR_Y_HI] = 128;
        wram
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FakeState {
        wram: Vec<u8>,
        cursor: usize,
    }

    struct ScriptedMachine {
        state: FakeState,
        timeline: Vec<Vec<u8>>,
        snapshots: BTreeMap<u64, FakeState>,
        next_id: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
        clock: u64,
    }

    impl ScriptedMachine {
        fn with_timeline(timeline: Vec<[u8; WRAM_SIZE]>) -> Self {
            let timeline: Vec<Vec<u8>> = timeline.into_iter().map(|wram| wram.to_vec()).collect();
            Self {
                state: FakeState {
                    wram: timeline[0].clone(),
                    cursor: 0,
                },
                timeline,
                snapshots: BTreeMap::new(),
                next_id: 0,
                staged: vec![],
                frames: vec![],
                clock: 0,
            }
        }

        fn save(&mut self, state: FakeState) -> SnapId {
            let id = SnapId(self.next_id);
            self.next_id += 1;
            self.snapshots.insert(id.0, state);
            id
        }
    }

    impl Machine for ScriptedMachine {
        type Portable = machine::SharedState;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            Ok(self.save(self.state.clone()))
        }

        fn drop_snapshot(&mut self, id: SnapId) -> Result<(), MachineError> {
            self.snapshots
                .remove(&id.0)
                .map(|_| ())
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn branch(&mut self, id: SnapId, input: &machine::Reproducer) -> Result<(), MachineError> {
            self.state = self
                .snapshots
                .get(&id.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged = nes::actions_of(input)?;
            Ok(())
        }

        fn replay(&mut self, id: SnapId) -> Result<(), MachineError> {
            self.state = self
                .snapshots
                .get(&id.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged.clear();
            Ok(())
        }

        fn run(
            &mut self,
            _: StopConditions,
            _: Option<&machine::Answer>,
        ) -> Result<machine::StopReason, MachineError> {
            self.frames.clear();
            for action in std::mem::take(&mut self.staged) {
                for _ in 0..action.bounded_hold_frames() {
                    self.state.cursor += 1;
                    self.state.wram =
                        self.timeline[self.state.cursor.min(self.timeline.len() - 1)].clone();
                    self.frames
                        .push(self.state.wram.clone().try_into().unwrap());
                    self.clock += 1;
                }
            }
            Ok(machine::StopReason::Quiescent { vtime: self.now() })
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
            let end = start
                .checked_add(len as usize)
                .ok_or(MachineError::ReadOutOfBounds)?;
            self.state
                .wram
                .get(start..end)
                .map(ToOwned::to_owned)
                .ok_or(MachineError::ReadOutOfBounds)
        }

        fn export(
            &mut self,
            id: SnapId,
            _: Option<&Self::Portable>,
        ) -> Result<Self::Portable, MachineError> {
            let state = self
                .snapshots
                .get(&id.0)
                .ok_or(MachineError::UnknownSnapshot)?;
            let bytes = serde_json::to_vec(state).unwrap();
            Ok(serde_json::from_value(serde_json::json!(bytes)).unwrap())
        }

        fn import(&mut self, state: &Self::Portable) -> Result<SnapId, MachineError> {
            let bytes: Vec<u8> =
                serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
            let decoded: FakeState = serde_json::from_slice(&bytes).unwrap();
            Ok(self.save(decoded))
        }

        fn portable_memory_charge(state: &Self::Portable) -> usize {
            QuickNesMachine::portable_memory_charge(state)
        }

        fn now(&self) -> machine::Moment {
            machine::Moment(self.clock)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    #[test]
    fn interior_frames_count_every_loss_and_cleared_hour() {
        let mut losing = gameplay_wram();
        losing[HOUSES_STANDING] = 0;
        losing[BUILDINGS_DESTROYED_THIS_LEVEL] = 1;
        let mut recovered = losing;
        recovered[ENEMY_MISSILES_LEFT] = 9;
        let mut cleared = recovered;
        cleared[GAME_STATE] = STATE_LEVEL_REWARD;
        cleared[ENEMY_MISSILES_LEFT] = 0;
        let machine = ScriptedMachine::with_timeline(vec![
            gameplay_wram(),
            losing,
            recovered,
            cleared,
            cleared,
        ]);
        let mut target = ThwaiteTarget::from_machine(machine).expect("scripted genesis");
        target.apply(&ButtonChord::new(0x01, 4));
        let observed = target.observe();
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(observed.frame_count, 4);
        assert_eq!(observed.evidence.buildings_lost, 1);
        assert_eq!(observed.evidence.levels_cleared, 1);
        assert_eq!(observed.evidence.perfect_levels, 0);
        assert!(!target.defended_a_perfect_hour());
        assert_eq!(observed.decoded.buildings_standing(), 11);
    }

    #[test]
    fn an_hour_cleared_without_a_loss_is_a_perfect_hour() {
        let mut thinning = gameplay_wram();
        thinning[ENEMY_MISSILES_LEFT] = 4;
        let mut cleared = thinning;
        cleared[GAME_STATE] = STATE_LEVEL_REWARD;
        cleared[ENEMY_MISSILES_LEFT] = 0;
        let machine =
            ScriptedMachine::with_timeline(vec![gameplay_wram(), thinning, cleared, cleared]);
        let mut target = ThwaiteTarget::from_machine(machine).expect("scripted genesis");
        target.apply(&ButtonChord::new(0x02, 3));
        let observed = target.observe();
        assert_eq!(observed.evidence.levels_cleared, 1);
        assert_eq!(observed.evidence.perfect_levels, 1);
        assert_eq!(observed.evidence.buildings_lost, 0);
        assert!(target.defended_a_perfect_hour());
    }

    #[test]
    fn an_interior_game_over_truncates_the_action_at_one_endpoint() {
        let mut ruined = gameplay_wram();
        ruined[GAME_STATE] = STATE_GAMEOVER;
        let after = gameplay_wram();
        let machine = ScriptedMachine::with_timeline(vec![
            gameplay_wram(),
            gameplay_wram(),
            ruined,
            after,
            after,
        ]);
        let mut target = ThwaiteTarget::from_machine(machine).expect("scripted genesis");
        target.apply(&ButtonChord::new(0x00, 4));
        let observed = target.observe();
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(observed.frame_count, 2);
        assert!(observed.terminal && target.is_game_over());
        assert_eq!(observed.decoded.town, None);
        assert_eq!(
            target
                .last_action_observations()
                .last()
                .map(|last| last.frame_count),
            Some(2)
        );
    }

    #[test]
    fn restored_snapshots_carry_defence_evidence() {
        let mut losing = gameplay_wram();
        losing[HOUSES_STANDING] = 0;
        losing[BUILDINGS_DESTROYED_THIS_LEVEL] = 1;
        let machine = ScriptedMachine::with_timeline(vec![gameplay_wram(), losing, losing, losing]);
        let mut target = ThwaiteTarget::from_machine(machine).expect("scripted genesis");
        target.apply(&ButtonChord::new(0x00, 1));
        let checkpoint = target.snapshot().expect("snapshot");
        assert_eq!(checkpoint.evidence.buildings_lost, 1);
        target.reset();
        assert_eq!(target.defence_evidence().buildings_lost, 0);
        target.restore(&checkpoint).expect("restore");
        assert_eq!(target.defence_evidence().buildings_lost, 1);
    }

    #[test]
    fn genesis_requires_an_intact_first_hour() {
        let wram = gameplay_wram();
        let state = decode_state(&wram).expect("decode");
        validate_genesis(state).expect("intact genesis");

        let mut damaged = wram;
        damaged[HOUSES_STANDING] = 0;
        let state = decode_state(&damaged).expect("decode");
        assert!(validate_genesis(state).is_err());

        let mut second_level = wram;
        second_level[GAME_HOUR] = 1;
        let state = decode_state(&second_level).expect("decode");
        assert!(validate_genesis(state).is_err());
    }

    #[test]
    fn score_combines_hundreds_and_ones() {
        let mut wram = gameplay_wram();
        wram[SCORE_100S] = 3;
        wram[SCORE_1S] = 45;
        assert_eq!(decode_state(&wram).expect("decode").score, 345);
    }

    #[test]
    fn terminal_states_carry_no_town() {
        let mut wram = gameplay_wram();
        wram[GAME_STATE] = STATE_GAMEOVER;
        let state = decode_state(&wram).expect("decode");
        assert!(state.terminal() && state.game_over() && !state.town_valid());

        wram[GAME_STATE] = STATE_INACTIVE;
        wram[GAME_DAY] = NUM_MADE_DAYS;
        let state = decode_state(&wram).expect("decode");
        assert!(state.terminal() && state.survived_all_days());
    }

    #[test]
    fn evidence_counts_losses_and_perfect_hours() {
        let before = decode_state(&gameplay_wram()).expect("decode");
        let mut destroyed = gameplay_wram();
        destroyed[HOUSES_STANDING] = 0;
        destroyed[HOUSES_STANDING + 1] = 0;
        destroyed[BUILDINGS_DESTROYED_THIS_LEVEL] = 2;
        let after = decode_state(&destroyed).expect("decode");
        let mut evidence = ThwaiteDefenceEvidence::default();
        accumulate_evidence(&mut evidence, before, after);
        assert_eq!(evidence.buildings_lost, 2);

        let mut cleared = destroyed;
        cleared[GAME_STATE] = STATE_LEVEL_REWARD;
        let cleared = decode_state(&cleared).expect("decode");
        accumulate_evidence(&mut evidence, after, cleared);
        assert_eq!(evidence.levels_cleared, 1);
        assert_eq!(evidence.perfect_levels, 0);

        let mut evidence = ThwaiteDefenceEvidence::default();
        let mut intact_reward = gameplay_wram();
        intact_reward[GAME_STATE] = STATE_LEVEL_REWARD;
        let intact_reward = decode_state(&intact_reward).expect("decode");
        accumulate_evidence(&mut evidence, before, intact_reward);
        assert_eq!(evidence.levels_cleared, 1);
        assert_eq!(evidence.perfect_levels, 1);
    }

    #[test]
    fn setup_tape_presses_only_a_and_holds_single_frames() {
        let tape = setup_tape();
        assert_eq!(
            tape.len(),
            SETUP_LEAD_FRAMES
                + SETUP_PRESSES * SETUP_PRESS_FRAMES
                + (SETUP_PRESSES - 1) * SETUP_RELEASE_FRAMES
                + SETUP_TAIL_FRAMES
        );
        assert!(
            tape.iter().all(
                |chord| chord.hold_frames == 1 && (chord.buttons == 0 || chord.buttons == 0x01)
            )
        );
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == 0x01).count(),
            SETUP_PRESSES * SETUP_PRESS_FRAMES
        );
    }
}
