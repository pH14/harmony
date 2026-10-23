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

pub type ZeldaInput = crate::search::archive::Input<ButtonChord>;

pub const TRIFORCE_PIECES: u8 = 8;
pub const ITEMS: [&str; 10] = [
    "raft",
    "ladder",
    "bow",
    "candle",
    "recorder",
    "power_bracelet",
    "magic_key",
    "magic_rod",
    "boomerang",
    "book",
];
pub const SWORDS: [&str; 3] = ["wooden_sword", "white_sword", "magic_sword"];

pub const MODE_PLAY: u8 = 0x05;
pub const MODE_DEATH: u8 = 0x11;
pub const MODE_ENDING: u8 = 0x13;
pub const OVERWORLD: u8 = 0;
pub const START_ROOM: u8 = 0x77;
pub const START_CONTAINERS: u8 = 3;

const LEVEL: usize = 0x10;
const MODE: usize = 0x12;
const LINK_X: usize = 0x70;
const LINK_Y: usize = 0x84;
const ROOM: usize = 0xeb;
const SWORD: usize = 0x657;
const BOMBS: usize = 0x658;
const BOW: usize = 0x65a;
const CANDLE: usize = 0x65b;
const RECORDER: usize = 0x65c;
const MAGIC_ROD: usize = 0x65f;
const RAFT: usize = 0x660;
const BOOK: usize = 0x661;
const LADDER: usize = 0x663;
const MAGIC_KEY: usize = 0x664;
const POWER_BRACELET: usize = 0x665;
const RUPEES: usize = 0x66d;
const KEYS: usize = 0x66e;
const HEARTS: usize = 0x66f;
const PARTIAL_HEART: usize = 0x670;
const TRIFORCE: usize = 0x671;
const BOOMERANG: usize = 0x674;
const MAGIC_BOOMERANG: usize = 0x675;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZeldaMechanicalState {
    pub mode: u8,
    pub level: u8,
    pub room: u8,
    pub x: u8,
    pub y: u8,
    pub hearts: u8,
    pub partial_heart: u8,
    pub rupees: u8,
    pub bombs: u8,
    pub keys: u8,
    pub sword: u8,
    pub items: u16,
    pub triforce: u8,
}

impl ZeldaMechanicalState {
    #[must_use]
    pub fn containers(self) -> u8 {
        (self.hearts >> 4) + 1
    }

    #[must_use]
    pub fn health(self) -> u16 {
        let full = u16::from(self.hearts & 0x0f);
        let partial = match self.partial_heart {
            0 => 0,
            1..=0x7f => 1,
            _ => 2,
        };
        full * 2 + partial
    }

    #[must_use]
    pub fn triforce_pieces(self) -> u8 {
        u8::try_from(self.triforce.count_ones()).unwrap_or(TRIFORCE_PIECES)
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.mode == MODE_DEATH
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.mode == MODE_ENDING
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaObservations {
    pub frame_count: u64,
    pub decoded: ZeldaMechanicalState,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: ZeldaMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaSnapshot {
    pub emulator_state: Vec<u8>,
    pub observation: ZeldaObservations,
    pub failed: bool,
}

impl ZeldaSnapshot {
    #[must_use]
    pub fn state(&self) -> ZeldaMechanicalState {
        self.observation.decoded
    }
}

#[derive(Debug)]
pub struct ZeldaTarget<M: NesBackend<Vec<u8>>> {
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: ZeldaObservations,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    observation: ZeldaObservations,
    action_observations: Vec<ZeldaObservations>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
}

impl<M: NesBackend<Vec<u8>>> ZeldaTarget<M> {
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = decode_state(&wram)?;
        validate_genesis(state)?;
        let genesis = machine.snapshot()?;
        let observation = ZeldaObservations {
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
                "Zelda setup did not quiesce".to_owned(),
            ));
        }
        machine.drop_snapshot(power_on)?;
        Self::from_machine(machine)
    }

    #[must_use]
    pub fn mechanical_state(&self) -> ZeldaMechanicalState {
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
    pub fn last_action_observations(&self) -> &[ZeldaObservations] {
        &self.action_observations
    }

    fn make_observation(frame_count: u64, state: ZeldaMechanicalState) -> ZeldaObservations {
        ZeldaObservations {
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
            .position(|wram| decode_state(wram).is_ok_and(ZeldaMechanicalState::terminal));
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
impl ZeldaTarget<QuickNesMachine> {
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
        input: &ZeldaInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<ZeldaVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Zelda gameplay genesis for film".into());
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
        metadata: &mut Option<ZeldaVideoMetadata>,
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
                    *metadata = Some(ZeldaVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: ZeldaMechanicalState::default(),
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

impl<M: NesBackend<Vec<u8>>> Target for ZeldaTarget<M> {
    type Action = ButtonChord;
    type Observations = ZeldaObservations;
    type Snapshot = ZeldaSnapshot;

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
            | (u64::from(state.level) << 48)
            | (u64::from(state.room) << 40)
            | (u64::from(state.x) << 32)
            | (u64::from(state.y) << 24)
            | (u64::from(state.health()) << 16)
            | (u64::from(state.rupees) << 8)
            | u64::from(state.triforce)
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
        Some(ZeldaSnapshot {
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

impl<M: NesBackend<Vec<u8>>> Drop for ZeldaTarget<M> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

const START: u8 = 0x08;
const SELECT: u8 = 0x04;
const BUTTON_A: u8 = 0x01;

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = Vec::new();
    let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
        tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
    };
    wait(&mut tape, 300);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, 200);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, 100);
    tape.push(ButtonChord::new(BUTTON_A, 2));
    wait(&mut tape, 30);
    for _ in 0..3 {
        tape.push(ButtonChord::new(SELECT, 2));
        wait(&mut tape, 20);
    }
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, 150);
    tape.push(ButtonChord::new(START, 2));
    wait(&mut tape, 200);
    tape
}

fn byte(wram: &[u8], address: usize) -> Result<u8, MachineError> {
    wram.get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Zelda RAM address {address:#x} is absent")))
}

pub fn decode_state(wram: &[u8]) -> Result<ZeldaMechanicalState, MachineError> {
    let held = |address| byte(wram, address).map(|value| value != 0);
    let items = [
        held(RAFT)?,
        held(LADDER)?,
        held(BOW)?,
        held(CANDLE)?,
        held(RECORDER)?,
        held(POWER_BRACELET)?,
        held(MAGIC_KEY)?,
        held(MAGIC_ROD)?,
        held(BOOMERANG)? || held(MAGIC_BOOMERANG)?,
        held(BOOK)?,
    ]
    .iter()
    .enumerate()
    .fold(0u16, |mask, (bit, &held)| mask | (u16::from(held) << bit));
    Ok(ZeldaMechanicalState {
        mode: byte(wram, MODE)?,
        level: byte(wram, LEVEL)?,
        room: byte(wram, ROOM)?,
        x: byte(wram, LINK_X)?,
        y: byte(wram, LINK_Y)?,
        hearts: byte(wram, HEARTS)?,
        partial_heart: byte(wram, PARTIAL_HEART)?,
        rupees: byte(wram, RUPEES)?,
        bombs: byte(wram, BOMBS)?,
        keys: byte(wram, KEYS)?,
        sword: byte(wram, SWORD)?,
        items,
        triforce: byte(wram, TRIFORCE)?,
    })
}

fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine
        .read(0, WRAM_SIZE as u32)?
        .try_into()
        .map_err(|_| MachineError::Backend("Zelda system RAM window has invalid length".to_owned()))
}

fn validate_genesis(state: ZeldaMechanicalState) -> Result<(), MachineError> {
    let valid = state.mode == MODE_PLAY
        && state.level == OVERWORLD
        && state.room == START_ROOM
        && state.containers() == START_CONTAINERS
        && state.health() == u16::from(START_CONTAINERS) * 2
        && state.sword == 0
        && state.items == 0
        && state.triforce == 0
        && state.rupees == 0;
    if valid {
        Ok(())
    } else {
        Err(MachineError::Backend(format!(
            "Zelda setup did not reach the first overworld screen: {state:?}"
        )))
    }
}

pub const GRID_SHIFT: u8 = 6;

#[must_use]
pub fn boundary_fields(state: ZeldaMechanicalState) -> [u16; 12] {
    [
        u16::from(state.mode),
        u16::from(state.level),
        u16::from(state.room),
        u16::from(state.x >> GRID_SHIFT),
        u16::from(state.y >> GRID_SHIFT),
        state.health(),
        u16::from(state.containers()),
        u16::from(state.rupees),
        u16::from(state.bombs),
        u16::from(state.sword),
        state.items,
        u16::from(state.triforce),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[MODE] = MODE_PLAY;
        wram[ROOM] = START_ROOM;
        wram[LINK_X] = 0x78;
        wram[LINK_Y] = 0x8d;
        wram[HEARTS] = 0x22;
        wram[PARTIAL_HEART] = 0xff;
        wram
    }

    #[test]
    fn checkpoint_image_budget_counts_spare_emulator_capacity_and_freezes_independently() {
        use searcher::search::{
            checkpoint::image::ImageBudget, snapshot_codec::CheckpointSnapshot,
        };
        let mut emulator_state = Vec::with_capacity(8192);
        emulator_state.extend_from_slice(&[3, 5, 7]);
        let mut snapshot = ZeldaSnapshot {
            emulator_state,
            observation: ZeldaObservations {
                frame_count: 11,
                decoded: Default::default(),
                terminal: false,
            },
            failed: false,
        };
        let exact = std::mem::size_of::<ZeldaSnapshot>() + snapshot.emulator_state.capacity();
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
    fn health_counts_half_hearts_from_the_low_nibble_and_the_partial_heart() {
        let mut wram = playing_wram();
        assert_eq!(decode_state(&wram).unwrap().health(), 6);
        assert_eq!(decode_state(&wram).unwrap().containers(), 3);
        wram[PARTIAL_HEART] = 0x7f;
        assert_eq!(decode_state(&wram).unwrap().health(), 5);
        wram[HEARTS] = 0x21;
        wram[PARTIAL_HEART] = 0xfe;
        assert_eq!(decode_state(&wram).unwrap().health(), 4);
        wram[HEARTS] = 0x20;
        wram[PARTIAL_HEART] = 0;
        assert_eq!(decode_state(&wram).unwrap().health(), 0);
    }

    #[test]
    fn the_decoder_counts_triforce_bits_and_sets_one_bit_per_key_item() {
        let mut wram = playing_wram();
        wram[TRIFORCE] = 0b1010_0001;
        wram[RAFT] = 1;
        wram[MAGIC_BOOMERANG] = 1;
        wram[CANDLE] = 2;
        let state = decode_state(&wram).unwrap();
        assert_eq!(state.triforce_pieces(), 3);
        assert_eq!(state.items, 0b01_0000_1001);
    }

    #[test]
    fn genesis_requires_the_first_screen_with_three_full_hearts_and_no_items() {
        assert!(validate_genesis(decode_state(&playing_wram()).unwrap()).is_ok());
        let mut wram = playing_wram();
        wram[PARTIAL_HEART] = 0x7f;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[SWORD] = 1;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
        let mut wram = playing_wram();
        wram[MODE] = 0x0e;
        assert!(validate_genesis(decode_state(&wram).unwrap()).is_err());
    }

    #[test]
    fn the_death_mode_and_the_ending_mode_are_terminal() {
        let mut wram = playing_wram();
        wram[MODE] = MODE_DEATH;
        let dead = decode_state(&wram).unwrap();
        assert!(dead.dead() && dead.terminal() && !dead.victory());
        wram[MODE] = MODE_ENDING;
        let won = decode_state(&wram).unwrap();
        assert!(won.victory() && won.terminal() && !won.dead());
        wram[MODE] = 0x0b;
        assert!(!decode_state(&wram).unwrap().terminal());
    }

    #[test]
    fn setup_tape_uses_only_start_select_and_a() {
        let tape = setup_tape();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            4
        );
        assert!(
            tape.iter()
                .all(|chord| chord.buttons & !(START | SELECT | BUTTON_A) == 0)
        );
    }
}

impl searcher::search::snapshot_codec::CheckpointSnapshot for ZeldaSnapshot {
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
        let snapshot = ZeldaSnapshot {
            emulator_state: vec![3, 1, 4, 1, 5],
            observation: ZeldaObservations {
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
            SnapshotCheckpoint::<ZeldaSnapshot>::from_bytes(&bytes, &checkpoint.format).unwrap(),
            checkpoint
        );
        let old = postcard::to_allocvec(&(
            "zelda-quicknes-snapshot-checkpoint-v0",
            &checkpoint.entries,
        ))
        .unwrap();
        assert!(SnapshotCheckpoint::<ZeldaSnapshot>::from_bytes(&old, &checkpoint.format).is_err());
    }
}
