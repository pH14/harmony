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

pub type PunchOutInput = crate::search::archive::Input<ButtonChord>;

pub const OPPONENTS: [&str; 14] = [
    "glass_joe",
    "von_kaiser",
    "piston_honda",
    "don_flamenco",
    "king_hippo",
    "great_tiger",
    "bald_bull",
    "piston_honda_2",
    "soda_popinski",
    "bald_bull_2",
    "don_flamenco_2",
    "mr_sandman",
    "super_macho_man",
    "mr_dream",
];
pub const OPPONENT_COUNT: u8 = 14;
pub const FULL_HEALTH: u8 = 0x60;
pub const INITIAL_HEARTS: u8 = 20;

pub const MODE_FIGHT: u8 = 0x01;
pub const MODE_ROUND_BREAK: u8 = 0xff;
pub const MODE_WON: u8 = 0xfe;
pub const MODE_TKO: u8 = 0xfc;
pub const MODE_LOST: u8 = 0xfd;
const STOP_MAC_DOWN: u8 = 2;

const MODE: usize = 0x00;
const OPPONENT: usize = 0x01;
const SCREEN: usize = 0x02;
const STOP: usize = 0x05;
const ROUND: usize = 0x06;
const OPPONENT_ACTION: usize = 0x90;
const OPPONENT_ACTION_ID: u8 = 0x7f;
const HEARTS_TENS: usize = 0x323;
const HEARTS_ONES: usize = 0x324;
const STARS: usize = 0x342;
const MAC_HEALTH: usize = 0x391;
const OPPONENT_HEALTH: usize = 0x398;
const ROUND_KNOCKDOWNS: usize = 0x3ca;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PunchOutMechanicalState {
    pub mode: u8,
    pub opponent: u8,
    pub screen: u8,
    pub stop: u8,
    pub round: u8,
    pub mac_health: u8,
    pub opponent_health: u8,
    pub hearts: u8,
    pub stars: u8,
    pub round_knockdowns: u8,
    pub opponent_action: u8,
}

impl PunchOutMechanicalState {
    #[must_use]
    pub fn fighting(self) -> bool {
        self.mode == MODE_FIGHT
    }

    #[must_use]
    pub fn knocked_down(self) -> bool {
        self.fighting() && self.stop == STOP_MAC_DOWN
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.knocked_down() || self.mode == MODE_LOST
    }

    #[must_use]
    pub fn opponents_defeated(self) -> u8 {
        self.opponent.min(OPPONENT_COUNT)
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.opponents_defeated() >= OPPONENT_COUNT
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutObservations {
    pub frame_count: u64,
    pub decoded: PunchOutMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: PunchOutMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PunchOutSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: PunchOutObservations,
    pub failed: bool,
}

impl PunchOutSnapshot {
    #[must_use]
    pub fn state(&self) -> PunchOutMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct PunchOutTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: PunchOutObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: PunchOutObservations,
    action_observations: Vec<PunchOutObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> PunchOutTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = PunchOutObservations {
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
                "Punch-Out!! setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> PunchOutMechanicalState {
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
    pub fn last_action_observations(&self) -> &[PunchOutObservations] {
        &self.action_observations
    }

    fn make_observation(frame_count: u64, state: PunchOutMechanicalState) -> PunchOutObservations {
        PunchOutObservations {
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
            .position(|wram| decode_state(wram).is_ok_and(PunchOutMechanicalState::terminal));
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
impl PunchOutTarget<QuickNesMachine> {
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
        input: &PunchOutInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<PunchOutVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Punch-Out!! gameplay genesis for film".into());
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
        metadata: &mut Option<PunchOutVideoMetadata>,
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
                    *metadata = Some(PunchOutVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: PunchOutMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for PunchOutTarget<M> {
    type Action = ButtonChord;
    type Observations = PunchOutObservations;
    type Snapshot = PunchOutSnapshot;

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
            | (u64::from(state.opponent) << 48)
            | (u64::from(state.screen) << 40)
            | (u64::from(state.round) << 32)
            | (u64::from(state.opponent_health) << 24)
            | (u64::from(state.mac_health) << 16)
            | (u64::from(state.round_knockdowns) << 8)
            | u64::from(state.opponent_action)
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
        Some(PunchOutSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for PunchOutTarget<M> {
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
    wait(&mut tape, 300);
    tape.push(ButtonChord::new(0x08, 2));
    wait(&mut tape, 400);
    tape.push(ButtonChord::new(0x08, 2));
    wait(&mut tape, 1100);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address).copied().ok_or_else(|| {
        MachineError::Backend(format!("Punch-Out!! RAM address {address:#x} is absent"))
    })
}

pub fn decode_state(wram: &[u8]) -> Result<PunchOutMechanicalState, MachineError> {
    let digit = |address| byte(wram, address).map(|value| value.min(9));
    Ok(PunchOutMechanicalState {
        mode: byte(wram, MODE)?,
        opponent: byte(wram, OPPONENT)?,
        screen: byte(wram, SCREEN)?,
        stop: byte(wram, STOP)?,
        round: byte(wram, ROUND)?,
        mac_health: byte(wram, MAC_HEALTH)?,
        opponent_health: byte(wram, OPPONENT_HEALTH)?,
        hearts: digit(HEARTS_TENS)? * 10 + digit(HEARTS_ONES)?,
        stars: byte(wram, STARS)?,
        round_knockdowns: byte(wram, ROUND_KNOCKDOWNS)?,
        opponent_action: byte(wram, OPPONENT_ACTION)? & OPPONENT_ACTION_ID,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("Punch-Out!! system RAM window has invalid length".to_owned())
    })
}

fn validate_genesis(state: PunchOutMechanicalState) -> Result<(), MachineError> {
    let valid = state.fighting()
        && state.opponent == 0
        && state.round == 1
        && state.mac_health == FULL_HEALTH
        && state.opponent_health == FULL_HEALTH
        && state.hearts == INITIAL_HEARTS
        && state.stars == 0
        && !state.terminal();
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Punch-Out!! setup did not reach the first round against Glass Joe: {state:?}"
        )))
    }
}

pub const HEALTH_BUCKET: u8 = 8;

#[must_use]
pub fn boundary_fields(state: PunchOutMechanicalState) -> [u8; 10] {
    [
        state.mode,
        state.opponent,
        state.screen,
        state.round,
        state.stop,
        state.opponent_health / HEALTH_BUCKET,
        state.mac_health / HEALTH_BUCKET,
        state.hearts,
        state.stars,
        state.round_knockdowns,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighting_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[MODE] = MODE_FIGHT;
        wram[ROUND] = 1;
        wram[MAC_HEALTH] = FULL_HEALTH;
        wram[OPPONENT_HEALTH] = FULL_HEALTH;
        wram[HEARTS_TENS] = 2;
        wram[OPPONENT_ACTION] = 0x81;
        wram
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = PunchOutSnapshot {
            emulator_state,
            observation: PunchOutObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<PunchOutSnapshot>() + snapshot.emulator_state.capacity();
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
    fn the_decoder_reads_hearts_as_two_decimal_digits_and_masks_the_action_start_bit() {
        let mut wram = fighting_wram();
        wram[HEARTS_TENS] = 1;
        wram[HEARTS_ONES] = 7;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.hearts, 17);
        assert_eq!(state.opponent_action, 0x01);
    }

    #[test]
    fn genesis_requires_the_first_round_against_glass_joe_at_full_health() {
        assert!(validate_genesis(decode_state(&fighting_wram()).unwrap()).is_ok());
        let mut wram = fighting_wram();
        wram[MAC_HEALTH] = FULL_HEALTH - 1;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = fighting_wram();
        wram[OPPONENT] = 1;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn a_knockdown_of_mac_and_a_lost_fight_are_terminal_deaths() {
        let mut wram = fighting_wram();
        wram[STOP] = STOP_MAC_DOWN;
        let down = decode_state(&wram).unwrap();
        assert!(down.dead() && down.terminal() && !down.victory());
        let mut wram = fighting_wram();
        wram[MODE] = MODE_LOST;
        assert!(decode_state(&wram).unwrap().dead());
        let mut wram = fighting_wram();
        wram[STOP] = 1;
        wram[OPPONENT_HEALTH] = 0;
        assert!(!decode_state(&wram).unwrap().terminal());
    }

    #[test]
    fn the_opponent_index_counts_defeated_opponents_and_the_last_one_is_victory() {
        let mut wram = fighting_wram();
        wram[MODE] = MODE_WON;
        wram[OPPONENT] = 1;
        let won = decode_state(&wram).unwrap();
        assert_eq!(won.opponents_defeated(), 1);
        assert!(!won.terminal());
        wram[OPPONENT] = OPPONENT_COUNT;
        assert!(decode_state(&wram).unwrap().victory());
    }

    #[test]
    fn setup_tape_presses_start_twice_without_other_buttons() {
        let tape = setup_tape();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 2);
        assert!(tape.iter().all(|chord| chord.buttons & !0x08 == 0));
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for PunchOutSnapshot {
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
        let snapshot = PunchOutSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: PunchOutObservations {
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
            SnapshotCheckpoint::<PunchOutSnapshot>::from_bytes(&bytes, &checkpoint.format).unwrap(),
            checkpoint
        );
        let old = postcard::to_allocvec(&(
            "punchout-quicknes-snapshot-checkpoint-v1",
            &checkpoint.entries,
        ))
        .unwrap();
        assert!(
            SnapshotCheckpoint::<PunchOutSnapshot>::from_bytes(&old, &checkpoint.format).is_err()
        );
    }
}
