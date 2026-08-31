// SPDX-License-Identifier: AGPL-3.0-or-later

//! Super Mario Bros game layer: memory decoders, input encoding, and the
//! machine-backed target adapter.
//!
//! Everything here decodes a few bytes of work RAM read through the machine
//! boundary or encodes actions as controller inputs; the emulator itself sits
//! behind [`machine::Machine`].

use std::{error::Error, path::Path, sync::Arc};

use crate::target::ExitKind;
use machine::{Machine, MachineError, SnapId, StopConditions, nes, quicknes::QuickNesMachine};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

use crate::target::Target;

const SCREEN_PAGE_OFFSET: usize = 0x071a;
const SCREEN_X_OFFSET: usize = 0x071c;
const PLAYER_Y_OFFSET: usize = 0x00ce;
const PLAYER_ENGINE_STATE_OFFSET: usize = 0x000e;
/// Player engine state the game sets when Mario is killed.
const PLAYER_KILLED_STATE: u8 = 0x0b;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
const LEVEL_ADVANCED_FLAG_TASK: u8 = 0x05;

/// A Super Mario Bros input replayed from the deterministic power-on state:
/// controller chords in execution order.
pub type SmbInput = crate::search::archive::Input<ButtonChord>;

/// Mechanical evidence captured at one NES observer event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbObservations {
    /// Number of frames actually emulated since genesis.
    pub frame_count: u64,
    /// Raw 2 KiB work RAM at a milestone crossing; otherwise empty in stored null-detector traces.
    pub wram: Vec<u8>,
    /// Route-agnostic decoded state recorded directly in the trace.
    #[serde(default)]
    pub decoded: SmbMechanicalState,
    /// Campaign milestones decoded at this event.
    #[serde(default)]
    pub milestones: SmbMilestones,
    /// Sorted work-RAM indices whose bytes changed since the previous observer event.
    pub changed_indices: Vec<u16>,
    /// Whether this event is the first observed player-death frame.
    #[serde(default)]
    pub dead: bool,
    /// Compact mechanical log line; it deliberately contains no decoded game fields.
    pub log_line: String,
}

/// Complete in-memory state needed to resume an NES prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSnapshot {
    emulator_state: SharedState,
    observation: SmbObservations,
    dead: bool,
    failed: bool,
}

const STATE_CHUNK_SIZE: usize = 512;

#[derive(Debug, Eq, PartialEq)]
struct SharedStateInner {
    chunks: Vec<Arc<[u8; STATE_CHUNK_SIZE]>>,
    len: usize,
}

/// Copy-on-write emulator bytes. Its Serde representation is deliberately
/// identical to `Vec<u8>`, so sharing is an in-memory detail and recorded
/// streams/checkpoints do not change.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedState(Arc<SharedStateInner>);

impl SharedState {
    fn from_bytes(bytes: Vec<u8>, base: Option<&Self>) -> Self {
        let mut chunks = Vec::with_capacity(bytes.len().div_ceil(STATE_CHUNK_SIZE));
        for (index, source) in bytes.chunks(STATE_CHUNK_SIZE).enumerate() {
            let mut chunk = [0_u8; STATE_CHUNK_SIZE];
            chunk[..source.len()].copy_from_slice(source);
            let shared = base
                .and_then(|state| state.0.chunks.get(index))
                .filter(|existing| existing.as_ref() == &chunk)
                .cloned()
                .unwrap_or_else(|| Arc::new(chunk));
            chunks.push(shared);
        }
        Self(Arc::new(SharedStateInner {
            chunks,
            len: bytes.len(),
        }))
    }

    fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.0.len);
        for chunk in &self.0.chunks {
            let remaining = self.0.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(STATE_CHUNK_SIZE)]);
        }
        bytes
    }
}

impl Serialize for SharedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len))?;
        for index in 0..self.0.len {
            sequence.serialize_element(
                &self.0.chunks[index / STATE_CHUNK_SIZE][index % STATE_CHUNK_SIZE],
            )?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for SharedState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(bytes, None))
    }
}

impl SmbSnapshot {
    /// Work RAM captured with the snapshot.
    #[must_use]
    pub fn wram(&self) -> &[u8] {
        &self.observation.wram
    }
}

/// Fixed boot walk from power-on to gameplay genesis, encoded as staged
/// controller inputs: the title screen settles, Start is pressed once, and
/// the pre-level sequence plays out. Target setup rather than model-visible
/// search guidance.
const BOOT_WALK: [ButtonChord; 4] = [
    ButtonChord {
        buttons: 0,
        hold_frames: 120,
    },
    ButtonChord {
        buttons: 0x08,
        hold_frames: 1,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 120,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 120,
    },
];

/// Machine-backed target used by the Super Mario Bros campaigns.
#[derive(Debug)]
pub struct SmbTarget {
    machine: QuickNesMachine,
    genesis: SnapId,
    observation: SmbObservations,
    action_observations: Vec<SmbObservations>,
    dead: bool,
    failed: bool,
    snapshot_base: Option<SharedState>,
}

impl SmbTarget {
    fn from_machine(mut machine: QuickNesMachine) -> Result<Self, MachineError> {
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&BOOT_WALK))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(power_on)?;
        let genesis = machine.snapshot()?;
        let wram = wram_array(&machine)?;
        let observation = SmbObservations {
            frame_count: 0,
            wram: wram.to_vec(),
            decoded: smb_mechanical_state_from_wram(&wram),
            milestones: smb_milestones_from_wram(&wram),
            changed_indices: Vec::new(),
            dead: false,
            log_line: "frame=0 changed=[]".to_owned(),
        };
        Ok(Self {
            machine,
            genesis,
            action_observations: vec![observation.clone()],
            observation,
            dead: false,
            failed: false,
            snapshot_base: None,
        })
    }

    /// Load SMB at gameplay genesis through the pinned native QuickNES core.
    ///
    /// This constructor is headless by contract. `core_sha256` becomes part
    /// of every snapshot compatibility header.
    ///
    /// # Errors
    ///
    /// Returns a machine error if the core, ROM, bootstrap, or snapshot fails.
    pub fn from_smb_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let machine = QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?;
        Self::from_machine(machine)
    }

    #[cfg(test)]
    pub(crate) fn loopback_for_tests(rom: &[u8]) -> Result<Self, MachineError> {
        Self::from_machine(QuickNesMachine::loopback_for_tests(rom)?)
    }

    /// Clock one fixed mask for a bounded horizon and report whether the run stays alive.
    ///
    /// This is an admission probe: it emits no observer events, consumes no
    /// randomness, and leaves the caller responsible for restoring the state it
    /// started from. It reads the same terminal condition execution reads.
    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.dead {
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
        let survived = self.probe_env(start, env);
        let _ = self.machine.drop_snapshot(start);
        survived
    }

    fn probe_env(&mut self, start: SnapId, env: Vec<ButtonChord>) -> bool {
        if self.machine.branch(start, &nes::reproducer(&env)).is_err() {
            self.failed = true;
            return false;
        }
        loop {
            match self.run_one_frame() {
                Ok(true) => {}
                Ok(false) => return true,
                Err(()) => return false,
            }
            if self.read_dead() {
                self.dead = true;
                return false;
            }
        }
    }

    /// Advance one frame of the staged environment. `Ok(true)` when a frame
    /// was emulated, `Ok(false)` at quiescence, `Err` on a crash or a machine
    /// failure. The console produces no cooperating-guest stop, so the run
    /// arms no class and the remaining reasons are unreachable.
    fn run_one_frame(&mut self) -> Result<bool, ()> {
        let deadline = machine::Moment(self.machine.now().0.saturating_add(1));
        match self.machine.run(
            StopConditions {
                deadline: Some(deadline),
                on: machine::StopMask::NONE,
            },
            None,
        ) {
            Ok(machine::StopReason::Deadline { .. }) => Ok(true),
            Ok(machine::StopReason::Quiescent { .. }) => Ok(false),
            Ok(_) => {
                self.failed = true;
                Err(())
            }
            Err(_) => {
                self.failed = true;
                Err(())
            }
        }
    }

    /// Return the latest raw work RAM without semantic decoding.
    #[must_use]
    pub fn wram(&self) -> [u8; WRAM_SIZE] {
        wram_array(&self.machine).unwrap_or([0; WRAM_SIZE])
    }

    fn read_dead(&self) -> bool {
        let engine_state = self.read_byte(PLAYER_ENGINE_STATE_OFFSET);
        let vertical_page = self.read_byte(PLAYER_VERTICAL_PAGE_OFFSET);
        engine_state == PLAYER_KILLED_STATE || vertical_page >= PLAYER_BELOW_PLAY_AREA_PAGE
    }

    fn read_byte(&self, addr: usize) -> u8 {
        self.machine
            .read(addr as u64, 1)
            .ok()
            .and_then(|bytes| bytes.first().copied())
            .unwrap_or(0)
    }

    /// Return whether execution reached the first player-death frame.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Plant one work-RAM byte, for tests that stage a game state.
    #[cfg(test)]
    pub(crate) fn poke_wram(&mut self, addr: usize, byte: u8) {
        self.machine.poke_wram(addr, byte);
    }

    /// Return whether the game is in its victory mode.
    ///
    /// Read from work RAM rather than carried as state: the operating mode
    /// stays at the victory value once reached, so a restored snapshot
    /// answers correctly without the snapshot format carrying the flag.
    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.read_byte(OPERATING_MODE_OFFSET) == VICTORY_OPERATING_MODE
            && self.read_byte(WORLD_NUMBER_OFFSET) == FINAL_WORLD_NUMBER
    }

    /// Return the total frames this instance has emulated since construction.
    ///
    /// This is deterministic work accounting over the instance's whole life,
    /// probes and bootstrap included. It is not campaign state: snapshots do
    /// not carry it and `restore` does not touch it.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.machine.now().0
    }

    /// Return every observer event emitted by the most recently applied action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[SmbObservations] {
        &self.action_observations
    }

    fn observation_from(
        &self,
        wram: &[u8; WRAM_SIZE],
        frame_count: u64,
        prior_wram: &[u8; WRAM_SIZE],
        dead: bool,
    ) -> SmbObservations {
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
        let log_line = format!("frame={frame_count} changed={changed_indices:?}");
        SmbObservations {
            frame_count,
            wram: wram.to_vec(),
            decoded: smb_mechanical_state_from_wram(wram),
            milestones: smb_milestones_from_wram(wram),
            changed_indices,
            dead,
            log_line,
        }
    }
}

fn wram_array(machine: &QuickNesMachine) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read_wram()
}

impl Target for SmbTarget {
    type Action = ButtonChord;
    type Observations = SmbObservations;
    type Snapshot = SmbSnapshot;

    fn reset(&mut self) {
        self.failed = self.machine.replay(self.genesis).is_err();
        self.snapshot_base = None;
        self.dead = false;
        let wram = self.wram();
        self.observation = self.observation_from(&wram, 0, &[0; WRAM_SIZE], false);
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.dead || self.is_victory() {
            return;
        }
        let Ok(mut prior_observed_wram) = wram_array(&self.machine) else {
            self.failed = true;
            return;
        };
        let mut prior_bucket = smb_scroll_bucket(&prior_observed_wram);
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
        let _ = self.machine.drop_snapshot(start);
        let hold_frames = action.bounded_hold_frames();
        let mut executed_frames = 0_u64;
        for _ in 0..hold_frames {
            match self.run_one_frame() {
                Ok(true) => {}
                Ok(false) => break,
                Err(()) => break,
            }
            executed_frames = executed_frames.saturating_add(1);
            let Ok(wram) = wram_array(&self.machine) else {
                self.failed = true;
                break;
            };
            let current_bucket = smb_scroll_bucket(&wram);
            self.dead = smb_player_is_dead(&wram);
            let victory = smb_is_victory(&wram);
            if current_bucket != prior_bucket || self.dead || victory {
                let observation = self.observation_from(
                    &wram,
                    self.observation.frame_count.saturating_add(executed_frames),
                    &prior_observed_wram,
                    self.dead,
                );
                prior_observed_wram = wram;
                prior_bucket = current_bucket;
                self.action_observations.push(observation);
            }
            if self.dead || victory {
                break;
            }
        }
        let endpoint_frame = self.observation.frame_count.saturating_add(executed_frames);
        let endpoint_already_recorded = self
            .action_observations
            .last()
            .is_some_and(|observation| observation.frame_count == endpoint_frame);
        if !endpoint_already_recorded {
            let wram = self.wram();
            self.action_observations.push(self.observation_from(
                &wram,
                endpoint_frame,
                &prior_observed_wram,
                self.dead,
            ));
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
        let Ok(snap) = self.machine.snapshot() else {
            self.failed = true;
            return None;
        };
        let exported = self.machine.take_snapshot(snap);
        let Ok(emulator_state) = exported else {
            self.failed = true;
            return None;
        };
        let emulator_state = SharedState::from_bytes(emulator_state, self.snapshot_base.as_ref());
        self.snapshot_base = Some(emulator_state.clone());
        Some(SmbSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            dead: self.dead,
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let emulator_state = snapshot.emulator_state.materialize();
        let imported = self.machine.import_snapshot(&emulator_state);
        let restored = self.machine.replay(imported);
        let _ = self.machine.drop_snapshot(imported);
        restored.map_err(|error| error.to_string())?;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.dead = snapshot.dead;
        self.failed = snapshot.failed;
        Ok(())
    }
}

fn smb_scroll_bucket(wram: &[u8; WRAM_SIZE]) -> u16 {
    u16::from(wram[SCREEN_PAGE_OFFSET]) * 16 + u16::from(wram[SCREEN_X_OFFSET] / 16)
}

/// Report the recorded camera position in pixels rather than 16-pixel buckets.
#[must_use]
pub fn smb_camera_pixels(wram: &[u8; WRAM_SIZE]) -> u32 {
    u32::from(wram[SCREEN_PAGE_OFFSET]) * 256 + u32::from(wram[SCREEN_X_OFFSET])
}

/// Lowest vertical page value that only occurs below the play area.
const PLAYER_BELOW_PLAY_AREA_PAGE: u8 = 2;

/// Whether Mario is dead: the kill state, or a fall below the play area,
/// which the engine state does not report before the life counter drops.
fn smb_player_is_dead(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_KILLED_STATE
        || wram[PLAYER_VERTICAL_PAGE_OFFSET] >= PLAYER_BELOW_PLAY_AREA_PAGE
}

/// Work-RAM index of the operating mode byte, `$0770`.
const OPERATING_MODE_OFFSET: usize = 0x0770;
/// Operating mode the game enters at every castle axe: the between-worlds
/// sequence everywhere except the final world, the ending there.
const VICTORY_OPERATING_MODE: u8 = 2;
/// Zero-based world number of the game's final world.
const FINAL_WORLD_NUMBER: u8 = 7;

/// Whether work RAM is in the game's victory mode: the axe sequence of the
/// final world's castle. Earlier castles enter the same operating mode and
/// then return to play, so the mode byte alone does not decide the game.
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

/// Campaign milestone ladder, accumulated over every observer event in a run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestones {
    /// Greatest 16-pixel scroll bucket observed while the RAM level tuple is 1-1.
    pub max_1_1_scroll_bucket: u16,
    /// Whether the 1-1 flag-task byte was observed active.
    pub reached_1_1_flag: bool,
    /// Whether the RAM level tuple reached 1-2.
    pub reached_1_2: bool,
    /// Whether the RAM level tuple advanced beyond 1-2.
    pub reached_onward: bool,
}

/// First deterministic execution reaching each milestone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneTimes {
    /// First execution reaching a nonzero 1-1 scroll bucket.
    pub progress_into_1_1: Option<u64>,
    /// First execution observing the 1-1 flag task.
    pub flag_1_1: Option<u64>,
    /// First execution observing level 1-2.
    pub level_1_2: Option<u64>,
    /// First execution observing a level beyond 1-2.
    pub onward: Option<u64>,
}

/// First testcase reaching each milestone, retained for films.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneInputs {
    /// First testcase making nonzero progress into 1-1.
    pub progress_into_1_1: Option<SmbInput>,
    /// First testcase observing the 1-1 flag task.
    pub flag_1_1: Option<SmbInput>,
    /// First testcase observing level 1-2.
    pub level_1_2: Option<SmbInput>,
    /// First testcase observing a level beyond 1-2.
    pub onward: Option<SmbInput>,
}

/// Maximum route-agnostic mechanical position observed at any emulated frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbProgressWatermark {
    /// Zero-based world number.
    pub world: u8,
    /// Zero-based level number within the world.
    pub level: u8,
    /// Current 16-pixel horizontal progress bucket.
    pub progress: u16,
}

/// Decode the milestone metrics from SMB work RAM.
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

/// Route-agnostic mechanical state available to completion search and evaluation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbMechanicalState {
    /// Zero-based world number decoded from work RAM.
    pub world: u8,
    /// Zero-based level number within the current world.
    pub level: u8,
    /// Current 16-pixel horizontal progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state, without interpreting a route.
    pub player_engine_state: u8,
    /// Whether the target's first-death state is active.
    pub dead: bool,
    /// Whether the level-end flag task is active.
    pub flag_active: bool,
}

/// Decode the bounded mechanical state used by generic completion search.
#[must_use]
pub fn smb_mechanical_state_from_wram(wram: &[u8; WRAM_SIZE]) -> SmbMechanicalState {
    SmbMechanicalState {
        world: wram[WORLD_NUMBER_OFFSET],
        level: smb_current_level(wram),
        progress: smb_scroll_bucket(wram),
        player_y_bucket: wram[PLAYER_Y_OFFSET] / 16,
        player_engine_state: wram[PLAYER_ENGINE_STATE_OFFSET],
        dead: smb_player_is_dead(wram),
        flag_active: wram[FLAG_TASK_OFFSET] != 0,
    }
}

/// Work-RAM index of the player's vertical page, which rises as `$00ce` wraps
/// below the play area.
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
    use super::{
        ButtonChord, MAX_HOLD_FRAMES, STATE_CHUNK_SIZE, SharedState, SmbTarget, WRAM_SIZE,
        smb_is_victory, smb_mechanical_state_from_wram,
    };
    use crate::target::Target;
    use std::sync::Arc;

    #[test]
    fn shared_state_preserves_the_flat_byte_wire_format() {
        let bytes = (0_u16..333).map(|value| value as u8).collect::<Vec<_>>();
        let state = SharedState::from_bytes(bytes.clone(), None);
        assert_eq!(state.materialize(), bytes);
        assert_eq!(
            postcard::to_allocvec(&state).expect("encode shared state"),
            postcard::to_allocvec(&bytes).expect("encode flat state")
        );
        assert_eq!(
            serde_json::to_vec(&state).expect("encode shared JSON"),
            serde_json::to_vec(&bytes).expect("encode flat JSON")
        );
        let decoded: SharedState =
            postcard::from_bytes(&postcard::to_allocvec(&state).expect("encode round-trip state"))
                .expect("decode shared state");
        assert_eq!(decoded, state);
    }

    #[test]
    fn shared_state_reuses_only_byte_identical_chunks() {
        let base = SharedState::from_bytes(vec![1_u8; STATE_CHUNK_SIZE * 3], None);
        let mut changed = base.materialize();
        changed[STATE_CHUNK_SIZE + 1] = 2;
        let child = SharedState::from_bytes(changed, Some(&base));
        assert!(Arc::ptr_eq(&base.0.chunks[0], &child.0.chunks[0]));
        assert!(!Arc::ptr_eq(&base.0.chunks[1], &child.0.chunks[1]));
        assert!(Arc::ptr_eq(&base.0.chunks[2], &child.0.chunks[2]));
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
        let frames_before = restored.frames_clocked();
        restored.apply(&ButtonChord::new(0x01, 10));
        assert_eq!(restored.frames_clocked(), frames_before);
        assert!(restored.last_action_observations().is_empty());
    }
}
