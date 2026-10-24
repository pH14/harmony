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

pub type JoustInput = crate::search::archive::Input<ButtonChord>;

pub const TARGET_WAVE: u8 = 26;
pub const START_LIVES: u8 = 4;

const GAME_MODE: usize = 0x00;
const GAME_PHASE: usize = 0x02;
const PLAYER_Y: usize = 0x1e;
const PLAYER_X: usize = 0x1f;
const WAVE_ONES: usize = 0x3a;
const WAVE_TENS: usize = 0x3b;
const VELOCITY_X: usize = 0x4e;
const VELOCITY_Y: usize = 0x52;
const WAVE_ENEMIES: usize = 0x73;
const WAVE_SPAWNED: usize = 0x74;
const WAVE_DEFEATED: usize = 0x75;
const LIVES: usize = 0xe9;
const SCORE_LOW: usize = 0xeb;
const SCORE_MID: usize = 0xec;
const SCORE_HIGH: usize = 0xed;
const OAM: usize = 0x200;
const OAM_SPRITES: usize = 64;
const PLAYING_MODE: u8 = 0x90;
const PLAYING_PHASE: u8 = 0x02;
const GAME_OVER_MODE: u8 = 0x80;
const GAME_OVER_PHASE: u8 = 0x03;
const PLAY_AREA_TOP: u8 = 0x10;
const PLAY_AREA_BOTTOM: u8 = 0xc0;
const RIDER_PALETTE: u8 = 2;
const PLAYER_PALETTE: u8 = 0;
const EGG_PALETTE: u8 = 3;
const EGG_TILE: u8 = 204;
const SPRITES_PER_RIDER: u8 = 4;
const FLIP_HORIZONTAL: u8 = 0x40;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct JoustMechanicalState {
    pub wave: u8,
    pub lives: u8,
    pub score: u32,
    pub x: u8,
    pub y: u8,
    pub velocity_x: i8,
    pub velocity_y: i8,
    pub enemies: u8,
    pub spawned: u8,
    pub defeated: u8,
    pub riders: u8,
    pub eggs: u8,
    pub facing_left: bool,
    pub in_game: bool,
    pub game_over: bool,
}

impl JoustMechanicalState {
    #[must_use]
    pub fn waves_cleared(self) -> u8 {
        self.wave.saturating_sub(1)
    }

    #[must_use]
    pub fn remaining(self) -> u8 {
        self.enemies.saturating_sub(self.defeated)
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.game_over
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.in_game && !self.game_over && self.wave >= TARGET_WAVE
    }

    #[must_use]
    pub fn playing(self) -> bool {
        self.in_game && !self.game_over && self.wave > 0
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustObservations {
    pub frame_count: u64,
    pub decoded: JoustMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: JoustMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: JoustObservations,
    pub failed: bool,
}

impl JoustSnapshot {
    #[must_use]
    pub fn state(&self) -> JoustMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct JoustTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: JoustObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: JoustObservations,
    action_observations: Vec<JoustObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> JoustTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = JoustObservations {
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
                "Joust setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> JoustMechanicalState {
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
    pub fn last_action_observations(&self) -> &[JoustObservations] {
        &self.action_observations
    }

    pub fn snapshot_of_state(
        &mut self,
        state: &Vec<u8>,
        frame_count: u64,
    ) -> Result<JoustSnapshot, Box<dyn Error>> {
        self.machine
            .restore_nes(state)
            .map_err(|error| error.to_string())?;
        let wram = read_wram(&self.machine)?;
        let decoded = decode_state(&wram)?;
        let next = self.machine.snapshot()?;
        if self.current != self.genesis {
            self.machine.drop_snapshot(self.current)?;
        }
        self.current = next;
        self.current_wram = wram;
        self.failed = false;
        self.snapshot_base = None;
        self.observation = Self::make_observation(frame_count, decoded);
        self.action_observations = vec![self.observation.clone()];
        self.snapshot()
            .ok_or_else(|| "failed to snapshot the restored state".into())
    }

    fn make_observation(frame_count: u64, state: JoustMechanicalState) -> JoustObservations {
        JoustObservations {
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
            .position(|wram| decode_state(wram).is_ok_and(JoustMechanicalState::terminal));
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
impl JoustTarget<QuickNesMachine> {
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
        input: &JoustInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<JoustVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Joust gameplay genesis for film".into());
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
        metadata: &mut Option<JoustVideoMetadata>,
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
                    *metadata = Some(JoustVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: JoustMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for JoustTarget<M> {
    type Action = ButtonChord;
    type Observations = JoustObservations;
    type Snapshot = JoustSnapshot;

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
        (u64::from(state.wave) << 56)
            | (u64::from(state.lives) << 48)
            | (u64::from(state.defeated) << 40)
            | (u64::from(state.x) << 32)
            | (u64::from(state.y) << 24)
            | (u64::from(state.velocity_x.cast_unsigned()) << 16)
            | (u64::from(state.velocity_y.cast_unsigned()) << 8)
            | u64::from(state.eggs)
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
        Some(JoustSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for JoustTarget<M> {
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
    wait(&mut tape, 200);
    tape.push(ButtonChord::new(START, 3));
    wait(&mut tape, 60);
    tape.push(ButtonChord::new(START, 3));
    wait(&mut tape, 200);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Joust RAM address {address:#x} is absent")))
}

fn bcd(value: u8) -> u32 {
    u32::from(value >> 4).min(9) * 10 + u32::from(value & 0x0f).min(9)
}

pub fn decode_state(wram: &[u8]) -> Result<JoustMechanicalState, MachineError> {
    let sprites = wram
        .get(OAM..OAM + OAM_SPRITES * 4)
        .ok_or_else(|| MachineError::Backend("Joust sprite memory is absent".to_owned()))?;
    let mut rider_sprites = 0u8;
    let mut eggs = 0u8;
    let mut facing_left = false;
    for sprite in sprites.chunks_exact(4) {
        let (y, tile, attributes) = (sprite[0], sprite[1], sprite[2]);
        if !(PLAY_AREA_TOP..PLAY_AREA_BOTTOM).contains(&y) {
            continue;
        }
        match attributes & 3 {
            RIDER_PALETTE => rider_sprites = rider_sprites.saturating_add(1),
            EGG_PALETTE if tile == EGG_TILE => eggs = eggs.saturating_add(1),
            PLAYER_PALETTE => facing_left = attributes & FLIP_HORIZONTAL != 0,
            _ => {}
        }
    }
    let tens = byte(wram, WAVE_TENS)?;
    let ones = byte(wram, WAVE_ONES)?;
    let wave = if tens > 9 || ones > 9 {
        0
    } else {
        tens * 10 + ones
    };
    Ok(JoustMechanicalState {
        wave,
        lives: byte(wram, LIVES)?,
        score: bcd(byte(wram, SCORE_HIGH)?) * 10_000
            + bcd(byte(wram, SCORE_MID)?) * 100
            + bcd(byte(wram, SCORE_LOW)?),
        x: byte(wram, PLAYER_X)?,
        y: byte(wram, PLAYER_Y)?,
        velocity_x: i8::from_ne_bytes([byte(wram, VELOCITY_X)?]),
        velocity_y: i8::from_ne_bytes([byte(wram, VELOCITY_Y)?]),
        enemies: byte(wram, WAVE_ENEMIES)?,
        spawned: byte(wram, WAVE_SPAWNED)?,
        defeated: byte(wram, WAVE_DEFEATED)?,
        riders: rider_sprites.div_ceil(SPRITES_PER_RIDER),
        eggs,
        facing_left,
        in_game: byte(wram, GAME_MODE)? == PLAYING_MODE && byte(wram, GAME_PHASE)? == PLAYING_PHASE,
        game_over: byte(wram, GAME_MODE)? == GAME_OVER_MODE
            && byte(wram, GAME_PHASE)? == GAME_OVER_PHASE,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine
        .read(0, WRAM_SIZE as u32)?
        .try_into()
        .map_err(|_| MachineError::Backend("Joust system RAM window has invalid length".to_owned()))
}

fn validate_genesis(state: JoustMechanicalState) -> Result<(), MachineError> {
    let valid = state.in_game
        && !state.game_over
        && state.wave == 1
        && state.lives == START_LIVES
        && state.score == 0
        && state.defeated == 0;
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Joust setup did not reach the first wave: {state:?}"
        )))
    }
}

pub const GRID_SHIFT: u8 = 5;

#[must_use]
pub fn boundary_fields(state: JoustMechanicalState) -> [u16; 8] {
    [
        u16::from(state.wave),
        u16::from(state.lives),
        u16::from(state.defeated),
        u16::from(state.x >> GRID_SHIFT),
        u16::from(state.y >> GRID_SHIFT),
        u16::try_from(state.score / 250).unwrap_or(u16::MAX),
        u16::from(state.eggs),
        u16::from(state.game_over),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_MODE] = PLAYING_MODE;
        wram[GAME_PHASE] = PLAYING_PHASE;
        wram[WAVE_ONES] = 1;
        wram[LIVES] = START_LIVES;
        wram[PLAYER_X] = 0x50;
        wram[PLAYER_Y] = 0xac;
        wram
    }

    fn sprite(wram: &mut [u8], index: usize, y: u8, tile: u8, attributes: u8, x: u8) {
        wram[OAM + index * 4..OAM + index * 4 + 4].copy_from_slice(&[y, tile, attributes, x]);
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = JoustSnapshot {
            emulator_state,
            observation: JoustObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<JoustSnapshot>() + snapshot.emulator_state.capacity();
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
    fn the_wave_is_two_decimal_digits_and_the_score_is_three_bcd_bytes() {
        let mut wram = playing_wram();
        wram[WAVE_TENS] = 2;
        wram[WAVE_ONES] = 3;
        wram[SCORE_HIGH] = 0x01;
        wram[SCORE_MID] = 0x16;
        wram[SCORE_LOW] = 0x50;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.wave, 23);
        assert_eq!(state.waves_cleared(), 22);
        assert_eq!(state.score, 11_650);
        wram[WAVE_ONES] = 0x0f;
        assert_eq!(decode_state(&wram).unwrap().wave, 0);
    }

    #[test]
    fn genesis_requires_the_first_wave_with_a_full_reserve_and_no_score() {
        assert!(validate_genesis(decode_state(&playing_wram()).unwrap()).is_ok());
        let mut wram = playing_wram();
        wram[SCORE_MID] = 5;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[GAME_PHASE] = 0;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[WAVE_ONES] = 2;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn the_game_over_screen_is_terminal_and_a_reached_target_wave_is_victory() {
        let mut wram = playing_wram();
        assert!(!decode_state(&wram).unwrap().terminal());
        wram[GAME_MODE] = GAME_OVER_MODE;
        wram[GAME_PHASE] = GAME_OVER_PHASE;
        let over = decode_state(&wram).unwrap();
        assert!(over.dead() && over.terminal() && !over.victory());
        let mut wram = playing_wram();
        wram[WAVE_TENS] = TARGET_WAVE / 10;
        wram[WAVE_ONES] = TARGET_WAVE % 10;
        let won = decode_state(&wram).unwrap();
        assert!(won.victory() && won.terminal());
    }

    #[test]
    fn riders_eggs_and_facing_come_from_the_sprite_memory() {
        let mut wram = playing_wram();
        for (index, tile) in (0..4).zip(76..) {
            sprite(
                &mut wram,
                index,
                0x48 + (index as u8 / 2) * 8,
                tile,
                0x22,
                0x70,
            );
        }
        for (index, tile) in (4..8).zip(80..) {
            sprite(
                &mut wram,
                index,
                0x60 + (index as u8 / 2) * 8,
                tile,
                0x62,
                0x90,
            );
        }
        sprite(&mut wram, 8, 0x82, EGG_TILE, 0x23, 0x40);
        sprite(&mut wram, 9, 0xac, 14, 0x60, 0x50);
        sprite(&mut wram, 10, 0xc4, 0xdb, 0x03, 0x68);
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.riders, 2);
        assert_eq!(state.eggs, 1);
        assert!(state.facing_left);
    }

    #[test]
    fn setup_tape_presses_only_start_twice() {
        let tape = setup_tape();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            2
        );
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for JoustSnapshot {
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
        let snapshot = JoustSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: JoustObservations {
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
            SnapshotCheckpoint::<JoustSnapshot>::from_bytes(&bytes, &checkpoint.format).unwrap(),
            checkpoint
        );
        let old =
            postcard::to_allocvec(&("joust-quicknes-snapshot-checkpoint-v0", &checkpoint.entries))
                .unwrap();
        assert!(SnapshotCheckpoint::<JoustSnapshot>::from_bytes(&old, &checkpoint.format).is_err());
    }
}
