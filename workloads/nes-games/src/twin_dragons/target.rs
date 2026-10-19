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

pub type TwinDragonsInput = crate::search::archive::Input<ButtonChord>;

pub const LEVEL_COUNT: u8 = 3;
pub const INITIAL_HEALTH: u8 = 3;

#[cfg(test)]
const GAME_STATE_TITLE: u8 = 0;
const GAME_STATE_PLAYING: u8 = 1;
const GAME_STATE_LOADING: u8 = 2;
const FINAL_NEXT_LEVEL: u8 = 0xff;

const NEXT_LEVEL: usize = 0x35;
const CURRENT_LEVEL: usize = 0x42;
const CAMERA_SCREEN: usize = 0x52;
const CAMERA_FINE: usize = 0x57;
const PLAYER_X: usize = 0x62;
const PLAYER_Y: usize = 0x63;
const PLAYER_ON_GROUND: usize = 0x6b;
const PLAYER_FACING: usize = 0x74;
const PLAYER_HEALTH: usize = 0x75;
const PLAYER_INVINCIBLE: usize = 0x76;
const GAME_STATE: usize = 0x9e;
const TRANSITION_TIMER: usize = 0xce;
const TOUCHED_GOAL: usize = 0x04e3;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TwinDragonsGameplayState {
    pub x: i16,
    pub y: i16,
    pub health: u8,
    pub on_ground: bool,
    pub facing: u8,
    pub invincible: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TwinDragonsMechanicalState {
    pub game_state: u8,
    pub level: u8,
    pub next_level: u8,
    pub loaded: bool,
    pub goal: bool,
    pub transition_timer: u8,
    pub gameplay: Option<TwinDragonsGameplayState>,
}

impl TwinDragonsMechanicalState {
    #[must_use]
    pub fn playing(self) -> bool {
        self.game_state == GAME_STATE_PLAYING
    }

    #[must_use]
    pub fn loading(self) -> bool {
        self.game_state == GAME_STATE_LOADING
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.playing() && self.loaded && !self.goal && self.gameplay.is_none()
    }

    #[must_use]
    pub fn levels_cleared(self) -> u8 {
        match self.game_state {
            GAME_STATE_PLAYING if self.goal => self.level.saturating_add(1),
            GAME_STATE_PLAYING | GAME_STATE_LOADING => self.level,
            _ => 0,
        }
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.levels_cleared() >= LEVEL_COUNT
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsObservations {
    pub frame_count: u64,
    pub decoded: TwinDragonsMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: TwinDragonsMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: TwinDragonsObservations,
    pub failed: bool,
}

impl TwinDragonsSnapshot {
    #[must_use]
    pub fn state(&self) -> TwinDragonsMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct TwinDragonsTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: TwinDragonsObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: TwinDragonsObservations,
    action_observations: Vec<TwinDragonsObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> TwinDragonsTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = TwinDragonsObservations {
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
                "Twin Dragons setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> TwinDragonsMechanicalState {
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
    pub fn last_action_observations(&self) -> &[TwinDragonsObservations] {
        &self.action_observations
    }

    fn make_observation(
        frame_count: u64,
        state: TwinDragonsMechanicalState,
    ) -> TwinDragonsObservations {
        TwinDragonsObservations {
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
                decode_state(wram).is_ok_and(TwinDragonsMechanicalState::terminal)
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
impl TwinDragonsTarget<QuickNesMachine> {
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
        input: &TwinDragonsInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<TwinDragonsVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Twin Dragons gameplay genesis for film".into());
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
        metadata: &mut Option<TwinDragonsVideoMetadata>,
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
                    *metadata = Some(TwinDragonsVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: TwinDragonsMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for TwinDragonsTarget<M> {
    type Action = ButtonChord;
    type Observations = TwinDragonsObservations;
    type Snapshot = TwinDragonsSnapshot;

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
        let (x, y, health) = state.gameplay.map_or((0, 0, 0), |gameplay| {
            (
                gameplay.x.div_euclid(32),
                gameplay.y.div_euclid(32),
                gameplay.health,
            )
        });
        let x = u64::from(u16::from_ne_bytes(x.to_ne_bytes()));
        let y = u64::from(u16::from_ne_bytes(y.to_ne_bytes()));
        (u64::from(state.game_state) << 56)
            | (u64::from(state.level) << 48)
            | (u64::from(state.goal) << 40)
            | (u64::from(health) << 32)
            | ((x & 0xffff) << 16)
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
        Some(TwinDragonsSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for TwinDragonsTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    wait(&mut tape, 60);
    tape.push(ButtonChord::new(0x08, 2));
    wait(&mut tape, 240);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Twin Dragons RAM address {address:#x} is absent"))
    })
}

pub fn decode_state(wram: &[u8]) -> Result<TwinDragonsMechanicalState, MachineError> {
    let game_state = byte(wram, GAME_STATE)?;
    let level = byte(wram, CURRENT_LEVEL)?;
    let next_level = byte(wram, NEXT_LEVEL)?;
    let loaded = next_level == expected_next_level(level);
    let goal = loaded && byte(wram, TOUCHED_GOAL)? != 0;
    let health = byte(wram, PLAYER_HEALTH)?;
    let gameplay = if game_state == GAME_STATE_PLAYING && loaded && !goal && health != 0 {
        let camera =
            i16::from(byte(wram, CAMERA_SCREEN)?) * 256 + i16::from(byte(wram, CAMERA_FINE)?);
        Some(TwinDragonsGameplayState {
            x: camera + i16::from(byte(wram, PLAYER_X)?),
            y: i16::from(byte(wram, PLAYER_Y)?),
            health,
            on_ground: byte(wram, PLAYER_ON_GROUND)? != 0,
            facing: byte(wram, PLAYER_FACING)?,
            invincible: byte(wram, PLAYER_INVINCIBLE)? != 0,
        })
    } else {
        None
    };
    Ok(TwinDragonsMechanicalState {
        game_state,
        level,
        next_level,
        loaded,
        goal,
        transition_timer: byte(wram, TRANSITION_TIMER)?,
        gameplay,
    })
}

fn expected_next_level(level: u8) -> u8 {
    if level.saturating_add(1) >= LEVEL_COUNT {
        FINAL_NEXT_LEVEL
    } else {
        level.saturating_add(1)
    }
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Twin Dragons system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: TwinDragonsMechanicalState) -> Result<(), MachineError> {
    let valid = state.game_state == GAME_STATE_PLAYING
        && state.level == 0
        && state.next_level == 1
        && state
            .gameplay
            .is_some_and(|gameplay| gameplay.health == INITIAL_HEALTH);
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Twin Dragons setup did not reach level 1 gameplay: {state:?}"
        )))
    }
}

#[must_use]
pub fn spatial_bucket(state: TwinDragonsMechanicalState) -> Option<(i16, i16)> {
    state
        .gameplay
        .map(|gameplay| (gameplay.x.div_euclid(16), gameplay.y.div_euclid(16)))
}

#[must_use]
pub fn boundary_fields(state: TwinDragonsMechanicalState) -> (u8, u8, bool, u8, bool) {
    (
        state.game_state,
        state.level,
        state.goal,
        state.gameplay.map_or(0, |gameplay| gameplay.health),
        state.gameplay.is_some_and(|gameplay| gameplay.on_ground),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_STATE] = GAME_STATE_PLAYING;
        wram[NEXT_LEVEL] = 1;
        wram[PLAYER_HEALTH] = INITIAL_HEALTH;
        wram[PLAYER_X] = 120;
        wram[PLAYER_Y] = 175;
        wram
    }

    #[test]
    fn world_x_adds_the_camera_screen_and_fine_scroll() {
        let mut wram = playing_wram();
        wram[CAMERA_SCREEN] = 2;
        wram[CAMERA_FINE] = 40;
        wram[PLAYER_X] = 136;
        let gameplay = decode_state(&wram).unwrap().gameplay.unwrap();
        assert_eq!(gameplay.x, 2 * 256 + 40 + 136);
        assert_eq!(gameplay.y, 175);
    }

    #[test]
    fn zero_health_is_a_terminal_death() {
        let mut wram = playing_wram();
        wram[PLAYER_HEALTH] = 0;
        let state = decode_state(&wram).unwrap();
        assert!(state.dead());
        assert!(state.terminal());
        assert!(!state.victory());
    }

    #[test]
    fn touching_the_final_goal_is_victory() {
        let mut wram = playing_wram();
        wram[CURRENT_LEVEL] = LEVEL_COUNT - 1;
        wram[NEXT_LEVEL] = FINAL_NEXT_LEVEL;
        wram[TOUCHED_GOAL] = 1;
        let state = decode_state(&wram).unwrap();
        assert!(state.gameplay.is_none());
        assert!(!state.dead());
        assert_eq!(state.levels_cleared(), LEVEL_COUNT);
        assert!(state.victory());
    }

    #[test]
    fn an_earlier_goal_clears_one_level_without_ending_the_game() {
        let mut wram = playing_wram();
        wram[TOUCHED_GOAL] = 1;
        let cleared = decode_state(&wram).unwrap();
        assert_eq!(cleared.levels_cleared(), 1);
        assert!(!cleared.terminal());
        wram[TOUCHED_GOAL] = 0;
        wram[GAME_STATE] = GAME_STATE_LOADING;
        wram[CURRENT_LEVEL] = 1;
        let loading = decode_state(&wram).unwrap();
        assert_eq!(loading.levels_cleared(), 1);
        assert!(loading.gameplay.is_none());
        assert!(!loading.terminal());
    }

    #[test]
    fn a_stale_goal_during_the_next_level_load_is_not_a_clear() {
        let mut wram = playing_wram();
        wram[CURRENT_LEVEL] = 1;
        wram[TOUCHED_GOAL] = 1;
        let state = decode_state(&wram).unwrap();
        assert!(!state.loaded);
        assert!(!state.goal);
        assert!(state.gameplay.is_none());
        assert!(!state.terminal());
        assert_eq!(state.levels_cleared(), 1);
        wram[NEXT_LEVEL] = FINAL_NEXT_LEVEL;
        wram[CURRENT_LEVEL] = LEVEL_COUNT - 1;
        assert!(decode_state(&wram).unwrap().victory());
    }

    #[test]
    fn genesis_requires_level_one_at_full_health() {
        let state = decode_state(&playing_wram()).unwrap();
        assert!(validate_genesis(state).is_ok());
        let mut wram = playing_wram();
        wram[GAME_STATE] = GAME_STATE_TITLE;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn setup_tape_presses_start_without_other_buttons() {
        let tape = setup_tape();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 1);
        assert!(tape.iter().all(|chord| chord.buttons & !0x08 == 0));
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for TwinDragonsSnapshot {
    type Codec = searcher::search::snapshot_codec::PostcardSnapshotCodec<Self>;
}
