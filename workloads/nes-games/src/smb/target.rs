// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(unix)]
use std::path::Path;
use std::{error::Error, mem::size_of};

use crate::target::ExitKind;
#[cfg(unix)]
use machine::quicknes::{QuickNesMachine, VideoFrame};
use machine::{MachineError, SnapId, StopConditions, nes};
use serde::{Deserialize, Serialize};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

use crate::{
    nes_backend::{NesBackend, SnapshotState, WorkRamCopy},
    smb::archive::{SmbArchiveKey, fold_frame},
    target::Target,
};

const SCREEN_PAGE_OFFSET: usize = 0x071a;
const SCREEN_X_OFFSET: usize = 0x071c;
const PLAYER_Y_OFFSET: usize = 0x00ce;
const PLAYER_X_PAGE_OFFSET: usize = 0x006d;
const PLAYER_X_LOW_OFFSET: usize = 0x0086;
const PLAYER_FACING_OFFSET: usize = 0x0033;
const PLAYER_ENGINE_STATE_OFFSET: usize = 0x000e;
const PLAYER_KILLED_STATE: u8 = 0x0b;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
pub const ROOM_IDENTITY_BYTES: [usize; 2] = [0x074e, 0x074f];

pub type SmbInput = crate::search::archive::Input<ButtonChord>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbObservations {
    pub frame_count: u64,
    pub wram: Vec<u8>,
    #[serde(default)]
    pub decoded: SmbMechanicalState,
    #[serde(default)]
    pub milestones: SmbMilestones,
    #[serde(default)]
    pub dead: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "")]
pub struct SmbSnapshot<P: SnapshotState = Vec<u8>> {
    emulator_state: P,
    observation: SnapshotObservation,
    work_ram: P::WorkRam,
    room_area: [u8; 2],
    frame_key: SmbArchiveKey,
    dead: bool,
    failed: bool,
}

impl<P: SnapshotState> SmbSnapshot<P> {
    pub fn logical_identity(&self) -> Result<Self, MachineError> {
        Ok(Self {
            emulator_state: self.emulator_state.logical_identity()?,
            ..self.clone()
        })
    }

    pub fn snapshot_identity_sha256(&self) -> Result<String, Box<dyn Error>> {
        crate::search::campaign::postcard_value_sha256(&self.logical_identity()?)
    }

    pub fn endpoint_identity_sha256(&self) -> Result<String, Box<dyn Error>> {
        crate::nes_backend::endpoint_identity_sha256(self, |snapshot| &mut snapshot.emulator_state)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SnapshotObservation {
    frame_count: u64,
    decoded: SmbMechanicalState,
    milestones: SmbMilestones,
    dead: bool,
}

impl SnapshotObservation {
    fn from_observation(observation: &SmbObservations) -> Self {
        Self {
            frame_count: observation.frame_count,
            decoded: observation.decoded,
            milestones: observation.milestones,
            dead: observation.dead,
        }
    }

    fn materialize(&self, wram: Vec<u8>) -> SmbObservations {
        SmbObservations {
            frame_count: self.frame_count,
            wram,
            decoded: self.decoded,
            milestones: self.milestones,
            dead: self.dead,
        }
    }
}

impl<P: SnapshotState> SmbSnapshot<P> {
    pub fn room_area(&self) -> [u8; 2] {
        self.room_area
    }

    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.observation.frame_count
    }

    #[must_use]
    pub fn emulator_state(&self) -> &P {
        &self.emulator_state
    }

    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.memory_charge()
    }

    pub fn resident_memory_charge(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.emulator_state.memory_charge())
            .saturating_add(self.work_ram.memory_charge())
    }
}

pub const BOOT_WALK: [ButtonChord; 2] = [
    ButtonChord {
        buttons: 0,
        hold_frames: 39,
    },
    ButtonChord {
        buttons: 0x08,
        hold_frames: 1,
    },
];

#[must_use]
pub fn boot_identifier() -> String {
    format!("boot=start-after-{}-idle-frames", BOOT_WALK[0].hold_frames)
}

const BOOT_PLAY_WAIT_FRAMES: u32 = 900;
const OPER_MODE_OFFSET: usize = 0x0770;
const OPER_MODE_PLAY: u8 = 1;
const OPER_MODE_TASK_OFFSET: usize = 0x0772;
const OPER_MODE_TASK_PLAY: u8 = 3;
const OPER_MODE_TASK_AREA_INIT: u8 = 0;

#[derive(Debug)]
pub struct SmbTarget<M, P = Vec<u8>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    machine: M,
    snapshot_base: Option<P>,
    genesis: SnapId,
    genesis_observation: SmbObservations,
    observation: SmbObservations,
    action_observations: Vec<SmbObservations>,
    genesis_frame_key: SmbArchiveKey,
    frame_key: SmbArchiveKey,
    stopped_at: Option<(SnapId, ButtonChord)>,
    dead: bool,
    failed: bool,
    execution_work: u64,
    boot_frames: u64,
    last_frames: u64,
    last_level_frames: u64,
}

impl<M, P> SmbTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    pub fn from_machine(machine: M) -> Result<Self, MachineError> {
        Self::boot(machine, true)
    }

    fn boot(mut machine: M, require_play: bool) -> Result<Self, MachineError> {
        machine::nes::run_actions(&mut machine, &BOOT_WALK)?;
        let idle = [ButtonChord {
            buttons: 0,
            hold_frames: 1,
        }];
        let mut reached_play = false;
        let mut boot_frames = BOOT_WALK
            .iter()
            .map(|chord| u64::from(chord.hold_frames))
            .sum::<u64>();
        for _ in 0..BOOT_PLAY_WAIT_FRAMES {
            if smb_is_playing(&wram_array(&machine)?) {
                reached_play = true;
                break;
            }
            boot_frames += 1;
            let here = machine.snapshot()?;
            machine.branch(here, &nes::reproducer(&idle))?;
            machine.run(StopConditions::default(), None)?;
            machine.drop_snapshot(here)?;
        }
        if require_play && !reached_play {
            return Err(MachineError::Backend(format!(
                "Super Mario Bros did not reach play within {BOOT_PLAY_WAIT_FRAMES} frames \
                 of the Start press"
            )));
        }
        let genesis = machine.snapshot()?;
        let wram = wram_array(&machine)?;
        let observation = SmbObservations {
            frame_count: 0,
            wram: wram.to_vec(),
            decoded: smb_mechanical_state_from_wram(&wram),
            milestones: smb_milestones_from_wram(&wram),
            dead: false,
        };
        let frame_key = fold_frame(None, &wram);
        Ok(Self {
            machine,
            snapshot_base: None,
            genesis,
            genesis_observation: observation.clone(),
            action_observations: vec![observation.clone()],
            observation,
            genesis_frame_key: frame_key,
            frame_key,
            stopped_at: None,
            dead: false,
            failed: false,
            execution_work: 0,
            boot_frames,
            last_frames: 0,
            last_level_frames: 0,
        })
    }

    pub fn root_after(&mut self, prefix: &[ButtonChord]) -> Result<(), MachineError> {
        self.reset();
        for action in prefix {
            self.apply(action);
        }
        if self.failed || self.dead || self.is_victory() || !self.rerun_to_stop() {
            return Err(MachineError::Backend(
                "the root prefix does not end in a live state".to_owned(),
            ));
        }
        let genesis = self.machine.snapshot()?;
        self.machine.drop_snapshot(self.genesis)?;
        self.genesis = genesis;
        self.snapshot_base = None;
        self.boot_frames = self
            .boot_frames
            .saturating_add(self.observation.frame_count);
        self.observation.frame_count = 0;
        self.genesis_observation = self.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.genesis_frame_key = self.frame_key;
        self.last_frames = 0;
        self.last_level_frames = 0;
        Ok(())
    }

    #[must_use]
    pub fn boot_frames(&self) -> u64 {
        self.boot_frames
    }

    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.observation.frame_count
    }

    #[must_use]
    pub fn frame_key(&self) -> SmbArchiveKey {
        self.frame_key
    }

    #[must_use]
    pub fn last_action_frames(&self) -> u64 {
        self.last_frames
    }

    #[must_use]
    pub fn last_action_level_frames(&self) -> u64 {
        self.last_level_frames
    }

    fn run_chord(&mut self, action: ButtonChord, until_control: bool) -> u64 {
        if !self.rerun_to_stop() {
            return 0;
        }
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return 0;
        };
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))
            .is_err()
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(start);
            return 0;
        }
        if !self.run_staged() {
            let _ = self.machine.drop_snapshot(start);
            return 0;
        }
        let mut endpoint = self.wram();
        let mut prior_bucket = smb_scroll_bucket(&endpoint);
        let mut executed_frames = 0_u64;
        let mut level_frames = 0_u64;
        for wram in self
            .machine
            .frames()
            .iter()
            .take(usize::from(action.bounded_hold_frames()))
        {
            executed_frames = executed_frames.saturating_add(1);
            if smb_is_inside_level(&endpoint) {
                level_frames = level_frames.saturating_add(1);
            }
            endpoint = *wram;
            self.frame_key = fold_frame(Some(self.frame_key), wram);
            let current_bucket = smb_scroll_bucket(wram);
            self.dead = smb_player_is_dead(wram);
            let victory = smb_is_victory(wram);
            if current_bucket != prior_bucket || self.dead || victory {
                prior_bucket = current_bucket;
                self.action_observations.push(observation_from(
                    wram,
                    self.observation.frame_count.saturating_add(executed_frames),
                    self.dead,
                ));
            }
            if self.dead || victory || (until_control && smb_player_has_control(wram)) {
                break;
            }
        }
        if executed_frames < self.machine.frames().len() as u64 {
            let kept = u8::try_from(executed_frames).unwrap_or(1);
            self.machine.keep_run_capture(usize::from(kept));
            self.stopped_at = Some((start, ButtonChord::new(action.buttons, kept)));
        } else {
            let _ = self.machine.drop_snapshot(start);
        }
        self.execution_work = self.execution_work.saturating_add(executed_frames);
        self.last_frames = self.last_frames.saturating_add(executed_frames);
        self.last_level_frames = self.last_level_frames.saturating_add(level_frames);
        let endpoint_frame = self.observation.frame_count.saturating_add(executed_frames);
        let endpoint_already_recorded = self
            .action_observations
            .last()
            .is_some_and(|observation| observation.frame_count == endpoint_frame);
        if !endpoint_already_recorded {
            self.action_observations
                .push(observation_from(&endpoint, endpoint_frame, self.dead));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        executed_frames
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.dead || !self.rerun_to_stop() {
            return false;
        }
        let mut env = Vec::new();
        let mut remaining = frames;
        while remaining > 0 {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            env.push(ButtonChord::new(buttons, u8::try_from(hold).unwrap_or(1)));
            remaining -= hold;
        }
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return false;
        };
        let survived = self.probe_env(start, &env, usize::from(frames));
        if self.machine.replay(start).is_err() {
            self.failed = true;
        }
        let _ = self.machine.drop_snapshot(start);
        survived && !self.failed
    }

    fn probe_env(&mut self, start: SnapId, env: &[ButtonChord], frames: usize) -> bool {
        if self.machine.branch(start, &nes::reproducer(env)).is_err() {
            self.failed = true;
            return false;
        }
        let mut observed = 0_usize;
        while observed < frames {
            if !self.run_staged() {
                return false;
            }
            let produced = self.machine.frames();
            if produced.is_empty()
                || produced
                    .iter()
                    .take(frames - observed)
                    .any(smb_player_is_dead)
            {
                return false;
            }
            observed = observed.saturating_add(produced.len());
        }
        true
    }

    fn rerun_to_stop(&mut self) -> bool {
        let Some((start, chord)) = self.stopped_at.take() else {
            return true;
        };
        let rerun = if self
            .machine
            .branch(start, &nes::reproducer(&[chord]))
            .is_ok()
        {
            self.run_staged()
        } else {
            self.failed = true;
            false
        };
        self.machine.keep_run_capture(0);
        let _ = self.machine.drop_snapshot(start);
        rerun
    }

    fn forget_stop(&mut self) {
        if let Some((start, _)) = self.stopped_at.take() {
            let _ = self.machine.drop_snapshot(start);
        }
    }

    fn run_staged(&mut self) -> bool {
        let stopped = matches!(
            self.machine.run(StopConditions::default(), None),
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        );
        self.failed |= !stopped;
        stopped
    }

    #[must_use]
    pub fn wram(&self) -> [u8; WRAM_SIZE] {
        self.observation
            .wram
            .as_slice()
            .try_into()
            .unwrap_or([0; WRAM_SIZE])
    }

    pub fn mechanical_state(&self) -> SmbMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    #[must_use]
    pub fn is_doomed(&self) -> bool {
        !self.dead && smb_player_is_doomed(&self.wram())
    }

    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.observation
            .wram
            .as_slice()
            .try_into()
            .ok()
            .is_some_and(smb_is_victory)
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[SmbObservations] {
        &self.action_observations
    }

    pub fn snapshot_of_state(
        &mut self,
        state: &P,
        frame_count: u64,
    ) -> Result<SmbSnapshot<P>, Box<dyn Error>> {
        self.forget_stop();
        self.machine
            .restore_nes(state)
            .map_err(|error| error.to_string())?;
        let wram = wram_array(&self.machine).map_err(|error| error.to_string())?;
        self.dead = smb_player_is_dead(&wram);
        self.failed = false;
        self.snapshot_base = None;
        self.observation = observation_from(&wram, frame_count, self.dead);
        self.action_observations = vec![self.observation.clone()];
        self.frame_key = fold_frame(None, &wram);
        self.snapshot()
            .ok_or_else(|| "failed to snapshot the restored state".into())
    }
}

fn observation_from(wram: &[u8; WRAM_SIZE], frame_count: u64, dead: bool) -> SmbObservations {
    SmbObservations {
        frame_count,
        wram: wram.to_vec(),
        decoded: smb_mechanical_state_from_wram(wram),
        milestones: smb_milestones_from_wram(wram),
        dead,
    }
}

fn wram_array<M, P>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    machine
        .read(0, WRAM_SIZE as u32)?
        .try_into()
        .map_err(|_| MachineError::Backend("SMB work RAM window has an invalid length".to_owned()))
}

impl<M, P> Target for SmbTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    type Action = ButtonChord;
    type Observations = SmbObservations;
    type Snapshot = SmbSnapshot<P>;

    fn reset(&mut self) {
        self.forget_stop();
        self.failed = self.machine.replay(self.genesis).is_err();
        self.snapshot_base = None;
        self.dead = false;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.frame_key = self.genesis_frame_key;
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        self.last_frames = 0;
        self.last_level_frames = 0;
        if self.failed || self.dead || self.is_victory() {
            return;
        }
        if action.hold_frames != WAIT_FOR_CONTROL_HOLD {
            self.run_chord(*action, false);
            return;
        }
        while self.last_frames < WAIT_FOR_CONTROL_FRAMES
            && !self.failed
            && !self.dead
            && !self.is_victory()
            && !smb_player_has_control(&self.wram())
        {
            if self.run_chord(ButtonChord::new(action.buttons, MAX_HOLD_FRAMES), true) == 0 {
                break;
            }
        }
        if self.action_observations.is_empty() {
            self.action_observations.push(self.observation.clone());
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        smb_fingerprint_from_wram(&self.wram())
    }

    fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        if !self.rerun_to_stop() {
            return None;
        }
        let Ok(emulator_state) = self.machine.capture_nes(self.snapshot_base.as_ref()) else {
            self.failed = true;
            return None;
        };
        self.snapshot_base = Some(emulator_state.clone());
        let wram = self.wram();
        Some(SmbSnapshot {
            emulator_state,
            observation: SnapshotObservation::from_observation(&self.observation),
            work_ram: P::WorkRam::copy_of(&wram),
            room_area: ROOM_IDENTITY_BYTES.map(|offset| wram[offset]),
            frame_key: self.frame_key,
            dead: self.dead,
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let kept = snapshot
            .work_ram
            .work_ram()
            .map(<[u8; WRAM_SIZE]>::try_from)
            .transpose()
            .map_err(|_| "SMB snapshot work RAM is not exactly 2 KiB")?;
        self.forget_stop();
        self.machine
            .restore_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        let wram = match kept {
            Some(wram) => wram,
            None => wram_array(&self.machine).map_err(|error| error.to_string())?,
        };
        self.observation = snapshot.observation.materialize(wram.to_vec());
        self.action_observations = vec![self.observation.clone()];
        self.dead = snapshot.dead;
        self.failed = snapshot.failed;
        self.frame_key = snapshot.frame_key;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        Ok(())
    }
}

#[cfg(unix)]
impl SmbTarget<QuickNesMachine, Vec<u8>> {
    pub fn from_smb_rom_bytes_headless_with_identity(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
    ) -> Result<Self, MachineError> {
        let machine = QuickNesMachine::from_rom_bytes_with_identity(
            rom,
            core_path,
            core_sha256,
            execution_sha256,
        )?;
        Self::from_machine(machine)
    }

    pub fn from_smb_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        Self::from_machine(machine)
    }

    pub fn from_smb_rom_bytes_capturing(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let mut machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        machine.set_video_capture(true);
        machine.set_audio_capture(true);
        Self::from_machine(machine)
    }

    pub fn drain_frames(&mut self) -> Vec<VideoFrame> {
        self.machine.take_video_frames()
    }

    pub fn drain_audio(&mut self) -> Vec<i16> {
        self.machine.take_audio_samples()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        self.machine.poke_wram(addr, byte)?;
        let wram = wram_array(&self.machine)?;
        self.observation.wram = wram.to_vec();
        self.observation.decoded = smb_mechanical_state_from_wram(&wram);
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn loopback_for_tests(rom: &[u8]) -> Result<Self, MachineError> {
        Self::boot(QuickNesMachine::loopback_for_tests(rom)?, false)
    }
}

fn smb_scroll_bucket(wram: &[u8; WRAM_SIZE]) -> u16 {
    (smb_camera_pixels(wram) / 16) as u16
}

#[must_use]
pub fn smb_camera_pixels(wram: &[u8; WRAM_SIZE]) -> u32 {
    u32::from(i16::from_be_bytes([wram[SCREEN_PAGE_OFFSET], wram[SCREEN_X_OFFSET]]).max(0) as u16)
}

const PLAYER_BELOW_PLAY_AREA_PAGE: u8 = 2;

fn smb_player_is_dead(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_KILLED_STATE
        || wram[PLAYER_VERTICAL_PAGE_OFFSET] >= PLAYER_BELOW_PLAY_AREA_PAGE
}

const PLAYER_CONTROL_STATE: u8 = 0x08;
const PLAYER_AIR_STATE_OFFSET: usize = 0x001d;
const PLAYER_JUMPING: u8 = 1;
const PLAYER_FALLING: u8 = 2;
const PLAYER_Y_SPEED_OFFSET: usize = 0x009f;
const SWIMMING_FLAG_OFFSET: usize = 0x0704;
const PLAY_AREA_PAGE: u8 = 1;
const GROUND_LINE_Y: u8 = 0xb0;
const METATILE_PIXELS: u8 = 16;
const ENEMY_SLOTS: usize = 5;
const ENEMY_ACTIVE_OFFSET: usize = 0x000f;
const ENEMY_ID_OFFSET: usize = 0x0016;
const LIFT_ENEMY_IDS: std::ops::RangeInclusive<u8> = 0x24..=0x2c;
const JUMPSPRING_ENEMY_ID: u8 = 0x32;
const BLOCK_BUFFERS: [usize; 2] = [0x0500, 0x05d0];
const BLOCK_BUFFER_COLUMNS: u32 = 16;
const FLOOR_ROWS: [usize; 2] = [11, 12];
const FOOT_OFFSETS: [u32; 2] = [3, 12];
const NON_SOLID_METATILES: [u8; 6] = [0x00, 0x26, 0x5f, 0x60, 0xc2, 0xc3];

fn metatile_is_solid(metatile: u8) -> bool {
    !NON_SOLID_METATILES.contains(&metatile)
}

fn floor_is_open_at(wram: &[u8; WRAM_SIZE], x: u32) -> bool {
    let column = x / 16;
    let buffer = BLOCK_BUFFERS[usize::from(column / BLOCK_BUFFER_COLUMNS % 2 == 1)];
    let offset = usize::try_from(column % BLOCK_BUFFER_COLUMNS).unwrap_or(0);
    FLOOR_ROWS
        .iter()
        .all(|row| !metatile_is_solid(wram[buffer + row * BLOCK_BUFFER_COLUMNS as usize + offset]))
}

fn a_lift_or_jumpspring_is_active(wram: &[u8; WRAM_SIZE]) -> bool {
    (0..ENEMY_SLOTS).any(|slot| {
        let id = wram[ENEMY_ID_OFFSET + slot];
        wram[ENEMY_ACTIVE_OFFSET + slot] != 0
            && (LIFT_ENEMY_IDS.contains(&id) || id == JUMPSPRING_ENEMY_ID)
    })
}

#[must_use]
pub fn smb_player_is_doomed(wram: &[u8; WRAM_SIZE]) -> bool {
    let x = u32::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u32::from(wram[PLAYER_X_LOW_OFFSET]);
    wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_CONTROL_STATE
        && matches!(
            wram[PLAYER_AIR_STATE_OFFSET],
            PLAYER_JUMPING | PLAYER_FALLING
        )
        && wram[SWIMMING_FLAG_OFFSET] == 0
        && wram[PLAYER_VERTICAL_PAGE_OFFSET] == PLAY_AREA_PAGE
        && wram[PLAYER_Y_OFFSET] >= GROUND_LINE_Y + METATILE_PIXELS
        && wram[PLAYER_Y_SPEED_OFFSET] < 0x80
        && FOOT_OFFSETS
            .iter()
            .all(|offset| floor_is_open_at(wram, x + offset))
        && !a_lift_or_jumpspring_is_active(wram)
}

const OPERATING_MODE_OFFSET: usize = 0x0770;
const VICTORY_OPERATING_MODE: u8 = 2;
const FINAL_WORLD_NUMBER: u8 = 7;
pub const WAIT_FOR_CONTROL_HOLD: u8 = 0;
const WAIT_FOR_CONTROL_FRAMES: u64 = 2_400;
const LEVEL_END_ENGINE_STATES: [u8; 4] = [0x00, 0x04, 0x05, 0x07];

#[must_use]
pub const fn wait_for_control(buttons: u8) -> ButtonChord {
    ButtonChord {
        buttons,
        hold_frames: WAIT_FOR_CONTROL_HOLD,
    }
}

#[must_use]
pub fn smb_player_has_control(wram: &[u8; WRAM_SIZE]) -> bool {
    smb_is_playing(wram) && wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_CONTROL_STATE
}

#[must_use]
pub fn smb_is_inside_level(wram: &[u8; WRAM_SIZE]) -> bool {
    smb_is_playing(wram) && !LEVEL_END_ENGINE_STATES.contains(&wram[PLAYER_ENGINE_STATE_OFFSET])
}

#[must_use]
pub fn smb_is_victory(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPERATING_MODE_OFFSET] == VICTORY_OPERATING_MODE
        && wram[WORLD_NUMBER_OFFSET] == FINAL_WORLD_NUMBER
}

const PLAYER_STATE_BYTES: [usize; 34] = [
    0x000e, 0x001d, 0x0033, 0x0045, 0x0057, 0x006d, 0x0086, 0x009f, 0x00b5, 0x00ce, 0x0416, 0x0433,
    0x0490, 0x0700, 0x0701, 0x0702, 0x0703, 0x0704, 0x0705, 0x0709, 0x070a, 0x0714, 0x0717, 0x071a,
    0x071c, 0x074e, 0x074f, 0x0750, 0x0754, 0x0756, 0x075c, 0x075f, 0x0782, 0x0783,
];
const SAVED_JOYPAD_OFFSET: usize = 0x06fc;
const SAVED_JUMP_AND_RUN_BITS: u8 = 0xc0;

fn smb_fingerprint_from_wram(wram: &[u8; WRAM_SIZE]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let bytes = PLAYER_STATE_BYTES
        .iter()
        .map(|&offset| wram[offset])
        .chain([wram[SAVED_JOYPAD_OFFSET] & SAVED_JUMP_AND_RUN_BITS]);
    for byte in bytes {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestones {
    pub max_1_1_scroll_bucket: u16,
    pub reached_1_1_flag: bool,
    pub reached_1_2: bool,
    pub reached_onward: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneTimes {
    pub progress_into_1_1: Option<u64>,
    pub flag_1_1: Option<u64>,
    pub level_1_2: Option<u64>,
    pub onward: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneInputs {
    pub progress_into_1_1: Option<SmbInput>,
    pub flag_1_1: Option<SmbInput>,
    pub level_1_2: Option<SmbInput>,
    pub onward: Option<SmbInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbProgressWatermark {
    pub world: u8,
    pub level: u8,
    pub progress: u16,
}

#[must_use]
pub fn smb_milestones_from_wram(wram: &[u8; WRAM_SIZE]) -> SmbMilestones {
    let world = wram[WORLD_NUMBER_OFFSET];
    let level = wram[LEVEL_NUMBER_OFFSET];
    let in_1_1 = world == 0 && level == 0;
    let scroll_bucket = if in_1_1 { smb_scroll_bucket(wram) } else { 0 };
    SmbMilestones {
        max_1_1_scroll_bucket: scroll_bucket,
        reached_1_1_flag: in_1_1 && wram[FLAG_TASK_OFFSET] != 0,
        reached_1_2: world == 0 && level == 1,
        reached_onward: world > 0 || (world == 0 && level > 1),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbMechanicalState {
    pub world: u8,
    pub level: u8,
    pub progress: u16,
    pub player_y_bucket: u8,
    pub player_engine_state: u8,
    pub dead: bool,
    pub flag_active: bool,
}

#[must_use]
pub fn smb_mechanical_state_from_wram(wram: &[u8; WRAM_SIZE]) -> SmbMechanicalState {
    SmbMechanicalState {
        world: wram[WORLD_NUMBER_OFFSET],
        level: wram[LEVEL_NUMBER_OFFSET],
        progress: if smb_area_is_loading(wram) {
            0
        } else {
            smb_scroll_bucket(wram)
        },
        player_y_bucket: wram[PLAYER_Y_OFFSET] / 16,
        player_engine_state: wram[PLAYER_ENGINE_STATE_OFFSET],
        dead: smb_player_is_dead(wram),
        flag_active: wram[FLAG_TASK_OFFSET] != 0,
    }
}

#[must_use]
pub fn smb_is_playing(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPER_MODE_OFFSET] == OPER_MODE_PLAY && wram[OPER_MODE_TASK_OFFSET] == OPER_MODE_TASK_PLAY
}

#[must_use]
pub fn smb_world_cleared(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPERATING_MODE_OFFSET] == VICTORY_OPERATING_MODE
        && wram[WORLD_NUMBER_OFFSET] != FINAL_WORLD_NUMBER
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbPlayerPosition {
    pub x: u32,
    pub y: u8,
    pub vertical_page: u8,
    pub facing: u8,
}

#[must_use]
pub fn smb_player_position(wram: &[u8; WRAM_SIZE]) -> SmbPlayerPosition {
    SmbPlayerPosition {
        x: u32::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u32::from(wram[PLAYER_X_LOW_OFFSET]),
        y: wram[PLAYER_Y_OFFSET],
        vertical_page: wram[PLAYER_VERTICAL_PAGE_OFFSET],
        facing: wram[PLAYER_FACING_OFFSET],
    }
}

pub fn smb_area_is_loading(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPER_MODE_OFFSET] == OPER_MODE_PLAY
        && wram[OPER_MODE_TASK_OFFSET] == OPER_MODE_TASK_AREA_INIT
}

const PLAYER_VERTICAL_PAGE_OFFSET: usize = 0x00b5;

impl<P: SnapshotState> searcher::search::snapshot_codec::CheckpointSnapshot for SmbSnapshot<P> {
    type Codec = searcher::search::snapshot_codec::PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        budget.reserve(std::mem::size_of::<Self>())?;
        self.emulator_state.checkpoint_image_charge(budget)?;
        budget.reserve(self.work_ram.memory_charge())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        BOOT_PLAY_WAIT_FRAMES, ButtonChord, MAX_HOLD_FRAMES, OPER_MODE_OFFSET, OPER_MODE_PLAY,
        OPER_MODE_TASK_OFFSET, OPER_MODE_TASK_PLAY, PLAYER_ENGINE_STATE_OFFSET,
        PLAYER_KILLED_STATE, SCREEN_X_OFFSET, SmbSnapshot, SmbTarget, WRAM_SIZE, smb_is_victory,
        smb_mechanical_state_from_wram, smb_player_is_doomed,
    };
    use crate::{
        nes_backend::{NesBackend, SnapshotState},
        target::{ExitKind, Target},
    };
    use machine::{
        Machine, MachineError, Moment, SnapId, StopConditions, nes, quicknes::QuickNesMachine,
    };
    use serde::{Deserialize, Serialize};

    #[test]
    fn a_core_that_never_reaches_play_is_an_error_rather_than_a_sealed_genesis() {
        let core = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let error = SmbTarget::boot(core, true).expect_err("non-play genesis is refused");
        assert!(
            error
                .to_string()
                .contains(&BOOT_PLAY_WAIT_FRAMES.to_string()),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn chord_duration_is_total_and_bounded() {
        assert_eq!(ButtonChord::new(0x81, 0).hold_frames, 1);
        assert_eq!(ButtonChord::new(0x81, u8::MAX).hold_frames, MAX_HOLD_FRAMES);
    }

    fn falling_over(floor: u8) -> [u8; WRAM_SIZE] {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[0x000e] = 0x08;
        wram[0x001d] = 2;
        wram[0x00b5] = 1;
        wram[0x00ce] = 0xc4;
        wram[0x009f] = 4;
        wram[0x006d] = 3;
        wram[0x0086] = 0x40;
        for row in [11, 12] {
            for column in 0..16 {
                wram[0x05d0 + row * 16 + column] = floor;
            }
        }
        wram
    }

    #[test]
    fn a_fall_below_the_ground_line_over_open_floor_is_doomed() {
        assert!(smb_player_is_doomed(&falling_over(0x00)));
        assert!(smb_player_is_doomed(&falling_over(0xc2)));
        assert!(!smb_player_is_doomed(&falling_over(0x54)));
    }

    #[test]
    fn a_state_that_can_still_land_or_rise_is_not_doomed() {
        let mut above = falling_over(0x00);
        above[0x00ce] = 0xb0;
        assert!(!smb_player_is_doomed(&above));
        let mut within_a_block = falling_over(0x00);
        within_a_block[0x00ce] = 0xbf;
        assert!(!smb_player_is_doomed(&within_a_block));
        for id in [0x24, 0x2c, 0x32] {
            let mut over_a_lift = falling_over(0x00);
            over_a_lift[0x0011] = 1;
            over_a_lift[0x0018] = id;
            assert!(!smb_player_is_doomed(&over_a_lift));
            over_a_lift[0x0011] = 0;
            assert!(smb_player_is_doomed(&over_a_lift));
        }
        let mut over_an_enemy = falling_over(0x00);
        over_an_enemy[0x0011] = 1;
        over_an_enemy[0x0018] = 0x06;
        assert!(smb_player_is_doomed(&over_an_enemy));
        let mut rising = falling_over(0x00);
        rising[0x009f] = 0xfc;
        assert!(!smb_player_is_doomed(&rising));
        let mut standing = falling_over(0x00);
        standing[0x001d] = 0;
        assert!(!smb_player_is_doomed(&standing));
        let mut swimming = falling_over(0x00);
        swimming[0x0704] = 1;
        assert!(!smb_player_is_doomed(&swimming));
        let mut in_pipe = falling_over(0x00);
        in_pipe[0x000e] = 0x03;
        assert!(!smb_player_is_doomed(&in_pipe));
        let mut edge = falling_over(0x00);
        edge[0x0086] = 0x48;
        edge[0x05d0 + 12 * 16 + 5] = 0x54;
        assert!(smb_player_is_doomed(&falling_over(0x00)));
        assert!(!smb_player_is_doomed(&edge));
    }

    #[test]
    fn mechanical_state_is_decoded_from_fixed_offsets() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[0x075f] = 2;
        wram[0x075c] = 3;
        wram[0x071a] = 4;
        wram[0x071c] = 32;
        wram[0x00ce] = 48;
        wram[0x000e] = 7;
        let decoded = smb_mechanical_state_from_wram(&wram);
        assert_eq!((decoded.world, decoded.level, decoded.progress), (2, 3, 66));
        assert_eq!(decoded.player_y_bucket, 3);
        assert_eq!(decoded.player_engine_state, 7);
    }

    #[test]
    fn the_level_number_holds_through_the_last_flag_task() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[0x075f] = 2;
        wram[0x075c] = 2;
        wram[0x0746] = 5;
        let decoded = smb_mechanical_state_from_wram(&wram);
        assert_eq!((decoded.world, decoded.level), (2, 2));
        assert!(decoded.flag_active);
    }

    #[test]
    fn scroll_progress_is_zero_while_an_area_loads() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[0x075f] = 1;
        wram[0x071a] = 9;
        wram[0x0770] = 1;
        wram[0x0772] = 3;
        assert_eq!(smb_mechanical_state_from_wram(&wram).progress, 144);
        wram[0x0772] = 0;
        assert_eq!(smb_mechanical_state_from_wram(&wram).progress, 0);
        wram[0x0770] = 2;
        assert_eq!(
            smb_mechanical_state_from_wram(&wram).progress,
            144,
            "the castle ending starts its tasks at zero"
        );
    }

    #[test]
    fn victory_is_decoded_from_the_operating_mode_and_world_bytes() {
        let mut wram = [0_u8; WRAM_SIZE];
        assert!(!smb_is_victory(&wram));
        wram[0x0770] = 2;
        assert!(!smb_is_victory(&wram), "earlier castles enter mode 2 too");
        wram[0x075f] = 7;
        wram[0x0770] = 1;
        assert!(!smb_is_victory(&wram));
        wram[0x0770] = 2;
        assert!(smb_is_victory(&wram));
        wram[0x0770] = 3;
        assert!(!smb_is_victory(&wram));
    }

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn a_snapshot_in_victory_mode_reports_victory_and_takes_no_action() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        assert!(!target.is_victory());
        target.poke_wram(0x0770, 2).unwrap();
        target.poke_wram(0x075f, 7).unwrap();
        let won = target.snapshot().expect("snapshot victory state");
        let mut restored = SmbTarget::loopback_for_tests(&rom).expect("load target");
        restored.restore(&won).expect("restore victory snapshot");
        assert!(restored.is_victory());
        let frames_before = restored.execution_work();
        restored.apply(&ButtonChord::new(0x01, 10));
        assert_eq!(restored.execution_work(), frames_before);
        assert!(restored.last_action_observations().is_empty());
    }

    #[test]
    fn snapshot_accessors_and_reset_restore_exact_genesis_state() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        for (offset, value) in super::ROOM_IDENTITY_BYTES.into_iter().zip([7, 9]) {
            target.poke_wram(offset, value).unwrap();
        }
        let identified = target.snapshot().expect("snapshot identified room");
        assert_eq!(identified.room_area(), [7, 9]);
        assert!(identified.emulator_state_bytes_len() > 1);
        assert!(identified.resident_memory_charge() > identified.emulator_state_bytes_len());

        target.reset();
        let genesis = target.snapshot().expect("snapshot genesis");
        target.apply(&ButtonChord::new(0x01, 10));
        let first_work = target.execution_work();
        assert!(first_work > 0);
        assert_ne!(target.snapshot().expect("snapshot advanced"), genesis);
        let saved = target.snapshot().expect("snapshot after first action");
        target.apply(&ButtonChord::new(0x02, 10));
        let second_work = target.execution_work();
        assert!(second_work > first_work);
        target.restore(&saved).expect("restore after second action");
        assert_eq!(target.snapshot().expect("snapshot restored"), saved);
        assert_eq!(
            saved.resident_memory_charge(),
            size_of::<super::SmbSnapshot>() + saved.emulator_state_bytes_len()
        );
        assert_eq!(target.execution_work(), second_work);
        target.reset();
        assert_eq!(target.execution_work(), second_work);
        target.apply(&ButtonChord::new(0x01, 10));
        assert!(target.execution_work() > second_work);
        target.reset();
        assert_eq!(target.snapshot().expect("snapshot reset"), genesis);
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FakePortable(Vec<u8>);

    impl SnapshotState for FakePortable {
        type WorkRam = Box<[u8]>;

        fn memory_charge(&self) -> usize {
            self.0.len()
        }

        fn checkpoint_image_charge(
            &self,
            budget: &mut searcher::search::checkpoint::image::ImageBudget,
        ) -> Result<(), Box<dyn std::error::Error>> {
            budget.array::<u8>(self.0.capacity())
        }

        fn logical_identity(&self) -> Result<Self, MachineError> {
            Ok(Self(self.0.iter().take(1).copied().collect()))
        }
    }

    #[derive(Debug)]
    struct FakeMachine {
        wram: [u8; WRAM_SIZE],
        snapshots: BTreeMap<u64, [u8; WRAM_SIZE]>,
        next_snapshot: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
        vtime: u64,
        readable: bool,
        kill_on_frame: Option<usize>,
        kept_capture: Vec<usize>,
    }

    impl FakeMachine {
        fn new() -> Self {
            let mut wram = [0; WRAM_SIZE];
            wram[OPER_MODE_OFFSET] = OPER_MODE_PLAY;
            wram[OPER_MODE_TASK_OFFSET] = OPER_MODE_TASK_PLAY;
            Self {
                wram,
                snapshots: BTreeMap::new(),
                next_snapshot: 0,
                staged: Vec::new(),
                frames: Vec::new(),
                vtime: 0,
                readable: true,
                kill_on_frame: None,
                kept_capture: Vec::new(),
            }
        }

        fn hold(&mut self, wram: [u8; WRAM_SIZE]) -> SnapId {
            let id = self.next_snapshot;
            self.next_snapshot += 1;
            self.snapshots.insert(id, wram);
            SnapId(id)
        }

        fn held(&self, snap: SnapId) -> Result<[u8; WRAM_SIZE], MachineError> {
            self.snapshots
                .get(&snap.0)
                .copied()
                .ok_or(MachineError::UnknownSnapshot)
        }
    }

    impl Machine for FakeMachine {
        type Portable = FakePortable;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            Ok(self.hold(self.wram))
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.snapshots
                .remove(&snap.0)
                .map(|_| ())
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn branch(&mut self, snap: SnapId, env: &machine::Reproducer) -> Result<(), MachineError> {
            self.wram = self.held(snap)?;
            self.staged = nes::actions_of(env)?;
            self.readable = false;
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.wram = self.held(snap)?;
            self.staged.clear();
            self.readable = false;
            Ok(())
        }

        fn run(
            &mut self,
            until: StopConditions,
            resolve: Option<&machine::Answer>,
        ) -> Result<machine::StopReason, MachineError> {
            if until != StopConditions::default() || resolve.is_some() {
                return Err(MachineError::Backend(
                    "only default stop conditions are supported".to_owned(),
                ));
            }
            self.frames.clear();
            for chord in std::mem::take(&mut self.staged) {
                for _ in 0..chord.bounded_hold_frames() {
                    self.wram[SCREEN_X_OFFSET] =
                        self.wram[SCREEN_X_OFFSET].wrapping_add(chord.buttons);
                    if self.kill_on_frame == Some(self.frames.len() + 1) {
                        self.wram[PLAYER_ENGINE_STATE_OFFSET] = PLAYER_KILLED_STATE;
                    }
                    self.frames.push(self.wram);
                    self.vtime += 1;
                }
            }
            self.readable = true;
            Ok(machine::StopReason::SnapshotPoint { vtime: self.now() })
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            if !self.readable {
                return Err(MachineError::Backend(
                    "no cached observation at the current stop".to_owned(),
                ));
            }
            let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
            let end = start
                .checked_add(usize::try_from(len).map_err(|_| MachineError::ReadOutOfBounds)?)
                .ok_or(MachineError::ReadOutOfBounds)?;
            self.wram
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(MachineError::ReadOutOfBounds)
        }

        fn export(
            &mut self,
            snap: SnapId,
            _base: Option<&Self::Portable>,
        ) -> Result<Self::Portable, MachineError> {
            Ok(FakePortable(self.held(snap)?.to_vec()))
        }

        fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
            let wram = portable
                .0
                .as_slice()
                .try_into()
                .map_err(|_| MachineError::Backend("fake state is not work RAM".to_owned()))?;
            Ok(self.hold(wram))
        }

        fn portable_memory_charge(portable: &Self::Portable) -> usize {
            portable.0.len()
        }

        fn now(&self) -> Moment {
            Moment(self.vtime)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    impl NesBackend<FakePortable> for FakeMachine {
        fn export_nes(
            &mut self,
            snapshot: SnapId,
            base: Option<&FakePortable>,
        ) -> Result<FakePortable, MachineError> {
            self.export(snapshot, base)
        }

        fn import_nes(&mut self, portable: &FakePortable) -> Result<SnapId, MachineError> {
            self.import(portable)
        }

        fn keep_run_capture(&mut self, frames: usize) {
            self.kept_capture.push(frames);
        }
    }

    #[test]
    fn logical_identity_projects_only_the_emulator_state() {
        let mut target = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        let snapshot = target.snapshot().expect("snapshot");
        let with_state = |state: Vec<u8>| SmbSnapshot {
            emulator_state: FakePortable(state),
            ..snapshot.clone()
        };
        let (raw, other_raw, other_logical) = (
            with_state(vec![1, 2]),
            with_state(vec![1, 3]),
            with_state(vec![4, 2]),
        );
        assert_ne!(raw, other_raw);
        assert_ne!(
            crate::search::campaign::postcard_value_sha256(&raw).unwrap(),
            crate::search::campaign::postcard_value_sha256(&other_raw).unwrap()
        );
        assert_eq!(
            raw.snapshot_identity_sha256().unwrap(),
            other_raw.snapshot_identity_sha256().unwrap()
        );
        assert_ne!(
            raw.snapshot_identity_sha256().unwrap(),
            other_logical.snapshot_identity_sha256().unwrap()
        );
        assert_eq!(
            raw.logical_identity().unwrap(),
            other_raw.logical_identity().unwrap()
        );
        assert_eq!(raw.logical_identity().unwrap().work_ram, raw.work_ram);
        let identity = |snapshot: &SmbSnapshot<FakePortable>| {
            postcard::to_allocvec(&snapshot.logical_identity().unwrap()).unwrap()
        };
        assert_eq!(identity(&raw), identity(&other_raw));
        assert_ne!(identity(&raw), identity(&other_logical));
    }

    #[test]
    fn the_archive_identity_keeps_the_execution_hash_and_the_endpoint_identity_drops_it() {
        let mut target = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        let snapshot = target.snapshot().expect("snapshot");
        let native = |execution: u8, core: u8| {
            let mut raw = b"HQNESST3".to_vec();
            raw.resize(machine::nes::STATE_HEADER_LEN, 0);
            raw[112..176].fill(execution);
            raw.extend([core; 64]);
            SmbSnapshot::<Vec<u8>> {
                emulator_state: crate::nes_backend::pack_quicknes_state(&raw).expect("pack"),
                observation: snapshot.observation.clone(),
                work_ram: (),
                room_area: snapshot.room_area,
                dead: snapshot.dead,
                failed: snapshot.failed,
                frame_key: snapshot.frame_key,
            }
        };
        let (own, foreign, changed) = (native(b'a', 1), native(b'b', 1), native(b'a', 2));
        assert_ne!(
            own.snapshot_identity_sha256().unwrap(),
            foreign.snapshot_identity_sha256().unwrap()
        );
        assert_eq!(
            own.endpoint_identity_sha256().unwrap(),
            foreign.endpoint_identity_sha256().unwrap()
        );
        assert_ne!(
            own.endpoint_identity_sha256().unwrap(),
            changed.endpoint_identity_sha256().unwrap()
        );
    }

    #[test]
    fn a_restore_succeeds_on_a_machine_that_reads_only_after_a_run() {
        let mut target = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        let genesis = target.observe();
        target.apply(&ButtonChord::new(0x10, 3));
        let snapshot = target.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.resident_memory_charge(),
            size_of::<SmbSnapshot<FakePortable>>()
                + snapshot.emulator_state_bytes_len()
                + WRAM_SIZE
        );
        let observed = target.observe();
        target.apply(&ButtonChord::new(0x20, 3));
        let action = ButtonChord::new(0x10, 4);

        target.restore(&snapshot).expect("restore");
        assert_eq!(target.observe(), observed);
        assert_eq!(target.snapshot(), Some(snapshot.clone()));
        target.apply(&action);
        let expected = target.last_action_observations().to_vec();
        assert_eq!(expected.len(), 4);

        target.restore(&snapshot).expect("restore");
        assert!(target.survives_probe(0x01, 200));
        target.apply(&action);
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.last_action_observations(), expected);
        assert_eq!(target.observe().frame_count, observed.frame_count + 4);

        target.reset();
        assert_eq!(target.observe(), genesis);
        target.apply(&action);
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.observe().frame_count, 4);
    }

    #[test]
    fn a_death_inside_a_chord_ends_the_action_at_the_death_frame() {
        let mut target = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        target.machine.kill_on_frame = Some(2);
        assert!(!target.survives_probe(0x10, 5));
        assert!(!target.is_dead());
        assert_eq!(target.execution_work(), 0);

        target.apply(&ButtonChord::new(0x10, 5));
        assert!(target.is_dead());
        assert_eq!(target.execution_work(), 2);
        assert_eq!(target.machine.kept_capture, [2]);
        let observations = target.last_action_observations();
        assert_eq!(
            observations
                .iter()
                .map(|observation| (observation.frame_count, observation.dead))
                .collect::<Vec<_>>(),
            [(1, false), (2, true)]
        );

        let stopped = target.snapshot().expect("snapshot");
        assert_eq!(target.machine.kept_capture, [2, 0]);
        assert_eq!(target.snapshot(), Some(stopped.clone()));
        let mut ended = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        ended.machine.kill_on_frame = Some(2);
        ended.apply(&ButtonChord::new(0x10, 2));
        assert_eq!(ended.snapshot(), Some(stopped));
    }
}
