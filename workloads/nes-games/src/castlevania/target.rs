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

pub type CastlevaniaInput = crate::search::archive::Input<ButtonChord>;

pub const FINAL_STAGE: u8 = 18;
pub const FULL_HEALTH: u8 = 0x40;
pub const FULL_BOSS_HEALTH: u8 = 0x40;
pub const SECOND_FORM_PENDING_DAMAGE: u8 = 0x48;
pub const SECOND_FORM_DAMAGE_BASE: u8 = 0x50;
pub const BOSSES: [(u8, &str); 6] = [
    (3, "phantom_bat"),
    (6, "medusa"),
    (9, "mummies"),
    (12, "frankenstein"),
    (15, "death"),
    (18, "dracula"),
];

#[cfg(test)]
const MODE_TITLE: u8 = 0x01;
#[cfg(test)]
const MODE_INTRO: u8 = 0x04;
const MODE_PLAYING: u8 = 0x05;
const MODE_DYING: u8 = 0x06;
const MODE_GAME_OVER_FADE: u8 = 0x07;
#[cfg(test)]
const MODE_STAGE_EXIT: u8 = 0x08;
#[cfg(test)]
const MODE_DOOR: u8 = 0x0a;
const MODE_STAGE_CLEAR: u8 = 0x0c;
const MODE_GAME_OVER: u8 = 0x0d;
const MODE_ENDING: u8 = 0x0f;
const PLAYING_CONTROL: u8 = 0x06;
const MAP_STAGE: u8 = 0x14;
const FELL_Y: u8 = 0xe0;
const GENESIS_LIVES: u8 = 4;

const MODE: usize = 0x18;
const SUBMODE: usize = 0x19;
const STAGE: usize = 0x28;
const NEXT_STAGE: usize = 0x29;
const LIVES: usize = 0x2a;
const Y: usize = 0x3f;
const X_LOW: usize = 0x40;
const X_HIGH: usize = 0x41;
const HEALTH: usize = 0x45;
const SECTION: usize = 0x46;
const INVINCIBLE: usize = 0x5b;
const MULTIPLIER: usize = 0x64;
const DRACULA_FORM: usize = 0x7a;
const WHIP: usize = 0x70;
const HEARTS: usize = 0x71;
const SUBWEAPON: usize = 0x15b;
const BOSS_HEALTH: usize = 0x1a9;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CastlevaniaMechanicalState {
    pub mode: u8,
    pub submode: u8,
    pub stage: u8,
    pub section: u8,
    pub lives: u8,
    pub health: u8,
    pub x: u16,
    pub y: u8,
    pub invincible: u8,
    pub hearts: u8,
    pub whip: u8,
    pub subweapon: u8,
    pub multiplier: u8,
    pub boss_health: u8,
    pub second_form: bool,
}

impl CastlevaniaMechanicalState {
    #[must_use]
    pub fn playing(self) -> bool {
        self.mode == MODE_PLAYING && self.submode == PLAYING_CONTROL && self.health > 0
    }

    #[must_use]
    pub fn dead(self) -> bool {
        matches!(self.mode, MODE_DYING | MODE_GAME_OVER_FADE | MODE_GAME_OVER)
            || (self.mode == MODE_PLAYING && (self.health == 0 || self.y >= FELL_Y))
    }

    #[must_use]
    pub fn boss_stage(self) -> Option<&'static str> {
        BOSSES
            .iter()
            .find(|(stage, _)| *stage == self.stage)
            .map(|(_, name)| *name)
    }

    #[must_use]
    pub fn boss_damage(self) -> u8 {
        if self.boss_stage().is_none() || self.mode != MODE_PLAYING {
            return 0;
        }
        let damage = FULL_BOSS_HEALTH.saturating_sub(self.boss_health);
        match (self.second_form, self.boss_health) {
            (false, _) => damage,
            (true, 0) => SECOND_FORM_PENDING_DAMAGE,
            (true, _) => SECOND_FORM_DAMAGE_BASE.saturating_add(damage),
        }
    }

    #[must_use]
    pub fn boss_down(self) -> bool {
        self.boss_stage().is_some()
            && self.mode == MODE_STAGE_CLEAR
            && self.boss_health == 0
            && (self.stage != FINAL_STAGE || self.second_form)
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.stage == FINAL_STAGE && self.mode == MODE_ENDING
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }

    #[must_use]
    pub fn hearts_bucket(self) -> u8 {
        match self.hearts {
            0 => 0,
            1..=4 => 1,
            5..=9 => 2,
            _ => 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaObservations {
    pub frame_count: u64,
    pub decoded: CastlevaniaMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: CastlevaniaMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: CastlevaniaObservations,
    pub failed: bool,
}

impl CastlevaniaSnapshot {
    #[must_use]
    pub fn state(&self) -> CastlevaniaMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct CastlevaniaTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: CastlevaniaObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: CastlevaniaObservations,
    action_observations: Vec<CastlevaniaObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> CastlevaniaTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = CastlevaniaObservations {
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
                "Castlevania setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> CastlevaniaMechanicalState {
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
    pub fn last_action_observations(&self) -> &[CastlevaniaObservations] {
        &self.action_observations
    }

    fn make_observation(
        frame_count: u64,
        state: CastlevaniaMechanicalState,
    ) -> CastlevaniaObservations {
        CastlevaniaObservations {
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
                decode_state(wram).is_ok_and(CastlevaniaMechanicalState::terminal)
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
impl CastlevaniaTarget<QuickNesMachine> {
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
        input: &CastlevaniaInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<CastlevaniaVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Castlevania gameplay genesis for film".into());
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
        metadata: &mut Option<CastlevaniaVideoMetadata>,
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
                    *metadata = Some(CastlevaniaVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: CastlevaniaMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for CastlevaniaTarget<M> {
    type Action = ButtonChord;
    type Observations = CastlevaniaObservations;
    type Snapshot = CastlevaniaSnapshot;

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
            | (u64::from(state.stage) << 48)
            | (u64::from(state.section) << 40)
            | (u64::from(state.health) << 32)
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
        let Ok(emulator_state) = self
            .machine
            .export_nes(self.current, self.snapshot_base.as_ref())
        else {
            self.failed = true;
            return None;
        };
        self.snapshot_base = Some(emulator_state.clone());
        Some(CastlevaniaSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for CastlevaniaTarget<M> {
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
    wait(&mut tape, 120);
    tape.push(ButtonChord::new(0x08, 2));
    wait(&mut tape, 600);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Castlevania RAM address {address:#x} is absent"))
    })
}

pub fn decode_state(wram: &[u8]) -> Result<CastlevaniaMechanicalState, MachineError> {
    let stage = match byte(wram, STAGE)? {
        MAP_STAGE => byte(wram, NEXT_STAGE)?,
        stage => stage,
    };
    Ok(CastlevaniaMechanicalState {
        mode: byte(wram, MODE)?,
        submode: byte(wram, SUBMODE)?,
        stage,
        section: byte(wram, SECTION)?,
        lives: byte(wram, LIVES)?,
        health: byte(wram, HEALTH)?,
        x: u16::from(byte(wram, X_HIGH)?) << 8 | u16::from(byte(wram, X_LOW)?),
        y: byte(wram, Y)?,
        invincible: byte(wram, INVINCIBLE)?,
        hearts: byte(wram, HEARTS)?,
        whip: byte(wram, WHIP)?,
        subweapon: byte(wram, SUBWEAPON)?,
        multiplier: byte(wram, MULTIPLIER)?,
        boss_health: byte(wram, BOSS_HEALTH)?,
        second_form: stage == FINAL_STAGE && byte(wram, DRACULA_FORM)? != 0,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Castlevania system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: CastlevaniaMechanicalState) -> Result<(), MachineError> {
    let valid = state.playing()
        && state.stage == 0
        && state.section == 0
        && state.health == FULL_HEALTH
        && state.lives == GENESIS_LIVES;
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Castlevania setup did not reach the first courtyard in play: {state:?}"
        )))
    }
}

#[must_use]
pub fn spatial_bucket(state: CastlevaniaMechanicalState) -> (u16, u8) {
    (state.x / 16, state.y / 16)
}

#[must_use]
pub fn boundary_fields(state: CastlevaniaMechanicalState) -> (u8, u8, u8, u8, u8, u8, u8, u8, u8) {
    (
        state.mode,
        state.submode,
        state.stage,
        state.section,
        state.health,
        state.whip,
        state.subweapon,
        state.hearts_bucket(),
        state.boss_damage() / 8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[MODE] = MODE_PLAYING;
        wram[SUBMODE] = PLAYING_CONTROL;
        wram[LIVES] = GENESIS_LIVES;
        wram[HEALTH] = FULL_HEALTH;
        wram[HEARTS] = 5;
        wram[X_LOW] = 0x30;
        wram[Y] = 0xa0;
        wram[BOSS_HEALTH] = FULL_BOSS_HEALTH;
        wram
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = CastlevaniaSnapshot {
            emulator_state,
            observation: CastlevaniaObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<CastlevaniaSnapshot>() + snapshot.emulator_state.capacity();
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
    fn world_x_joins_the_high_and_low_bytes() {
        let mut wram = playing_wram();
        wram[X_HIGH] = 2;
        wram[X_LOW] = 0x80;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.x, 0x280);
        assert!(state.playing());
        assert!(validate_genesis(state).is_ok());
    }

    #[test]
    fn zero_health_and_the_dying_mode_are_deaths() {
        let mut wram = playing_wram();
        wram[HEALTH] = 0;
        assert!(decode_state(&wram).unwrap().dead());
        let mut wram = playing_wram();
        wram[MODE] = MODE_DYING;
        let state = decode_state(&wram).unwrap();
        assert!(state.dead());
        assert!(state.terminal());
        assert!(!state.victory());
    }

    #[test]
    fn a_door_transition_is_neither_play_nor_terminal() {
        let mut wram = playing_wram();
        wram[MODE] = MODE_DOOR;
        let state = decode_state(&wram).unwrap();
        assert!(!state.playing());
        assert!(!state.terminal());
    }

    #[test]
    fn falling_below_the_screen_is_a_death_before_the_dying_mode() {
        let mut wram = playing_wram();
        wram[Y] = 0xc8;
        assert!(!decode_state(&wram).unwrap().dead());
        wram[Y] = FELL_Y;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.health, FULL_HEALTH);
        assert!(state.dead());
        assert!(state.terminal());
    }

    #[test]
    fn boss_damage_counts_only_in_boss_stages_during_play() {
        let mut wram = playing_wram();
        wram[STAGE] = 2;
        wram[BOSS_HEALTH] = 0x10;
        assert_eq!(decode_state(&wram).unwrap().boss_damage(), 0);
        wram[STAGE] = 3;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.boss_damage(), 0x30);
        assert_eq!(state.boss_stage(), Some("phantom_bat"));
        wram[SUBMODE] = 0;
        wram[MODE] = MODE_STAGE_CLEAR;
        assert_eq!(decode_state(&wram).unwrap().boss_damage(), 0);
    }

    #[test]
    fn a_boss_is_down_only_once_its_crystal_starts_the_stage_clear() {
        let mut wram = playing_wram();
        wram[STAGE] = 9;
        wram[BOSS_HEALTH] = 0;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.boss_stage(), Some("mummies"));
        assert_eq!(state.boss_damage(), FULL_BOSS_HEALTH);
        assert!(!state.boss_down());
        assert!(!state.terminal());
        wram[MODE] = MODE_STAGE_CLEAR;
        wram[SUBMODE] = 0;
        let state = decode_state(&wram).unwrap();
        assert!(state.boss_down());
        assert!(!state.terminal());
        wram[STAGE] = 8;
        assert!(!decode_state(&wram).unwrap().boss_down());
    }

    #[test]
    fn the_castle_map_reads_the_next_stage() {
        let mut wram = playing_wram();
        wram[MODE] = MODE_STAGE_CLEAR;
        wram[SUBMODE] = 7;
        wram[STAGE] = MAP_STAGE;
        wram[NEXT_STAGE] = 4;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.stage, 4);
        assert!(!state.boss_down());
        assert!(!state.terminal());
    }

    #[test]
    fn dracula_damage_rises_through_both_forms() {
        let dracula = |form: u8, meter: u8| {
            let mut wram = playing_wram();
            wram[STAGE] = FINAL_STAGE;
            wram[DRACULA_FORM] = form;
            wram[BOSS_HEALTH] = meter;
            decode_state(&wram).unwrap()
        };
        let fresh_first = dracula(0, FULL_BOSS_HEALTH);
        let hurt_first = dracula(0, 0x20);
        let dying_first = dracula(0, 0);
        let transient = dracula(1, 0);
        let fresh_second = dracula(1, FULL_BOSS_HEALTH);
        let hurt_second = dracula(1, 0x20);
        let nearly_drained = dracula(1, 1);
        assert!(!fresh_first.second_form && fresh_first.second_form == dying_first.second_form);
        assert!(transient.second_form && fresh_second.second_form);
        assert!(!dying_first.terminal() && !nearly_drained.terminal());
        let route = [
            fresh_first,
            hurt_first,
            dying_first,
            transient,
            fresh_second,
            hurt_second,
            nearly_drained,
        ];
        let damage: Vec<u8> = route.iter().map(|state| state.boss_damage()).collect();
        assert!(
            damage.windows(2).all(|pair| pair[0] < pair[1]),
            "{damage:?}"
        );
        let buckets: Vec<u8> = damage.iter().map(|value| value / 8).collect();
        assert!(
            buckets.windows(2).all(|pair| pair[0] < pair[1]),
            "{buckets:?}"
        );
        let mut wram = playing_wram();
        wram[STAGE] = 15;
        wram[DRACULA_FORM] = 1;
        assert!(!decode_state(&wram).unwrap().second_form);
    }

    #[test]
    fn only_the_ending_mode_in_the_final_stage_is_victory() {
        let mut wram = playing_wram();
        wram[STAGE] = FINAL_STAGE;
        assert!(!decode_state(&wram).unwrap().victory());
        wram[MODE] = MODE_DOOR;
        assert!(!decode_state(&wram).unwrap().victory());
        wram[MODE] = MODE_STAGE_EXIT;
        assert!(!decode_state(&wram).unwrap().victory());
        wram[MODE] = MODE_STAGE_CLEAR;
        wram[BOSS_HEALTH] = 0;
        wram[DRACULA_FORM] = 1;
        let crystal = decode_state(&wram).unwrap();
        assert!(crystal.boss_down());
        assert!(!crystal.victory());
        assert!(!crystal.terminal());
        wram[MODE] = MODE_ENDING;
        let ending = decode_state(&wram).unwrap();
        assert!(ending.victory());
        assert!(ending.terminal());
        assert!(!ending.boss_down());
        wram[STAGE] = 17;
        assert!(!decode_state(&wram).unwrap().victory());
    }

    #[test]
    fn entering_the_final_stage_is_neither_victory_nor_a_defeated_dracula() {
        let mut wram = playing_wram();
        wram[STAGE] = FINAL_STAGE;
        wram[BOSS_HEALTH] = FULL_BOSS_HEALTH;
        for (mode, submode) in [(MODE_STAGE_EXIT, 0), (MODE_STAGE_CLEAR, 8)] {
            wram[MODE] = mode;
            wram[SUBMODE] = submode;
            let state = decode_state(&wram).unwrap();
            assert!(!state.victory());
            assert!(!state.boss_down());
            assert!(!state.terminal());
        }
    }

    #[test]
    fn dracula_needs_the_second_form_and_an_empty_meter_to_fall() {
        let mut wram = playing_wram();
        wram[STAGE] = FINAL_STAGE;
        wram[MODE] = MODE_STAGE_CLEAR;
        wram[BOSS_HEALTH] = 0;
        assert!(!decode_state(&wram).unwrap().boss_down());
        wram[DRACULA_FORM] = 1;
        assert!(decode_state(&wram).unwrap().boss_down());
        wram[BOSS_HEALTH] = 0x10;
        assert!(!decode_state(&wram).unwrap().boss_down());
    }

    #[test]
    fn genesis_requires_the_courtyard_in_play() {
        let mut wram = playing_wram();
        wram[MODE] = MODE_TITLE;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[MODE] = MODE_INTRO;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn setup_tape_presses_start_without_other_buttons() {
        let tape = setup_tape();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 1);
        assert!(tape.iter().all(|chord| chord.buttons & !0x08 == 0));
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for CastlevaniaSnapshot {
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
        let snapshot = CastlevaniaSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: CastlevaniaObservations {
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
            SnapshotCheckpoint::<CastlevaniaSnapshot>::from_bytes(&bytes, &checkpoint.format)
                .unwrap(),
            checkpoint
        );
        let old = postcard::to_allocvec(&(
            "castlevania-quicknes-snapshot-checkpoint-v0",
            &checkpoint.entries,
        ))
        .unwrap();
        assert!(
            SnapshotCheckpoint::<CastlevaniaSnapshot>::from_bytes(&old, &checkpoint.format)
                .is_err()
        );
    }
}
