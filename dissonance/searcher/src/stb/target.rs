// SPDX-License-Identifier: AGPL-3.0-or-later

//! Super Tilt Bro memory decoder and machine-backed target.
//!
//! The source-built game keeps both fighters in system RAM. This module is
//! the only place that knows those labels; the generic searcher receives a
//! bounded controller vocabulary, compact mechanical observations, and
//! restorable snapshots.

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

use crate::target::{ExitKind, Target};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

/// A Super Tilt Bro input replayed from the sealed local-match genesis.
pub type StbInput = crate::search::archive::Input<ButtonChord>;

const GAME_STATE_INGAME: u8 = 0x00;
const GAME_STATE_GAMEOVER: u8 = 0x02;
const GAME_MODE_LOCAL: u8 = 0x00;
const PLAYER_STATE_INNEXISTANT: u8 = 0x02;

// Labels from game/mem_labels.asm at the pinned upstream revision.
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
const CONFIG_AI_LEVEL: usize = 0xda;
const CONFIG_SELECTED_STAGE: usize = 0xdb;
const CONFIG_GAME_MODE: usize = 0xe2;

/// Player RAM that is meaningful only while both fighters are active in the
/// in-game state. Keeping this payload optional prevents menu/game-over RAM
/// reuse from becoming a fabricated damage, stock, location, or capability
/// observation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbGameplayState {
    /// Player state-machine values.
    pub player_a_state: u8,
    pub player_b_state: u8,
    /// Signed world coordinates in whole pixels. The source stores each as
    /// a pixel byte plus a signed screen/page byte.
    pub player_a_x: i16,
    pub player_b_x: i16,
    pub player_a_y: i16,
    pub player_b_y: i16,
    /// Screen/page components distinguish scroll from a wrapped pixel byte.
    pub player_a_x_screen: i8,
    pub player_b_x_screen: i8,
    pub player_a_y_screen: i8,
    pub player_b_y_screen: i8,
    /// Facing direction bytes from the source state.
    pub player_a_direction: u8,
    pub player_b_direction: u8,
    /// Damage percentages and remaining stocks.
    pub player_a_damage: u8,
    pub player_b_damage: u8,
    pub player_a_stocks: u8,
    pub player_b_stocks: u8,
    /// State-machine clocks and hitstun counters.
    pub player_a_state_clock: u8,
    pub player_b_state_clock: u8,
    pub player_a_hitstun: u8,
    pub player_b_hitstun: u8,
    /// Mechanical contact flags (zero means no contact).
    pub player_a_grounded: bool,
    pub player_b_grounded: bool,
    pub player_a_walled: bool,
    pub player_b_walled: bool,
}

/// Source-grounded mechanical state at one emulator frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbMechanicalState {
    /// Global state (`0` is in-game and `2` is the game-over screen).
    pub game_state: u8,
    /// Configured game mode (`0` is local).
    pub game_mode: u8,
    /// Configured autonomous opponent level (`1` is Easy in the config UI).
    pub ai_level: u8,
    /// Selected versus stage index.
    pub stage: u8,
    /// Player RAM decoded only in the source phase where it has gameplay
    /// meaning. This is `None` on menus and game-over screens.
    pub gameplay: Option<StbGameplayState>,
    /// Winner byte is meaningful only once `game_state == 2`.
    pub game_winner: u8,
}

impl StbMechanicalState {
    /// Whether the source has entered the game-over screen.
    #[must_use]
    pub fn match_over(self) -> bool {
        self.game_state == GAME_STATE_GAMEOVER
    }

    /// Whether player A won the local match.
    #[must_use]
    pub fn player_a_won(self) -> bool {
        self.match_over() && self.game_winner == 0
    }

    /// Whether the optional player payload is valid for gameplay use.
    #[must_use]
    pub fn gameplay_valid(self) -> bool {
        self.gameplay.is_some()
    }
}

/// Mechanical evidence emitted at a changed state boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbObservations {
    pub frame_count: u64,
    pub decoded: StbMechanicalState,
    pub changed_indices: Vec<u16>,
    /// Whether player A or B lost one stock at this event.
    #[serde(default)]
    pub player_a_ko: bool,
    #[serde(default)]
    pub player_b_ko: bool,
    /// Cumulative validated stock-loss counts at this observation. The
    /// terminal underflow loss is included even though the source resets its
    /// terminal stock byte to zero before entering the game-over screen.
    #[serde(default)]
    pub player_a_ko_count: u8,
    #[serde(default)]
    pub player_b_ko_count: u8,
    /// Whether this event is the terminal match state.
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

/// Geometry and frame count of one rendered replay.
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

/// Complete state needed to resume one STB prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbSnapshot<P = machine::SharedState> {
    pub(crate) emulator_state: P,
    pub(crate) observation: StbObservations,
    pub(crate) wram: Vec<u8>,
    pub(crate) failed: bool,
    /// Last phase-valid gameplay payload used to carry stock evidence across
    /// the source's invalid fighter-RAM transition into game over.
    #[serde(default)]
    pub(crate) last_valid_gameplay: Option<StbGameplayState>,
    /// Cumulative stock-loss evidence at this exact snapshot endpoint.
    #[serde(default)]
    pub(crate) player_a_ko_count: u8,
    #[serde(default)]
    pub(crate) player_b_ko_count: u8,
}

impl<P> StbSnapshot<P> {
    /// Decoded endpoint state carried by this snapshot.
    #[must_use]
    pub fn state(&self) -> StbMechanicalState {
        self.observation.decoded
    }
}

/// Machine-backed target used by STB campaigns.
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
}

impl<M: Machine> StbTarget<M> {
    /// Seal a machine that is already stopped at a valid local-match genesis.
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
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
        })
    }

    /// Current decoded state.
    #[must_use]
    pub fn mechanical_state(&self) -> StbMechanicalState {
        self.observation.decoded
    }

    /// Whether the local match is over (win or loss).
    #[must_use]
    pub fn is_match_over(&self) -> bool {
        self.observation.decoded.match_over()
    }

    /// Whether player A won after the game-over transition.
    #[must_use]
    pub fn player_a_won(&self) -> bool {
        self.observation.decoded.player_a_won()
    }

    /// Total deterministic frames clocked by this instance.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    /// Observer events emitted by the most recent action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[StbObservations] {
        &self.action_observations
    }

    /// Test a fixed continuation and restore the caller's state afterward.
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
        if self.machine.replay(current).is_err() {
            self.failed = true;
            return false;
        }
        survived
    }
}

impl StbTarget<QuickNesMachine> {
    /// Load the pinned UNROM image and walk ordinary title/mode/config/
    /// character/stage menus to the local AI match genesis.
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
                "Super Tilt Bro setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    /// Replay a searched input while writing packed RGB24 frames and S16LE
    /// stereo audio. Capture is never enabled by workers.
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
            let mut tail_terminal = endpoint.match_over();
            while remaining > 0 && !tail_terminal {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                self.render_action(
                    ButtonChord::new(0, u8::try_from(hold)?),
                    video_output,
                    audio_output,
                    &mut metadata,
                )?;
                remaining -= hold;
                tail_terminal = decode_state(&read_wram(&self.machine)?)?.match_over();
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
            if decode_state(&read_wram(&self.machine)?)?.match_over() {
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
                // Keep the last valid live payload while the source clears
                // fighter RAM for one or more frames before setting the
                // global game-over state. A live zero-stock frame is valid;
                // the terminal underflow is the next loss event.
                let (player_a_ko, player_b_ko) = if let Some(gameplay) = state.gameplay {
                    let player_a_ko = last_valid_gameplay
                        .is_some_and(|prior| gameplay.player_a_stocks < prior.player_a_stocks);
                    let player_b_ko = last_valid_gameplay
                        .is_some_and(|prior| gameplay.player_b_stocks < prior.player_b_stocks);
                    player_a_ko_count =
                        player_a_ko_count.max(4_u8.saturating_sub(gameplay.player_a_stocks));
                    player_b_ko_count =
                        player_b_ko_count.max(4_u8.saturating_sub(gameplay.player_b_stocks));
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
                    || preference_tuple(state) != preference_tuple(prior_state)
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
        // A terminal frame can occur before a held action's requested end.
        // Re-run only the exact prefix through that frame before taking the
        // current snapshot. This keeps the emulator handle, decoded endpoint,
        // and recorded action prefix on one executed frame.
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
        let imported = self
            .machine
            .import(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.machine.replay(imported) {
            let _ = self.machine.drop_snapshot(imported);
            return Err(error.to_string().into());
        }
        if self.current != self.genesis
            && let Err(error) = self.machine.drop_snapshot(self.current)
        {
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

/// Ordinary controller tape from power-on to the selected local-AI match.
///
/// The probe binary exposes this tape's state boundaries so a changed ROM or
/// emulator backend cannot silently turn a menu input into gameplay genesis.
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let press_release = |tape: &mut Vec<ButtonChord>, button| {
        tape.push(ButtonChord::new(button, 1));
        tape.push(ButtonChord::new(0, 1));
    };
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    // The title animation and each screen transition can consume frames while
    // rendering is disabled. Keep a generous fixed settle interval so an A
    // edge is never delivered to a transition initializer rather than the
    // intended menu. Local is the default mode and the following screens keep
    // four stocks and Easy AI unchanged.
    wait(&mut tape, 180);
    // Title -> mode selection.
    // ButtonChord uses the NES serial/input layout consumed by QuickNES:
    // A=0x01, B=0x02, Select=0x04, Start=0x08, Up=0x10, Down=0x20,
    // Left=0x40, Right=0x80. STB's fetched RAM byte is bit-reversed, so
    // this A input appears as source CONTROLLER_BTN_A ($80) in RAM.
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    // Mode selection -> config.
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    // Config -> character selection.
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    // One-player character selection: P1 ready, then P2 ready through the
    // game's own one-controller flow. Touching controller B would disable AI.
    press_release(&mut tape, 0x01);
    wait(&mut tape, 180);
    press_release(&mut tape, 0x01);
    wait(&mut tape, 240);
    // Stage selection -> local match.
    press_release(&mut tape, 0x01);
    // Let the spawn/countdown settle, then seal before the autonomous match
    // can consume a stock while waiting in the menu setup.
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

/// Decode the source-labelled STB state from the 2 KiB QuickNES system-RAM
/// window.
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

fn validate_genesis(state: StbMechanicalState) -> Result<(), MachineError> {
    let Some(gameplay) = state.gameplay else {
        return Err(MachineError::Backend(format!(
            "Super Tilt Bro setup did not reach live local AI match: state={} mode={} ai={} gameplay=none",
            state.game_state, state.game_mode, state.ai_level,
        )));
    };
    if state.game_state != GAME_STATE_INGAME
        || state.game_mode != GAME_MODE_LOCAL
        || state.ai_level != 1
        || gameplay.player_a_stocks == 0
        || gameplay.player_b_stocks == 0
    {
        return Err(MachineError::Backend(format!(
            "Super Tilt Bro setup did not reach live local AI match: state={} mode={} ai={} player_states=({}, {}) stocks=({}, {})",
            state.game_state,
            state.game_mode,
            state.ai_level,
            gameplay.player_a_state,
            gameplay.player_b_state,
            gameplay.player_a_stocks,
            gameplay.player_b_stocks,
        )));
    }
    Ok(())
}

/// Coarse paired fighter location. The signed world coordinate keeps camera
/// scroll and off-screen movement distinct from a wrapped low byte.
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

/// Same-location preference: retain damage/stocks/capability information
/// without multiplying archive locations by every volatile field.
pub type StbPreference = (u8, u8, u8, u8, u8, u8, bool, bool);

#[must_use]
pub fn preference_tuple(state: StbMechanicalState) -> Option<StbPreference> {
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
