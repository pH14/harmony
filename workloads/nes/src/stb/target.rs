// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

pub type StbInput = crate::search::archive::Input<ButtonChord>;

pub const INITIAL_STOCKS: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StbAi {
    Easy = 1,
    Fair = 2,
    Hard = 3,
}

impl StbAi {
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Fair => "fair",
            Self::Hard => "hard",
        }
    }
}
impl std::str::FromStr for StbAi {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "easy" => Ok(Self::Easy),
            "fair" => Ok(Self::Fair),
            "hard" => Ok(Self::Hard),
            _ => Err(format!(
                "unknown STB AI {value:?}; expected easy, fair, or hard"
            )),
        }
    }
}

const GAME_STATE_INGAME: u8 = 0x00;
const GAME_STATE_GAMEOVER: u8 = 0x02;
const GAME_MODE_LOCAL: u8 = 0x00;
const PLAYER_STATE_INNEXISTANT: u8 = 0x02;

const PLAYER_A_STATE: usize = 0x00;
const PLAYER_B_STATE: usize = 0x01;
const PLAYER_A_HITSTUN: usize = 0x02;
const PLAYER_B_HITSTUN: usize = 0x03;
const PLAYER_A_X: usize = 0x04;
const PLAYER_B_X: usize = 0x05;
const PLAYER_A_Y: usize = 0x06;
const PLAYER_B_Y: usize = 0x07;
const PLAYER_A_DIRECTION: usize = 0x08;
const PLAYER_B_DIRECTION: usize = 0x09;
const PLAYER_A_X_SCREEN: usize = 0x0e;
const PLAYER_B_X_SCREEN: usize = 0x0f;
const PLAYER_A_Y_SCREEN: usize = 0x10;
const PLAYER_B_Y_SCREEN: usize = 0x11;
const PLAYER_A_STATE_CLOCK: usize = 0x12;
const PLAYER_B_STATE_CLOCK: usize = 0x13;
const PLAYER_A_DAMAGE: usize = 0x48;
const PLAYER_B_DAMAGE: usize = 0x49;
const PLAYER_A_STOCKS: usize = 0x54;
const PLAYER_B_STOCKS: usize = 0x55;
const PLAYER_A_GROUNDED: usize = 0x62;
const PLAYER_B_GROUNDED: usize = 0x63;
const PLAYER_A_WALLED: usize = 0x64;
const PLAYER_B_WALLED: usize = 0x65;
const GAME_WINNER: usize = 0x05db;
const GLOBAL_GAME_STATE: usize = 0xd4;
const CONFIG_INITIAL_STOCKS: usize = 0xd9;
const CONFIG_AI_LEVEL: usize = 0xda;
const CONFIG_SELECTED_STAGE: usize = 0xdb;
const CONFIG_GAME_MODE: usize = 0xe2;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbGameplayState {
    pub player_a_state: u8,
    pub player_b_state: u8,
    pub player_a_x: i16,
    pub player_b_x: i16,
    pub player_a_y: i16,
    pub player_b_y: i16,
    pub player_a_x_screen: i8,
    pub player_b_x_screen: i8,
    pub player_a_y_screen: i8,
    pub player_b_y_screen: i8,
    pub player_a_direction: u8,
    pub player_b_direction: u8,
    pub player_a_damage: u8,
    pub player_b_damage: u8,
    pub player_a_stocks: u8,
    pub player_b_stocks: u8,
    pub player_a_state_clock: u8,
    pub player_b_state_clock: u8,
    pub player_a_hitstun: u8,
    pub player_b_hitstun: u8,
    pub player_a_grounded: bool,
    pub player_b_grounded: bool,
    pub player_a_walled: bool,
    pub player_b_walled: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbMechanicalState {
    pub game_state: u8,
    pub game_mode: u8,
    pub ai_level: u8,
    pub stage: u8,
    pub gameplay: Option<StbGameplayState>,
    pub game_winner: u8,
}

impl StbMechanicalState {
    #[must_use]
    pub fn match_over(self) -> bool {
        self.game_state == GAME_STATE_GAMEOVER
    }

    #[must_use]
    pub fn player_a_won(self) -> bool {
        self.match_over() && self.game_winner == 0
    }

    #[must_use]
    pub fn gameplay_valid(self) -> bool {
        self.gameplay.is_some()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbObservations {
    pub frame_count: u64,
    pub decoded: StbMechanicalState,
    pub changed_indices: Vec<u16>,
    #[serde(default)]
    pub player_a_ko: bool,
    #[serde(default)]
    pub player_b_ko: bool,
    #[serde(default)]
    pub player_a_ko_count: u8,
    #[serde(default)]
    pub player_b_ko_count: u8,
    #[serde(default)]
    pub terminal: bool,
    pub log_line: String,
}

#[derive(Clone, Copy, Debug)]
struct StbStockEvidence {
    player_a_ko: bool,
    player_b_ko: bool,
    player_a_ko_count: u8,
    player_b_ko_count: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub input_endpoint: StbMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbSnapshot<P = machine::SharedState> {
    pub(crate) emulator_state: P,
    pub(crate) observation: StbObservations,
    pub(crate) wram: Vec<u8>,
    pub(crate) failed: bool,
    #[serde(default)]
    pub(crate) last_valid_gameplay: Option<StbGameplayState>,
    #[serde(default)]
    pub(crate) player_a_ko_count: u8,
    #[serde(default)]
    pub(crate) player_b_ko_count: u8,
}

impl<P> StbSnapshot<P> {
    #[must_use]
    pub fn state(&self) -> StbMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct StbTarget<M: Machine = QuickNesMachine> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: StbObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: StbObservations,
    action_observations: Vec<StbObservations>,
    last_valid_gameplay: Option<StbGameplayState>,
    player_a_ko_count: u8,
    player_b_ko_count: u8,
    failed: bool,
    snapshot_base: Option<M::Portable>,
    execution_work: u64,
}

impl<M: Machine> StbTarget<M> {
    pub fn from_machine(machine: M) -> Result<Self, MachineError> {
        Self::from_machine_with_ai(machine, StbAi::Easy)
    }

    pub fn from_machine_with_ai(mut machine: M, ai: StbAi) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state, byte(&wram, CONFIG_INITIAL_STOCKS)?, ai)?;
        let genesis = machine.snapshot()?;
        let observation = StbObservations {
            frame_count: 0,
            decoded: state,
            changed_indices: Vec::new(),
            player_a_ko: false,
            player_b_ko: false,
            player_a_ko_count: 0,
            player_b_ko_count: 0,
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
            last_valid_gameplay: state.gameplay,
            player_a_ko_count: 0,
            player_b_ko_count: 0,
            failed: false,
            snapshot_base: None,
            execution_work: 0,
        })
    }

    #[must_use]
    pub fn mechanical_state(&self) -> StbMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_match_over(&self) -> bool {
        self.observation.decoded.match_over()
    }

    #[must_use]
    pub fn player_a_won(&self) -> bool {
        self.observation.decoded.player_a_won()
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[StbObservations] {
        &self.action_observations
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_match_over() || frames == 0 {
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
                Ok(state) => !state.match_over(),
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

impl StbTarget<QuickNesMachine> {
    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_headless_with_ai(rom, core_path, core_sha256, StbAi::Easy)
    }

    pub fn from_rom_bytes_headless_with_ai(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        ai: StbAi,
    ) -> Result<Self, MachineError> {
        let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&setup_tape_with_ai(ai)))?;
        let run = machine.run(StopConditions::default(), None)?;
        if !matches!(run, machine::StopReason::Quiescent { .. }) {
            return Err(MachineError::Backend(
                "Super Tilt Bro setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine_with_ai(machine, ai)
    }

    pub fn render_input(
        &mut self,
        input: &StbInput,
        tail_frames: u32,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<StbVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore STB gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            for action in &input.actions {
                self.render_action(*action, video_output, audio_output, &mut metadata)?;
                if decode_state(&read_wram(&self.machine)?)?.match_over() {
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
        metadata: &mut Option<StbVideoMetadata>,
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
                    *metadata = Some(StbVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        input_endpoint: StbMechanicalState::default(),
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

impl<M: Machine> StbTarget<M> {
    fn make_observation(
        &self,
        frame_count: u64,
        state: StbMechanicalState,
        wram: &[u8; WRAM_SIZE],
        prior_wram: &[u8; WRAM_SIZE],
        evidence: StbStockEvidence,
    ) -> StbObservations {
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
        StbObservations {
            frame_count,
            decoded: state,
            changed_indices: changed_indices.clone(),
            player_a_ko: evidence.player_a_ko,
            player_b_ko: evidence.player_b_ko,
            player_a_ko_count: evidence.player_a_ko_count,
            player_b_ko_count: evidence.player_b_ko_count,
            terminal: state.match_over(),
            log_line: format!(
                "frame={frame_count} changed={changed_indices:?} ko_a={} ko_b={} count_a={} count_b={}",
                evidence.player_a_ko,
                evidence.player_b_ko,
                evidence.player_a_ko_count,
                evidence.player_b_ko_count,
            ),
        }
    }

    fn apply_internal(&mut self, action: &ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.is_match_over() {
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
            .position(|wram| decode_state(wram).is_ok_and(StbMechanicalState::match_over));
        let logical_frames =
            terminal_index.map_or(produced_frames, |index| index.saturating_add(1));
        let (
            endpoint_wram,
            endpoint_state,
            emitted,
            prior_wram,
            last_valid_gameplay,
            player_a_ko_count,
            player_b_ko_count,
            endpoint_player_a_ko,
            endpoint_player_b_ko,
        ) = {
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
            let mut prior_state = initial_state;
            let mut emitted = false;
            let mut last_valid_gameplay = self.last_valid_gameplay;
            let mut player_a_ko_count = self.player_a_ko_count;
            let mut player_b_ko_count = self.player_b_ko_count;
            let mut endpoint_player_a_ko = false;
            let mut endpoint_player_b_ko = false;
            let mut terminal_loss_recorded = false;
            for (offset, wram) in frames.iter().take(logical_frames).enumerate() {
                let Ok(state) = decode_state(wram) else {
                    self.failed = true;
                    return;
                };
                let (player_a_ko, player_b_ko) = if let Some(gameplay) = state.gameplay {
                    let player_a_ko = last_valid_gameplay
                        .is_some_and(|prior| gameplay.player_a_stocks < prior.player_a_stocks);
                    let player_b_ko = last_valid_gameplay
                        .is_some_and(|prior| gameplay.player_b_stocks < prior.player_b_stocks);
                    player_a_ko_count = player_a_ko_count
                        .max(INITIAL_STOCKS.saturating_sub(gameplay.player_a_stocks));
                    player_b_ko_count = player_b_ko_count
                        .max(INITIAL_STOCKS.saturating_sub(gameplay.player_b_stocks));
                    last_valid_gameplay = Some(gameplay);
                    (player_a_ko, player_b_ko)
                } else if state.match_over() && !terminal_loss_recorded {
                    let player_a_ko = state.game_winner == 1
                        && last_valid_gameplay
                            .is_some_and(|gameplay| gameplay.player_a_stocks == 0);
                    let player_b_ko = state.game_winner == 0
                        && last_valid_gameplay
                            .is_some_and(|gameplay| gameplay.player_b_stocks == 0);
                    if player_a_ko {
                        player_a_ko_count = player_a_ko_count.saturating_add(1);
                    }
                    if player_b_ko {
                        player_b_ko_count = player_b_ko_count.saturating_add(1);
                    }
                    terminal_loss_recorded = true;
                    (player_a_ko, player_b_ko)
                } else {
                    (false, false)
                };
                endpoint_player_a_ko = player_a_ko;
                endpoint_player_b_ko = player_b_ko;
                let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                    || boundary_fields(state) != boundary_fields(prior_state)
                    || state.game_state != prior_state.game_state;
                if boundary {
                    let frame_count = self.observation.frame_count.saturating_add(
                        u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1),
                    );
                    self.action_observations.push(self.make_observation(
                        frame_count,
                        state,
                        wram,
                        &prior_wram,
                        StbStockEvidence {
                            player_a_ko,
                            player_b_ko,
                            player_a_ko_count,
                            player_b_ko_count,
                        },
                    ));
                    prior_wram = *wram;
                    prior_state = state;
                    emitted = true;
                }
            }
            (
                endpoint_wram,
                endpoint_state,
                emitted,
                prior_wram,
                last_valid_gameplay,
                player_a_ko_count,
                player_b_ko_count,
                endpoint_player_a_ko,
                endpoint_player_b_ko,
            )
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
                StbStockEvidence {
                    player_a_ko: endpoint_player_a_ko,
                    player_b_ko: endpoint_player_b_ko,
                    player_a_ko_count,
                    player_b_ko_count,
                },
            ));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        self.current_wram = endpoint_wram;
        self.last_valid_gameplay = last_valid_gameplay;
        self.player_a_ko_count = player_a_ko_count;
        self.player_b_ko_count = player_b_ko_count;
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

impl<M: Machine> Target for StbTarget<M> {
    type Action = ButtonChord;
    type Observations = StbObservations;
    type Snapshot = StbSnapshot<M::Portable>;

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
        self.last_valid_gameplay = self.genesis_observation.decoded.gameplay;
        self.player_a_ko_count = self.genesis_observation.player_a_ko_count;
        self.player_b_ko_count = self.genesis_observation.player_b_ko_count;
    }

    fn apply(&mut self, action: &Self::Action) {
        self.apply_internal(action);
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        let (
            player_a_state,
            player_b_state,
            player_a_stocks,
            player_b_stocks,
            player_a_x,
            player_b_x,
        ) = state.gameplay.map_or((0, 0, 0, 0, 0, 0), |gameplay| {
            (
                gameplay.player_a_state,
                gameplay.player_b_state,
                gameplay.player_a_stocks,
                gameplay.player_b_stocks,
                gameplay.player_a_x.div_euclid(32),
                gameplay.player_b_x.div_euclid(32),
            )
        });
        let player_a_x = u64::from(u16::from_ne_bytes(player_a_x.to_ne_bytes()));
        let player_b_x = u64::from(u16::from_ne_bytes(player_b_x.to_ne_bytes()));
        (u64::from(state.game_state) << 56)
            | (u64::from(player_a_state) << 48)
            | (u64::from(player_b_state) << 40)
            | (u64::from(player_a_stocks) << 32)
            | (u64::from(player_b_stocks) << 24)
            | ((player_a_x & 0xff) << 8)
            | (player_b_x & 0xff)
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
        Some(StbSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            wram: self.current_wram.to_vec(),
            failed: self.failed,
            last_valid_gameplay: self.last_valid_gameplay,
            player_a_ko_count: self.player_a_ko_count,
            player_b_ko_count: self.player_b_ko_count,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let restored_wram: [u8; WRAM_SIZE] = snapshot
            .wram
            .clone()
            .try_into()
            .map_err(|_| "STB snapshot work RAM has an invalid length")?;
        if decode_state(&restored_wram)?.ai_level != self.genesis_observation.decoded.ai_level {
            return Err("STB snapshot belongs to a different AI workload".into());
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
        self.last_valid_gameplay = snapshot
            .last_valid_gameplay
            .or(self.observation.decoded.gameplay);
        self.player_a_ko_count = snapshot.player_a_ko_count;
        self.player_b_ko_count = snapshot.player_b_ko_count;
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<M: Machine> Drop for StbTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

pub fn setup_tape() -> Vec<ButtonChord> {
    setup_tape_with_ai(StbAi::Easy)
}

pub fn setup_tape_with_ai(ai: StbAi) -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let press_release = |tape: &mut Vec<ButtonChord>, button| {
        tape.push(ButtonChord::new(button, 1));
        tape.push(ButtonChord::new(0, 1));
    };
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    wait(&mut tape, 180);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    if ai != StbAi::Easy {
        press_release(&mut tape, 0x20);
        press_release(&mut tape, 0x20);
        for _ in 1..ai.level() {
            press_release(&mut tape, 0x80);
        }
    }
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 240);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 90);
    tape
}

fn signed_world_pixels(pixel: u8, screen: u8) -> i16 {
    i16::from(i8::from_ne_bytes([screen])) * 256 + i16::from(pixel)
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("STB RAM address {address:#x} is absent")))
}

pub fn decode_state(wram: &[u8]) -> Result<StbMechanicalState, MachineError> {
    let game_state = byte(wram, GLOBAL_GAME_STATE)?;
    let game_mode = byte(wram, CONFIG_GAME_MODE)?;
    let ai_level = byte(wram, CONFIG_AI_LEVEL)?;
    let stage = byte(wram, CONFIG_SELECTED_STAGE)?;
    let player_a_state = byte(wram, PLAYER_A_STATE)?;
    let player_b_state = byte(wram, PLAYER_B_STATE)?;
    let gameplay = if game_state == GAME_STATE_INGAME
        && player_a_state != PLAYER_STATE_INNEXISTANT
        && player_b_state != PLAYER_STATE_INNEXISTANT
    {
        Some(StbGameplayState {
            player_a_state,
            player_b_state,
            player_a_x: signed_world_pixels(
                byte(wram, PLAYER_A_X)?,
                byte(wram, PLAYER_A_X_SCREEN)?,
            ),
            player_b_x: signed_world_pixels(
                byte(wram, PLAYER_B_X)?,
                byte(wram, PLAYER_B_X_SCREEN)?,
            ),
            player_a_y: signed_world_pixels(
                byte(wram, PLAYER_A_Y)?,
                byte(wram, PLAYER_A_Y_SCREEN)?,
            ),
            player_b_y: signed_world_pixels(
                byte(wram, PLAYER_B_Y)?,
                byte(wram, PLAYER_B_Y_SCREEN)?,
            ),
            player_a_x_screen: i8::from_ne_bytes([byte(wram, PLAYER_A_X_SCREEN)?]),
            player_b_x_screen: i8::from_ne_bytes([byte(wram, PLAYER_B_X_SCREEN)?]),
            player_a_y_screen: i8::from_ne_bytes([byte(wram, PLAYER_A_Y_SCREEN)?]),
            player_b_y_screen: i8::from_ne_bytes([byte(wram, PLAYER_B_Y_SCREEN)?]),
            player_a_direction: byte(wram, PLAYER_A_DIRECTION)?,
            player_b_direction: byte(wram, PLAYER_B_DIRECTION)?,
            player_a_damage: byte(wram, PLAYER_A_DAMAGE)?,
            player_b_damage: byte(wram, PLAYER_B_DAMAGE)?,
            player_a_stocks: byte(wram, PLAYER_A_STOCKS)?,
            player_b_stocks: byte(wram, PLAYER_B_STOCKS)?,
            player_a_state_clock: byte(wram, PLAYER_A_STATE_CLOCK)?,
            player_b_state_clock: byte(wram, PLAYER_B_STATE_CLOCK)?,
            player_a_hitstun: byte(wram, PLAYER_A_HITSTUN)?,
            player_b_hitstun: byte(wram, PLAYER_B_HITSTUN)?,
            player_a_grounded: byte(wram, PLAYER_A_GROUNDED)? != 0,
            player_b_grounded: byte(wram, PLAYER_B_GROUNDED)? != 0,
            player_a_walled: byte(wram, PLAYER_A_WALLED)? != 0,
            player_b_walled: byte(wram, PLAYER_B_WALLED)? != 0,
        })
    } else {
        None
    };
    Ok(StbMechanicalState {
        game_state,
        game_mode,
        ai_level,
        stage,
        gameplay,
        game_winner: byte(wram, GAME_WINNER)?,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine
        .read(0, WRAM_SIZE as u32)?
        .try_into()
        .map_err(|_| MachineError::Backend("STB system RAM window has invalid length".to_owned()))
}

fn validate_genesis(
    state: StbMechanicalState,
    initial_stocks: u8,
    ai: StbAi,
) -> Result<(), MachineError> {
    let Some(gameplay) = state.gameplay else {
        return Err(MachineError::Backend(format!(
            "Super Tilt Bro setup did not reach live local AI match: state={} mode={} ai={} gameplay=none",
            state.game_state, state.game_mode, state.ai_level,
        )));
    };
    if state.game_state != GAME_STATE_INGAME
        || state.game_mode != GAME_MODE_LOCAL
        || state.ai_level != ai.level()
        || state.stage != 0
        || initial_stocks != INITIAL_STOCKS
        || gameplay.player_a_stocks != INITIAL_STOCKS
        || gameplay.player_b_stocks != INITIAL_STOCKS
    {
        return Err(MachineError::Backend(format!(
            "Super Tilt Bro setup did not reach live local AI match: state={} mode={} ai={} stage={} initial_stocks={} player_states=({}, {}) stocks=({}, {})",
            state.game_state,
            state.game_mode,
            state.ai_level,
            state.stage,
            initial_stocks,
            gameplay.player_a_state,
            gameplay.player_b_state,
            gameplay.player_a_stocks,
            gameplay.player_b_stocks,
        )));
    }
    Ok(())
}

#[must_use]
pub fn spatial_bucket(state: StbMechanicalState) -> Option<(i16, i16, i16, i16)> {
    state.gameplay.map(|gameplay| {
        (
            gameplay.player_a_x.div_euclid(16),
            gameplay.player_a_y.div_euclid(16),
            gameplay.player_b_x.div_euclid(16),
            gameplay.player_b_y.div_euclid(16),
        )
    })
}

pub type StbBoundaryFields = (u8, u8, u8, u8, u8, u8, bool, bool);

#[must_use]
pub fn boundary_fields(state: StbMechanicalState) -> Option<StbBoundaryFields> {
    state.gameplay.map(|gameplay| {
        (
            gameplay.player_a_stocks,
            gameplay.player_b_stocks,
            gameplay.player_b_damage,
            gameplay.player_a_damage,
            gameplay.player_a_hitstun,
            gameplay.player_b_hitstun,
            gameplay.player_a_grounded,
            gameplay.player_b_grounded,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

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
        fail_drop: bool,
        skip_replay: bool,
    }

    fn live_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[PLAYER_A_STATE] = 5;
        wram[PLAYER_B_STATE] = 5;
        wram[PLAYER_A_STOCKS] = INITIAL_STOCKS;
        wram[PLAYER_B_STOCKS] = INITIAL_STOCKS;
        wram[CONFIG_INITIAL_STOCKS] = INITIAL_STOCKS;
        wram[CONFIG_AI_LEVEL] = 1;
        wram
    }

    impl ScriptedMachine {
        fn match_timeline() -> Self {
            let mut timeline = vec![live_wram()];
            for stocks in (0..INITIAL_STOCKS).rev() {
                let mut wram = live_wram();
                wram[PLAYER_A_STOCKS] = stocks;
                wram[PLAYER_B_STOCKS] = stocks;
                timeline.push(wram);
            }
            let mut invalid = timeline.last().unwrap().clone();
            invalid[PLAYER_B_STATE] = PLAYER_STATE_INNEXISTANT;
            invalid[PLAYER_B_STOCKS] = 255;
            timeline.extend([invalid.clone(), invalid.clone()]);
            invalid[GLOBAL_GAME_STATE] = GAME_STATE_GAMEOVER;
            invalid[GAME_WINNER] = 0;
            invalid[PLAYER_A_STOCKS] = 103;
            invalid[PLAYER_B_STOCKS] = 89;
            timeline.push(invalid.clone());
            invalid[GAME_WINNER] = 1;
            timeline.push(invalid);
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
                fail_drop: false,
                skip_replay: false,
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
        type Portable = FakeState;
        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            Ok(self.save(self.state.clone()))
        }
        fn drop_snapshot(&mut self, id: SnapId) -> Result<(), MachineError> {
            if std::mem::take(&mut self.fail_drop) {
                return Err(MachineError::Backend("injected drop failure".into()));
            }
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
            if std::mem::take(&mut self.skip_replay) {
                return Ok(());
            }
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
                    self.state.wram[PLAYER_A_X] = action.buttons;
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
        fn export(&mut self, id: SnapId, _: Option<&FakeState>) -> Result<FakeState, MachineError> {
            self.snapshots
                .get(&id.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)
        }
        fn import(&mut self, state: &FakeState) -> Result<SnapId, MachineError> {
            Ok(self.save(state.clone()))
        }
        fn portable_memory_charge(state: &FakeState) -> usize {
            state.wram.len() + size_of::<usize>()
        }
        fn now(&self) -> machine::Moment {
            machine::Moment(self.clock)
        }
        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    #[test]
    fn interior_terminal_has_one_aligned_endpoint_and_final_stock_loss() {
        let mut target = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        target.apply(&ButtonChord::new(1, 10));
        let observed = target.observe();
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(observed.frame_count, 7);
        assert!(observed.decoded.player_a_won());
        assert_eq!(observed.decoded.gameplay, None);
        assert_eq!(
            (observed.player_a_ko_count, observed.player_b_ko_count),
            (4, 5)
        );
        assert!(observed.player_b_ko);
        assert!(
            target
                .last_action_observations()
                .iter()
                .any(|o| o.decoded.gameplay.is_none() && !o.terminal)
        );
        let snapshot = target.snapshot().unwrap();
        assert_eq!(snapshot.emulator_state.cursor, 7);
        assert_eq!(snapshot.emulator_state.wram, snapshot.wram);
        assert_eq!(snapshot.wram, target.machine.state.wram);
        assert_eq!(snapshot.observation, observed);
        target.apply(&ButtonChord::new(2, 3));
        assert_eq!(target.observe(), observed);
    }

    #[test]
    fn restore_across_invalid_phase_and_another_worker_preserves_stock_evidence() {
        let mut target = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        target.apply(&ButtonChord::new(1, 5));
        let first_work = target.execution_work();
        assert!(first_work > 0);
        let saved = target.snapshot().unwrap();
        assert_eq!(saved.observation.decoded.gameplay, None);
        assert_eq!(saved.player_b_ko_count, 4);
        let continuation = ButtonChord::new(2, 2);
        target.apply(&continuation);
        let expected = target.observe();
        let expected_events = target.last_action_observations().to_vec();
        let second_work = target.execution_work();
        assert!(second_work > first_work);
        target.restore(&saved).unwrap();
        assert_eq!(target.execution_work(), second_work);
        target.apply(&ButtonChord::new(4, 1));
        let discarded_work = target.execution_work();
        assert!(discarded_work > second_work);
        target.restore(&saved).unwrap();
        assert_eq!(target.execution_work(), discarded_work);
        target.apply(&continuation);
        let final_work = target.execution_work();
        assert!(final_work > discarded_work);
        assert_eq!(target.observe(), expected);
        assert_eq!(target.last_action_observations(), expected_events);
        assert_eq!(expected.player_b_ko_count, 5);
        let mut other = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        other.restore(&saved).unwrap();
        other.apply(&continuation);
        assert_eq!(other.observe(), expected);
        assert_eq!(other.fingerprint(), target.fingerprint());
        target.reset();
        assert_eq!(target.execution_work(), final_work);
        target.apply(&ButtonChord::new(1, 5));
        assert!(target.execution_work() > discarded_work);
        target.reset();
        assert_eq!(target.observe().frame_count, 0);
        assert_eq!(target.observe().player_b_ko_count, 0);
        assert_eq!(target.machine.snapshots.len(), 1);
    }

    #[test]
    fn adverse_probe_checks_the_future_and_restores_live_machine_state() {
        let mut target = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        let state = target.machine.state.clone();
        assert!(!target.survives_probe(0, 8));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.machine.state, state);
        assert!(target.machine.staged.is_empty());
        assert_eq!(target.observe().player_b_ko_count, 0);
        target.apply(&ButtonChord::new(1, 2));
        let mut baseline = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        baseline.apply(&ButtonChord::new(1, 2));
        assert_eq!(target.observe(), baseline.observe());
    }

    #[test]
    fn probe_rejects_a_backend_that_claims_restore_without_restoring_ram() {
        let mut target = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        target.machine.skip_replay = true;
        assert!(!target.survives_probe(0, 2));
        assert_eq!(target.exit_kind(), ExitKind::Crash);
    }

    #[test]
    fn failed_handle_release_during_restore_poison_target() {
        let mut target = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        target.apply(&ButtonChord::new(1, 1));
        let saved = target.snapshot().unwrap();
        target.apply(&ButtonChord::new(2, 1));
        target.machine.fail_drop = true;
        assert!(target.restore(&saved).is_err());
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert!(target.snapshot().is_none());
    }

    #[test]
    fn requested_ai_is_validated_and_snapshots_cannot_cross_difficulties() {
        let mut easy = StbTarget::from_machine(ScriptedMachine::match_timeline()).unwrap();
        let saved = easy.snapshot().unwrap();
        for ai in [StbAi::Fair, StbAi::Hard] {
            let mut machine = ScriptedMachine::match_timeline();
            assert!(StbTarget::from_machine_with_ai(machine, ai).is_err());
            machine = ScriptedMachine::match_timeline();
            machine.state.wram[CONFIG_AI_LEVEL] = ai.level();
            let mut target = StbTarget::from_machine_with_ai(machine, ai).unwrap();
            assert_eq!(target.mechanical_state().ai_level, ai.level());
            assert!(target.restore(&saved).is_err());
            assert_eq!(target.exit_kind(), ExitKind::Ok);
            assert_eq!(target.machine.state.wram[CONFIG_AI_LEVEL], ai.level());
        }
    }

    #[test]
    fn genesis_rejects_a_different_stage_or_initial_stock_configuration() {
        for (address, value) in [
            (CONFIG_SELECTED_STAGE, 1),
            (CONFIG_INITIAL_STOCKS, 3),
            (PLAYER_A_STOCKS, 3),
            (PLAYER_B_STOCKS, 3),
        ] {
            let mut machine = ScriptedMachine::match_timeline();
            machine.state.wram[address] = value;
            assert!(
                StbTarget::from_machine(machine).is_err(),
                "accepted address {address:#x}={value}"
            );
        }
    }

    #[test]
    fn source_coordinates_keep_signed_screen_component() {
        assert_eq!(signed_world_pixels(0x10, 0), 0x10);
        assert_eq!(signed_world_pixels(0xf0, 0xff), -16);
    }

    #[test]
    fn decoder_requires_the_complete_wram_window() {
        assert!(decode_state(&[0; 64]).is_err());
    }

    #[test]
    fn match_over_disambiguates_player_zero_winner() {
        let state = StbMechanicalState {
            game_winner: 0,
            game_state: GAME_STATE_GAMEOVER,
            ..StbMechanicalState::default()
        };
        assert!(state.player_a_won());
    }

    #[test]
    fn decoder_hides_fighter_ram_after_gameplay_phase() {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GLOBAL_GAME_STATE] = GAME_STATE_GAMEOVER;
        wram[PLAYER_A_STATE] = PLAYER_STATE_INNEXISTANT;
        wram[PLAYER_B_STATE] = PLAYER_STATE_INNEXISTANT;
        wram[PLAYER_A_DAMAGE] = 103;
        wram[PLAYER_B_DAMAGE] = 89;
        wram[PLAYER_A_STOCKS] = 103;
        wram[PLAYER_B_STOCKS] = 89;
        wram[GAME_WINNER] = 0;
        let state = decode_state(&wram).expect("complete game-over RAM");
        assert_eq!(state.gameplay, None);
        assert!(state.player_a_won());
    }

    #[test]
    fn decoder_hides_fighter_ram_when_one_player_is_invalid() {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GLOBAL_GAME_STATE] = GAME_STATE_INGAME;
        wram[PLAYER_A_STATE] = PLAYER_STATE_INNEXISTANT;
        wram[PLAYER_B_STATE] = 5;
        let state = decode_state(&wram).expect("complete transition RAM");
        assert_eq!(state.gameplay, None);
    }
}
