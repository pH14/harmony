// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, mem::size_of, path::Path};

use crate::target::ExitKind;
use machine::{
    MachineError, SnapId, StopConditions, nes,
    quicknes::{QuickNesMachine, VideoFrame},
};
use serde::{Deserialize, Serialize};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

use crate::{
    nes_backend::{NesBackend, SnapshotState, WorkRamCopy, capture_nes, restore_nes},
    target::Target,
};

const SCREEN_PAGE_OFFSET: usize = 0x071a;
const SCREEN_X_OFFSET: usize = 0x071c;
const PLAYER_Y_OFFSET: usize = 0x00ce;
const PLAYER_ENGINE_STATE_OFFSET: usize = 0x000e;
const PLAYER_KILLED_STATE: u8 = 0x0b;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
const LEVEL_ADVANCED_FLAG_TASK: u8 = 0x05;
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
    dead: bool,
    failed: bool,
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
    pub(crate) fn room_area(&self) -> [u8; 2] {
        self.room_area
    }

    #[must_use]
    pub fn emulator_state_bytes_len(&self) -> usize {
        self.emulator_state.memory_charge()
    }

    pub(crate) fn resident_memory_charge(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.emulator_state.memory_charge())
            .saturating_add(self.work_ram.memory_charge())
    }
}

const BOOT_WALK: [ButtonChord; 2] = [
    ButtonChord {
        buttons: 0,
        hold_frames: 120,
    },
    ButtonChord {
        buttons: 0x08,
        hold_frames: 1,
    },
];

const BOOT_PLAY_WAIT_FRAMES: u32 = 900;
const OPER_MODE_OFFSET: usize = 0x0770;
const OPER_MODE_PLAY: u8 = 1;
const OPER_MODE_TASK_OFFSET: usize = 0x0772;
const OPER_MODE_TASK_PLAY: u8 = 3;
const OPER_MODE_TASK_AREA_INIT: u8 = 0;

#[derive(Debug)]
pub struct SmbTarget<M = QuickNesMachine, P = Vec<u8>>
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
    stopped_at: Option<(SnapId, ButtonChord)>,
    dead: bool,
    failed: bool,
    execution_work: u64,
}

impl<M, P> SmbTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    pub fn from_machine(machine: M) -> Result<Self, MachineError> {
        if !machine.starts_at_power_on() {
            return Err(MachineError::Backend(
                "SMB machine does not expose a game-neutral NES power-on state".to_owned(),
            ));
        }
        Self::boot(machine, true)
    }

    fn boot(mut machine: M, require_play: bool) -> Result<Self, MachineError> {
        machine::nes::run_actions(&mut machine, &BOOT_WALK)?;
        let idle = [ButtonChord {
            buttons: 0,
            hold_frames: 1,
        }];
        let mut reached_play = false;
        for _ in 0..BOOT_PLAY_WAIT_FRAMES {
            let wram = wram_array(&machine)?;
            if wram[OPER_MODE_OFFSET] == OPER_MODE_PLAY
                && wram[OPER_MODE_TASK_OFFSET] == OPER_MODE_TASK_PLAY
            {
                reached_play = true;
                break;
            }
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
        Ok(Self {
            machine,
            snapshot_base: None,
            genesis,
            genesis_observation: observation.clone(),
            action_observations: vec![observation.clone()],
            observation,
            stopped_at: None,
            dead: false,
            failed: false,
            execution_work: 0,
        })
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

    pub(crate) fn mechanical_state(&self) -> SmbMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead
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
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.dead || self.is_victory() {
            return;
        }
        let Ok(start) = self.machine.snapshot() else {
            self.failed = true;
            return;
        };
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(start);
            return;
        }
        if !self.run_staged() {
            let _ = self.machine.drop_snapshot(start);
            return;
        }
        let mut endpoint = self.wram();
        let mut prior_bucket = smb_scroll_bucket(&endpoint);
        let mut executed_frames = 0_u64;
        for wram in self
            .machine
            .frames()
            .iter()
            .take(usize::from(action.bounded_hold_frames()))
        {
            executed_frames = executed_frames.saturating_add(1);
            endpoint = *wram;
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
            if self.dead || victory {
                break;
            }
        }
        if executed_frames < self.machine.frames().len() as u64 {
            let chord =
                ButtonChord::new(action.buttons, u8::try_from(executed_frames).unwrap_or(1));
            self.stopped_at = Some((start, chord));
        } else {
            let _ = self.machine.drop_snapshot(start);
        }
        self.execution_work = self.execution_work.saturating_add(executed_frames);
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
        let Ok(emulator_state) = capture_nes(&mut self.machine, self.snapshot_base.as_ref()) else {
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
        restore_nes(&mut self.machine, &snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        let wram = match kept {
            Some(wram) => wram,
            None => wram_array(&self.machine).map_err(|error| error.to_string())?,
        };
        self.observation = snapshot.observation.materialize(wram.to_vec());
        self.action_observations = vec![self.observation.clone()];
        self.dead = snapshot.dead;
        self.failed = snapshot.failed;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        Ok(())
    }
}

impl SmbTarget<QuickNesMachine, Vec<u8>> {
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

    #[cfg(test)]
    pub(crate) fn poke_wram(&mut self, addr: usize, byte: u8) {
        self.machine.poke_wram(addr, byte);
        if let Ok(wram) = wram_array(&self.machine) {
            self.observation.wram = wram.to_vec();
            self.observation.decoded = smb_mechanical_state_from_wram(&wram);
        }
    }

    #[cfg(test)]
    pub(crate) fn loopback_for_tests(rom: &[u8]) -> Result<Self, MachineError> {
        Self::boot(QuickNesMachine::loopback_for_tests(rom)?, false)
    }
}

fn smb_scroll_bucket(wram: &[u8; WRAM_SIZE]) -> u16 {
    u16::from(wram[SCREEN_PAGE_OFFSET]) * 16 + u16::from(wram[SCREEN_X_OFFSET] / 16)
}

#[must_use]
pub fn smb_camera_pixels(wram: &[u8; WRAM_SIZE]) -> u32 {
    u32::from(wram[SCREEN_PAGE_OFFSET]) * 256 + u32::from(wram[SCREEN_X_OFFSET])
}

const PLAYER_BELOW_PLAY_AREA_PAGE: u8 = 2;

fn smb_player_is_dead(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_KILLED_STATE
        || wram[PLAYER_VERTICAL_PAGE_OFFSET] >= PLAYER_BELOW_PLAY_AREA_PAGE
}

const OPERATING_MODE_OFFSET: usize = 0x0770;
const VICTORY_OPERATING_MODE: u8 = 2;
const FINAL_WORLD_NUMBER: u8 = 7;

#[must_use]
pub fn smb_is_victory(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPERATING_MODE_OFFSET] == VICTORY_OPERATING_MODE
        && wram[WORLD_NUMBER_OFFSET] == FINAL_WORLD_NUMBER
}

fn smb_fingerprint_from_wram(wram: &[u8; WRAM_SIZE]) -> u64 {
    let screen_page = u64::from(wram[SCREEN_PAGE_OFFSET]);
    let screen_x_bucket = u64::from(wram[SCREEN_X_OFFSET] / 16);
    let player_y_bucket = u64::from(wram[PLAYER_Y_OFFSET] / 32);
    (screen_page << 8) | (screen_x_bucket << 4) | player_y_bucket
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
    let level = smb_current_level(wram);
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
        level: smb_current_level(wram),
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

pub(crate) fn smb_area_is_loading(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[OPER_MODE_OFFSET] == OPER_MODE_PLAY
        && wram[OPER_MODE_TASK_OFFSET] == OPER_MODE_TASK_AREA_INIT
}

const PLAYER_VERTICAL_PAGE_OFFSET: usize = 0x00b5;

fn smb_current_level(wram: &[u8; WRAM_SIZE]) -> u8 {
    let level = wram[LEVEL_NUMBER_OFFSET];
    if wram[FLAG_TASK_OFFSET] == LEVEL_ADVANCED_FLAG_TASK {
        level.saturating_sub(1)
    } else {
        level
    }
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        BOOT_PLAY_WAIT_FRAMES, ButtonChord, MAX_HOLD_FRAMES, OPER_MODE_OFFSET, OPER_MODE_PLAY,
        OPER_MODE_TASK_OFFSET, OPER_MODE_TASK_PLAY, PLAYER_ENGINE_STATE_OFFSET,
        PLAYER_KILLED_STATE, SCREEN_X_OFFSET, SmbSnapshot, SmbTarget, WRAM_SIZE, smb_is_victory,
        smb_mechanical_state_from_wram,
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
        target.poke_wram(0x0770, 2);
        target.poke_wram(0x075f, 7);
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
            target.poke_wram(offset, value);
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
        let observations = target.last_action_observations();
        assert_eq!(
            observations
                .iter()
                .map(|observation| (observation.frame_count, observation.dead))
                .collect::<Vec<_>>(),
            [(1, false), (2, true)]
        );

        let stopped = target.snapshot().expect("snapshot");
        assert_eq!(target.snapshot(), Some(stopped.clone()));
        let mut ended = SmbTarget::from_machine(FakeMachine::new()).expect("boot");
        ended.machine.kill_on_frame = Some(2);
        ended.apply(&ButtonChord::new(0x10, 2));
        assert_eq!(ended.snapshot(), Some(stopped));
    }
}
