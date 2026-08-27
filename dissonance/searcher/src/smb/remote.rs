// SPDX-License-Identifier: AGPL-3.0-or-later

//! Chord-boundary SMB target over the consonance control protocol.
//!
//! The guest consumes one complete [`ButtonChord`] and yields before its next
//! payload fetch. This target therefore observes exactly one endpoint per
//! action, validates the guest-reported frame delta, and reads the complete
//! WRAM mirror the agent published during setup. Archive/search policy stays
//! above this module and sees the same actions and observation types as the
//! in-process target.

use std::{collections::VecDeque, error::Error, io::Read, io::Write};

use machine::{
    Machine, MachineError, Moment, SnapId, StopConditions, StopMask, StopReason,
    control::{RestoreCounters, SocketMachine},
    nes,
};
use serde::{Deserialize, Serialize};

use crate::{
    smb::target::{
        BOOT_WALK, ButtonChord, SmbCampaignTarget, SmbObservations, SmbSnapshot,
        SmbSnapshotEvidence, SmbTarget, WRAM_SIZE, smb_fingerprint_from_wram, smb_is_victory,
        smb_mechanical_state_from_wram, smb_milestones_from_wram, smb_player_is_dead,
    },
    target::{ExitKind, SnapshotRestoreCounters, Target},
};

/// Resident continuation handles kept in one VMM session. Older archive
/// snapshots remain restorable through their durable genesis lineage.
const LIVE_SNAPSHOT_LIMIT: usize = 1_024;

/// Control-machine additions the remote SMB adapter needs beyond the mirrored
/// core verb set. The production implementation is [`SocketMachine`]; the
/// trait keeps target logic portable and directly testable.
pub trait GuestControlMachine: Machine {
    /// Guest-reported cumulative emulated frame.
    fn logical_frame(&self) -> Moment;
    /// Guest-published host-readable WRAM window.
    fn wram_window(&self) -> Option<(u64, u32)>;
    /// Mark the gameplay genesis handle for restore accounting.
    fn mark_genesis(&mut self, snap: SnapId) -> Result<(), MachineError>;
    /// Genesis versus continuation restore counts.
    fn restore_counters(&self) -> RestoreCounters;
    /// Canonical whole-machine state hash at the current stopped boundary.
    fn state_hash(&mut self) -> Result<[u8; 32], MachineError>;
}

impl<S: Read + Write> GuestControlMachine for SocketMachine<S> {
    fn logical_frame(&self) -> Moment {
        SocketMachine::logical_frame(self)
    }

    fn wram_window(&self) -> Option<(u64, u32)> {
        SocketMachine::wram_window(self)
    }

    fn mark_genesis(&mut self, snap: SnapId) -> Result<(), MachineError> {
        SocketMachine::mark_genesis(self, snap)
    }

    fn restore_counters(&self) -> RestoreCounters {
        SocketMachine::restore_counters(self)
    }

    fn state_hash(&mut self) -> Result<[u8; 32], MachineError> {
        SocketMachine::state_hash(self)
    }
}

/// Restorable remote SMB state. `handle` is deliberately session-local and is
/// skipped in persisted checkpoints; `lineage` reconstructs the same state from
/// gameplay genesis in a fresh session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSmbSnapshot {
    #[serde(skip)]
    handle: Option<SnapId>,
    lineage: Vec<ButtonChord>,
    observation: SmbObservations,
    state_hash: [u8; 32],
    dead: bool,
    failed: bool,
}

impl RemoteSmbSnapshot {
    /// WRAM bytes captured at this chord boundary.
    #[must_use]
    pub fn wram(&self) -> &[u8] {
        &self.observation.wram
    }

    /// Whether this value still has a fast session-local snapshot handle.
    #[must_use]
    pub fn has_live_handle(&self) -> bool {
        self.handle.is_some()
    }

    /// Canonical whole-machine state hash captured at this chord boundary.
    #[must_use]
    pub fn state_hash(&self) -> [u8; 32] {
        self.state_hash
    }
}

impl SmbSnapshotEvidence for RemoteSmbSnapshot {
    fn snapshot_wram(&self) -> &[u8] {
        self.wram()
    }
}

/// SMB target driven through a cooperating guest's control session.
#[derive(Debug)]
pub struct RemoteSmbTarget<M: GuestControlMachine> {
    machine: M,
    genesis: SnapId,
    genesis_frame: Moment,
    frames_clocked: u64,
    wram_gpa: u64,
    wram: [u8; WRAM_SIZE],
    observation: SmbObservations,
    action_observations: Vec<SmbObservations>,
    lineage: Vec<ButtonChord>,
    live_snapshots: VecDeque<SnapId>,
    dead: bool,
    failed: bool,
}

impl<M: GuestControlMachine> RemoteSmbTarget<M> {
    /// Take over a negotiated machine, stop at `setup_complete`, execute the
    /// same fixed boot walk as the in-process target, and seal gameplay genesis.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/malformed WRAM publication, a wrong stop,
    /// or any control/emulator failure.
    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let stop = machine.run(
            StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(machine::class_bit::SNAPSHOT_POINT),
            },
            None,
        )?;
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return Err(MachineError::Backend(format!(
                "guest did not stop at setup_complete: {stop:?}"
            )));
        }
        let (wram_gpa, wram_len) = machine
            .wram_window()
            .ok_or_else(|| MachineError::Backend("guest did not publish a WRAM window".into()))?;
        if wram_len as usize != WRAM_SIZE {
            return Err(MachineError::Backend(format!(
                "guest WRAM window is {wram_len} bytes, expected {WRAM_SIZE}"
            )));
        }

        let setup = machine.snapshot()?;
        machine.branch(setup, &nes::reproducer(&BOOT_WALK))?;
        let boot_frames = BOOT_WALK.iter().fold(0_u64, |total, action| {
            total.saturating_add(u64::from(action.bounded_hold_frames()))
        });
        let deadline = machine.logical_frame().0.saturating_add(boot_frames);
        let stop = machine.run(
            StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            None,
        )?;
        if stop
            != (StopReason::Deadline {
                vtime: Moment(deadline),
            })
        {
            return Err(MachineError::Backend(format!(
                "guest boot walk stopped unexpectedly: {stop:?}"
            )));
        }
        machine.drop_snapshot(setup)?;
        let genesis = machine.snapshot()?;
        machine.mark_genesis(genesis)?;
        let genesis_frame = machine.logical_frame();
        let wram = read_wram(&machine, wram_gpa)?;
        let observation = observation(&wram, 0, &[0; WRAM_SIZE], false);
        Ok(Self {
            machine,
            genesis,
            genesis_frame,
            frames_clocked: boot_frames,
            wram_gpa,
            wram,
            action_observations: vec![observation.clone()],
            observation,
            lineage: Vec::new(),
            live_snapshots: VecDeque::new(),
            dead: false,
            failed: false,
        })
    }

    /// Return the negotiated machine stopped at gameplay genesis together with
    /// the held genesis handle. M5's portability driver uses this to stage a
    /// multi-chord tape whose midpoint can continue without a restore; normal
    /// campaigns keep using the target abstraction.
    pub fn into_genesis_machine(self) -> (M, SnapId) {
        (self.machine, self.genesis)
    }

    /// Complete WRAM from the last successful chord-boundary read.
    #[must_use]
    pub fn wram(&self) -> [u8; WRAM_SIZE] {
        self.wram
    }

    /// Observer events emitted by the most recently applied chord.
    #[must_use]
    pub fn last_action_observations(&self) -> &[SmbObservations] {
        &self.action_observations
    }

    /// Whether the endpoint is a player-death state.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Whether the endpoint is the final victory state.
    #[must_use]
    pub fn is_victory(&self) -> bool {
        smb_is_victory(&self.wram)
    }

    /// Total guest frames emulated over this target's lifetime.
    ///
    /// Snapshot restore rewinds the guest frame counter but never this work
    /// counter, matching the in-process target and campaign accounting seam.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.frames_clocked
    }

    /// Current restore accounting from the control session.
    #[must_use]
    pub fn restore_counters(&self) -> RestoreCounters {
        self.machine.restore_counters()
    }

    /// Canonical whole-machine state hash at the current chord boundary.
    ///
    /// # Errors
    ///
    /// Returns a control transport or server hash error.
    pub fn state_hash(&mut self) -> Result<[u8; 32], MachineError> {
        self.machine.state_hash()
    }

    fn apply_checked(&mut self, action: &ButtonChord) -> Result<(), MachineError> {
        let prior = self.wram;
        let start_frame = self.machine.logical_frame();
        let start = self.machine.snapshot()?;
        if let Err(error) = self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
        {
            let _ = self.machine.drop_snapshot(start);
            return Err(error);
        }
        self.machine.drop_snapshot(start)?;
        let expected = start_frame
            .0
            .saturating_add(u64::from(action.bounded_hold_frames()));
        let stop = self.machine.run(
            StopConditions {
                deadline: Some(Moment(expected)),
                on: StopMask::NONE.arm(machine::class_bit::SNAPSHOT_POINT),
            },
            None,
        )?;
        let end_frame = self.machine.logical_frame().0;
        let Some(executed_frames) = end_frame.checked_sub(start_frame.0).filter(|executed| {
            *executed > 0 && *executed <= u64::from(action.bounded_hold_frames())
        }) else {
            return Err(MachineError::Backend(format!(
                "guest reported invalid chord frame range: start {}, end {end_frame}, maximum \
                 {expected}",
                start_frame.0
            )));
        };
        let wram = read_wram(&self.machine, self.wram_gpa)?;
        self.dead = smb_player_is_dead(&wram);
        let victory = smb_is_victory(&wram);
        let complete = stop
            == (StopReason::Deadline {
                vtime: Moment(expected),
            });
        let early_terminal = matches!(stop, StopReason::SnapshotPoint { .. })
            && end_frame < expected
            && (self.dead || victory);
        if !complete && !early_terminal {
            return Err(MachineError::Backend(format!(
                "chord stopped at an invalid lifecycle boundary: expected frame {expected}, got \
                 {stop:?} at frame {end_frame}, dead={}, victory={victory}",
                self.dead
            )));
        }
        self.frames_clocked = self.frames_clocked.saturating_add(executed_frames);
        let frame_count = self.observation.frame_count.saturating_add(executed_frames);
        let endpoint = observation(&wram, frame_count, &prior, self.dead);
        self.wram = wram;
        self.action_observations = vec![endpoint.clone()];
        self.observation = endpoint;
        self.lineage.push(*action);
        Ok(())
    }

    fn restore_snapshot(&mut self, snapshot: &RemoteSmbSnapshot) -> Result<(), MachineError> {
        if snapshot.lineage.is_empty() {
            self.machine.replay(self.genesis)?;
        } else {
            match snapshot
                .handle
                .filter(|handle| self.live_snapshots.contains(handle))
            {
                Some(handle) => self.machine.replay(handle)?,
                None => {
                    self.machine
                        .branch(self.genesis, &nes::reproducer(&snapshot.lineage))?;
                    let frames = snapshot.lineage.iter().fold(0_u64, |total, action| {
                        total.saturating_add(u64::from(action.bounded_hold_frames()))
                    });
                    let deadline = self.genesis_frame.0.saturating_add(frames);
                    let stop = self.machine.run(
                        StopConditions {
                            deadline: Some(Moment(deadline)),
                            on: StopMask::NONE,
                        },
                        None,
                    )?;
                    if stop
                        != (StopReason::Deadline {
                            vtime: Moment(deadline),
                        })
                    {
                        return Err(MachineError::Backend(format!(
                            "lineage reconstruction stopped unexpectedly: {stop:?}"
                        )));
                    }
                    self.frames_clocked = self.frames_clocked.saturating_add(frames);
                }
            }
        }
        let actual = read_wram(&self.machine, self.wram_gpa)?;
        if actual.as_slice() != snapshot.wram() {
            return Err(MachineError::Backend(
                "restored WRAM differs from the snapshot evidence".into(),
            ));
        }
        if self.machine.state_hash()? != snapshot.state_hash {
            return Err(MachineError::Backend(
                "restored whole-state hash differs from the snapshot evidence".into(),
            ));
        }
        self.observation = snapshot.observation.clone();
        self.wram = actual;
        self.action_observations = vec![self.observation.clone()];
        self.lineage.clone_from(&snapshot.lineage);
        self.dead = snapshot.dead;
        self.failed = snapshot.failed;
        Ok(())
    }

    fn reset_checked(&mut self) -> Result<(), MachineError> {
        self.machine.replay(self.genesis)?;
        let wram = read_wram(&self.machine, self.wram_gpa)?;
        self.wram = wram;
        self.dead = false;
        self.lineage.clear();
        self.observation = observation(&wram, 0, &[0; WRAM_SIZE], false);
        self.action_observations = vec![self.observation.clone()];
        Ok(())
    }
}

#[cfg(unix)]
impl RemoteSmbTarget<SocketMachine<std::os::unix::net::UnixStream>> {
    /// Connect to a control socket and initialize gameplay genesis.
    ///
    /// # Errors
    ///
    /// Returns a socket, protocol, guest setup, or boot-walk error.
    pub fn connect(path: impl AsRef<std::path::Path>) -> Result<Self, MachineError> {
        Self::from_machine(SocketMachine::connect(path)?)
    }
}

impl<M: GuestControlMachine> Target for RemoteSmbTarget<M> {
    type Action = ButtonChord;
    type Observations = SmbObservations;
    type Snapshot = RemoteSmbSnapshot;

    fn reset(&mut self) {
        self.failed = self.reset_checked().is_err();
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.dead || self.is_victory() {
            return;
        }
        if self.apply_checked(action).is_err() {
            self.failed = true;
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        smb_fingerprint_from_wram(&self.wram)
    }

    fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        match self.machine.snapshot() {
            Ok(handle) => {
                let state_hash = match self.machine.state_hash() {
                    Ok(hash) => hash,
                    Err(_) => {
                        let _ = self.machine.drop_snapshot(handle);
                        self.failed = true;
                        return None;
                    }
                };
                self.live_snapshots.push_back(handle);
                if self.live_snapshots.len() > LIVE_SNAPSHOT_LIMIT {
                    let Some(evicted) = self.live_snapshots.pop_front() else {
                        self.failed = true;
                        return None;
                    };
                    if self.machine.drop_snapshot(evicted).is_err() {
                        self.failed = true;
                        return None;
                    }
                }
                Some(RemoteSmbSnapshot {
                    handle: Some(handle),
                    lineage: self.lineage.clone(),
                    observation: self.observation.clone(),
                    state_hash,
                    dead: self.dead,
                    failed: self.failed,
                })
            }
            Err(_) => {
                self.failed = true;
                None
            }
        }
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        self.restore_snapshot(snapshot)
            .map_err(|error| error.to_string().into())
    }
}

impl<M> SmbCampaignTarget for RemoteSmbTarget<M>
where
    M: GuestControlMachine + Send,
{
    fn campaign_wram(&self) -> [u8; WRAM_SIZE] {
        self.wram()
    }

    fn campaign_action_observations(&self) -> &[SmbObservations] {
        self.last_action_observations()
    }

    fn campaign_is_dead(&self) -> bool {
        self.is_dead()
    }

    fn campaign_is_victory(&self) -> bool {
        self.is_victory()
    }

    fn campaign_frames_clocked(&self) -> u64 {
        self.frames_clocked()
    }

    fn campaign_survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        let mut remaining = frames;
        while remaining > 0 && !self.is_dead() && self.exit_kind() == ExitKind::Ok {
            let hold = remaining.min(u16::from(machine::nes::MAX_HOLD_FRAMES));
            self.apply(&ButtonChord::new(buttons, u8::try_from(hold).unwrap_or(1)));
            remaining -= hold;
        }
        !self.is_dead() && self.exit_kind() == ExitKind::Ok
    }

    fn campaign_restore_counters(&self) -> SnapshotRestoreCounters {
        let counters = self.restore_counters();
        SnapshotRestoreCounters {
            genesis: counters.genesis,
            continuation: counters.continuation,
        }
    }
}

/// Snapshot pair for the live guest and the independent in-process emulator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DifferentialSmbSnapshot {
    remote: RemoteSmbSnapshot,
    local: SmbSnapshot,
}

impl DifferentialSmbSnapshot {
    /// Number of actions from gameplay genesis to this branch point.
    #[must_use]
    pub fn lineage_len(&self) -> usize {
        self.remote.lineage.len()
    }

    /// Canonical whole-machine hash captured at this branch point.
    #[must_use]
    pub fn state_hash(&self) -> [u8; 32] {
        self.remote.state_hash()
    }
}

/// Hash evidence for one uninterrupted-versus-restored continuation check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContinuationHashEvidence {
    /// Whole-machine hash at the sampled branch point.
    pub branch_state_hash: [u8; 32],
    /// Whole-machine hash after every compared chord.
    pub chord_state_hashes: Vec<[u8; 32]>,
}

impl SmbSnapshotEvidence for DifferentialSmbSnapshot {
    fn snapshot_wram(&self) -> &[u8] {
        self.remote.wram()
    }
}

/// Production M2 target that compares the guest build and in-process TetaNES
/// at every complete chord boundary.
#[derive(Debug)]
pub struct DifferentialSmbTarget<M: GuestControlMachine> {
    remote: RemoteSmbTarget<M>,
    local: SmbTarget,
    diverged: bool,
}

impl<M: GuestControlMachine> DifferentialSmbTarget<M> {
    /// Build both independent emulator compositions over the same ROM.
    ///
    /// # Errors
    ///
    /// Returns an emulator/control setup error or an initial WRAM mismatch.
    pub fn from_machine(machine: M, rom: &[u8]) -> Result<Self, MachineError> {
        let remote = RemoteSmbTarget::from_machine(machine)?;
        let local = SmbTarget::from_smb_rom_bytes_headless(rom)?;
        if remote.wram() != local.wram() {
            return Err(MachineError::Backend(
                "guest and in-process TetaNES disagree at gameplay genesis".into(),
            ));
        }
        Ok(Self {
            remote,
            local,
            diverged: false,
        })
    }

    fn agrees(&self) -> bool {
        self.remote.wram() == self.local.wram()
            && self.remote.is_dead() == self.local.is_dead()
            && self.remote.is_victory() == self.local.is_victory()
            && self.remote.exit_kind() == self.local.exit_kind()
    }

    /// Run a suffix without interruption, restore its current branch point,
    /// and require the repeated suffix to reproduce every chord state hash.
    ///
    /// The target must already be positioned at the branch point to sample.
    /// A fresh paired snapshot captures that exact state; the first path runs
    /// directly onward, while the second begins only after restoring the pair.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty suffix, snapshot/control failure,
    /// cross-build divergence, or the first differing replayed chord hash.
    pub fn verify_current_continuation(
        &mut self,
        actions: &[ButtonChord],
    ) -> Result<ContinuationHashEvidence, MachineError> {
        if actions.is_empty() {
            return Err(MachineError::Backend(
                "continuation hash oracle requires at least one chord".into(),
            ));
        }
        let branch = self.snapshot().ok_or_else(|| {
            MachineError::Backend("continuation hash oracle could not snapshot the branch".into())
        })?;
        let mut uninterrupted = Vec::with_capacity(actions.len());
        for action in actions {
            self.apply(action);
            if self.diverged || self.remote.exit_kind() != ExitKind::Ok {
                return Err(MachineError::Backend(
                    "continuation hash oracle failed on the uninterrupted path".into(),
                ));
            }
            uninterrupted.push(self.remote.state_hash()?);
            if self.remote.is_dead() || self.remote.is_victory() {
                break;
            }
        }

        self.restore(&branch).map_err(|error| {
            MachineError::Backend(format!(
                "continuation hash oracle could not restore its branch: {error}"
            ))
        })?;
        for (index, (action, expected)) in actions.iter().zip(&uninterrupted).enumerate() {
            self.apply(action);
            if self.diverged || self.remote.exit_kind() != ExitKind::Ok {
                return Err(MachineError::Backend(format!(
                    "restored continuation failed at chord {index}"
                )));
            }
            let actual = self.remote.state_hash()?;
            if actual != *expected {
                return Err(MachineError::Backend(format!(
                    "restored continuation state hash differs at chord {index}"
                )));
            }
        }
        Ok(ContinuationHashEvidence {
            branch_state_hash: branch.state_hash(),
            chord_state_hashes: uninterrupted,
        })
    }
}

#[cfg(unix)]
impl DifferentialSmbTarget<SocketMachine<std::os::unix::net::UnixStream>> {
    /// Connect the guest target and initialize the independent local target.
    ///
    /// # Errors
    ///
    /// Returns a socket, protocol, guest setup, local emulator, or differential
    /// mismatch error.
    pub fn connect(path: impl AsRef<std::path::Path>, rom: &[u8]) -> Result<Self, MachineError> {
        Self::from_machine(SocketMachine::connect(path)?, rom)
    }
}

impl<M: GuestControlMachine> Target for DifferentialSmbTarget<M> {
    type Action = ButtonChord;
    type Observations = SmbObservations;
    type Snapshot = DifferentialSmbSnapshot;

    fn reset(&mut self) {
        self.remote.reset();
        self.local.reset();
        self.diverged = !self.agrees();
    }

    fn apply(&mut self, action: &Self::Action) {
        if self.diverged {
            return;
        }
        self.remote.apply(action);
        self.local.apply(action);
        self.diverged = !self.agrees();
    }

    fn observe(&self) -> Self::Observations {
        self.remote.observe()
    }

    fn fingerprint(&self) -> u64 {
        self.remote.fingerprint()
    }

    fn exit_kind(&self) -> ExitKind {
        if self.diverged {
            ExitKind::Crash
        } else {
            self.remote.exit_kind()
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        if self.diverged {
            return None;
        }
        Some(DifferentialSmbSnapshot {
            remote: self.remote.snapshot()?,
            local: self.local.snapshot()?,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        self.remote.restore(&snapshot.remote)?;
        self.local.restore(&snapshot.local)?;
        self.diverged = !self.agrees();
        if self.diverged {
            return Err("guest and in-process TetaNES diverged after restore".into());
        }
        Ok(())
    }
}

impl<M> SmbCampaignTarget for DifferentialSmbTarget<M>
where
    M: GuestControlMachine + Send,
{
    fn campaign_wram(&self) -> [u8; WRAM_SIZE] {
        self.remote.wram()
    }

    fn campaign_action_observations(&self) -> &[SmbObservations] {
        self.remote.last_action_observations()
    }

    fn campaign_is_dead(&self) -> bool {
        self.remote.is_dead()
    }

    fn campaign_is_victory(&self) -> bool {
        self.remote.is_victory()
    }

    fn campaign_frames_clocked(&self) -> u64 {
        self.remote.frames_clocked()
    }

    fn campaign_survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        let action = ButtonChord::new(buttons, u8::try_from(frames).unwrap_or(u8::MAX));
        self.apply(&action);
        !self.campaign_is_dead() && self.exit_kind() == ExitKind::Ok
    }

    fn campaign_restore_counters(&self) -> SnapshotRestoreCounters {
        self.remote.campaign_restore_counters()
    }

    fn campaign_diverged(&self) -> bool {
        self.diverged
    }
}

fn read_wram<M: GuestControlMachine>(
    machine: &M,
    gpa: u64,
) -> Result<[u8; WRAM_SIZE], MachineError> {
    let bytes = machine.read(gpa, WRAM_SIZE as u32)?;
    bytes
        .try_into()
        .map_err(|_| MachineError::Backend("control server returned short WRAM".into()))
}

fn observation(
    wram: &[u8; WRAM_SIZE],
    frame_count: u64,
    prior: &[u8; WRAM_SIZE],
    dead: bool,
) -> SmbObservations {
    let changed_indices = wram
        .iter()
        .zip(prior)
        .enumerate()
        .filter_map(|(index, (current, previous))| {
            (current != previous)
                .then(|| u16::try_from(index).ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    SmbObservations {
        frame_count,
        wram: wram.to_vec(),
        decoded: smb_mechanical_state_from_wram(wram),
        milestones: smb_milestones_from_wram(wram),
        changed_indices: changed_indices.clone(),
        dead,
        log_line: format!("frame={frame_count} changed={changed_indices:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::BTreeMap};

    use machine::{
        Answer, Machine, Reproducer,
        nes::{NesMachine, RenderMode},
    };

    use super::*;
    use crate::smb::target::SmbTarget;
    use sha2::{Digest, Sha256};

    #[derive(Debug)]
    struct InProcessControl {
        machine: NesMachine,
        counters: RestoreCounters,
        genesis: Option<SnapId>,
        logical_frame: Moment,
        snapshot_frames: BTreeMap<SnapId, Moment>,
        reported_deadline_offset: i64,
        published_wram_len: u32,
        read_calls: Cell<u64>,
        fail_read_call: Option<u64>,
        state_hash_calls: u64,
        corrupt_state_hash_call: Option<u64>,
        early_yield_after: Option<u64>,
        early_yield_is_terminal: bool,
    }

    impl Machine for InProcessControl {
        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            let snap = self.machine.snapshot()?;
            self.snapshot_frames.insert(snap, self.logical_frame);
            Ok(snap)
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.machine.drop_snapshot(snap)?;
            self.snapshot_frames.remove(&snap);
            Ok(())
        }

        fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
            self.machine.branch(snap, env)?;
            self.logical_frame = *self
                .snapshot_frames
                .get(&snap)
                .ok_or_else(|| MachineError::Backend("unknown snapshot clock".into()))?;
            if self.genesis == Some(snap) {
                self.counters.genesis = self.counters.genesis.saturating_add(1);
            } else {
                self.counters.continuation = self.counters.continuation.saturating_add(1);
            }
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.machine.replay(snap)?;
            self.logical_frame = *self
                .snapshot_frames
                .get(&snap)
                .ok_or_else(|| MachineError::Backend("unknown snapshot clock".into()))?;
            if self.genesis == Some(snap) {
                self.counters.genesis = self.counters.genesis.saturating_add(1);
            } else {
                self.counters.continuation = self.counters.continuation.saturating_add(1);
            }
            Ok(())
        }

        fn run(
            &mut self,
            until: StopConditions,
            resolve: Option<&Answer>,
        ) -> Result<StopReason, MachineError> {
            // Synthesize the guest's setup lifecycle point before any payload.
            if until.on.armed(machine::class_bit::SNAPSHOT_POINT) && self.machine.now() == Moment(0)
            {
                return Ok(StopReason::SnapshotPoint { vtime: Moment(0) });
            }
            if until.on.armed(machine::class_bit::SNAPSHOT_POINT)
                && let Some(frames) = self.early_yield_after.take()
            {
                let target = self.machine.now().0.saturating_add(frames);
                let stop = self.machine.run(
                    StopConditions {
                        deadline: Some(Moment(target)),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(MachineError::Backend(
                        "early-yield fixture did not reach its frame".into(),
                    ));
                }
                self.logical_frame = Moment(self.logical_frame.0.saturating_add(frames));
                if self.early_yield_is_terminal {
                    // The fixture plants the observation layer's killed-state byte.
                    self.machine.poke_wram(0x000e, 0x0b);
                }
                return Ok(StopReason::SnapshotPoint {
                    vtime: self.logical_frame,
                });
            }
            let logical_start = self.logical_frame;
            let lifetime_start = self.machine.now();
            let translated = StopConditions {
                deadline: until.deadline.map(|deadline| {
                    Moment(
                        lifetime_start
                            .0
                            .saturating_add(deadline.0.saturating_sub(logical_start.0)),
                    )
                }),
                on: until.on,
            };
            let stop = self.machine.run(translated, resolve)?;
            let elapsed = self.machine.now().0.saturating_sub(lifetime_start.0);
            self.logical_frame = Moment(logical_start.0.saturating_add(elapsed));
            Ok(match stop {
                StopReason::Deadline { .. } => StopReason::Deadline {
                    vtime: Moment(
                        self.logical_frame
                            .0
                            .saturating_add_signed(self.reported_deadline_offset),
                    ),
                },
                other => other,
            })
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            let call = self.read_calls.get().saturating_add(1);
            self.read_calls.set(call);
            if self.fail_read_call == Some(call) {
                return Err(MachineError::Backend("planted WRAM read failure".into()));
            }
            self.machine.read(addr, len)
        }
    }

    impl GuestControlMachine for InProcessControl {
        fn logical_frame(&self) -> Moment {
            self.logical_frame
        }

        fn wram_window(&self) -> Option<(u64, u32)> {
            Some((0, self.published_wram_len))
        }

        fn mark_genesis(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.genesis = Some(snap);
            Ok(())
        }

        fn restore_counters(&self) -> RestoreCounters {
            self.counters
        }

        fn state_hash(&mut self) -> Result<[u8; 32], MachineError> {
            let snap = self.machine.snapshot()?;
            let bytes = self.machine.export_snapshot(snap)?;
            self.machine.drop_snapshot(snap)?;
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            hasher.update(self.logical_frame.0.to_le_bytes());
            let mut digest: [u8; 32] = hasher.finalize().into();
            self.state_hash_calls = self.state_hash_calls.saturating_add(1);
            if self.corrupt_state_hash_call == Some(self.state_hash_calls) {
                digest[0] ^= 0x80;
            }
            Ok(digest)
        }
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

    fn input_sensitive_nrom() -> Vec<u8> {
        let mut rom = synthetic_nrom();
        let prg = &mut rom[16..16 + (16 * 1024)];
        let program = [
            0xa9, 0x01, 0x8d, 0x16, 0x40, 0xa9, 0x00, 0x8d, 0x16, 0x40, 0xad, 0x16, 0x40, 0x29,
            0x01, 0x8d, 0x1a, 0x07, 0x4c, 0x00, 0x80,
        ];
        prg[..program.len()].copy_from_slice(&program);
        rom
    }

    fn control(rom: &[u8]) -> InProcessControl {
        InProcessControl {
            machine: NesMachine::from_rom_bytes(rom, RenderMode::Neither).unwrap(),
            counters: RestoreCounters::default(),
            genesis: None,
            logical_frame: Moment(0),
            snapshot_frames: BTreeMap::new(),
            reported_deadline_offset: 0,
            published_wram_len: WRAM_SIZE as u32,
            read_calls: Cell::new(0),
            fail_read_call: None,
            state_hash_calls: 0,
            corrupt_state_hash_call: None,
            early_yield_after: None,
            early_yield_is_terminal: false,
        }
    }

    #[test]
    fn production_differential_compares_every_chord_and_restore() {
        let rom = synthetic_nrom();
        let mut target = DifferentialSmbTarget::from_machine(control(&rom), &rom).unwrap();
        target.apply(&ButtonChord::new(0x81, 4));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        let snapshot = target.snapshot().unwrap();
        target.apply(&ButtonChord::new(0, 2));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        target.restore(&snapshot).unwrap();
        assert_eq!(target.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn production_differential_rejects_a_component_rom_mismatch() {
        let remote_rom = synthetic_nrom();
        let local_rom = input_sensitive_nrom();
        let mut target =
            DifferentialSmbTarget::from_machine(control(&remote_rom), &local_rom).unwrap();
        target.apply(&ButtonChord::new(0x01, 4));
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert!(target.snapshot().is_none());
    }

    #[test]
    fn production_continuation_oracle_catches_replayed_chord_hash_drift() {
        let rom = synthetic_nrom();
        let mut positive = DifferentialSmbTarget::from_machine(control(&rom), &rom).unwrap();
        positive.apply(&ButtonChord::new(0x81, 4));
        let actions = [ButtonChord::new(0, 2), ButtonChord::new(0x40, 3)];
        let evidence = positive.verify_current_continuation(&actions).unwrap();
        assert_eq!(evidence.chord_state_hashes.len(), actions.len());

        let mut corrupted_control = control(&rom);
        // One action: branch snapshot, uninterrupted hash, restore validation,
        // then the planted first replayed-chord hash.
        corrupted_control.corrupt_state_hash_call = Some(4);
        let mut negative = DifferentialSmbTarget::from_machine(corrupted_control, &rom).unwrap();
        let error = negative
            .verify_current_continuation(&actions[..1])
            .expect_err("planted replay hash drift must fail");
        assert!(error.to_string().contains("differs at chord 0"));
    }

    #[test]
    fn remote_target_matches_local_endpoint_and_reconstructs_a_serialized_snapshot() {
        let rom = synthetic_nrom();
        let mut remote = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        let mut local = SmbTarget::from_smb_rom_bytes_headless(&rom).unwrap();
        let work_before = remote.frames_clocked();
        let actions = [ButtonChord::new(0x81, 4), ButtonChord::new(0, 2)];
        for action in actions {
            remote.apply(&action);
            local.apply(&action);
            assert_eq!(remote.wram(), local.wram());
            assert_eq!(remote.observe().frame_count, local.observe().frame_count);
        }

        let snapshot = remote.snapshot().unwrap();
        assert!(snapshot.has_live_handle());
        remote.apply(&ButtonChord::new(0x40, 3));
        remote.restore(&snapshot).unwrap();
        assert_eq!(remote.wram().as_slice(), snapshot.wram());
        assert!(remote.restore_counters().continuation > 0);

        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let durable: RemoteSmbSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert!(!durable.has_live_handle());
        remote.apply(&ButtonChord::new(0x02, 7));
        remote.restore(&durable).unwrap();
        assert_eq!(remote.wram().as_slice(), durable.wram());
        assert!(remote.restore_counters().genesis > 0);
        assert_eq!(remote.frames_clocked().saturating_sub(work_before), 22);
    }

    #[test]
    fn root_snapshots_restore_through_genesis_before_and_after_serialization() {
        let rom = synthetic_nrom();
        let mut remote = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        let root = remote.snapshot().unwrap();
        assert!(root.has_live_handle());

        remote.apply(&ButtonChord::new(0x81, 4));
        let before_live = remote.restore_counters();
        remote.restore(&root).unwrap();
        let after_live = remote.restore_counters();
        assert_eq!(after_live.genesis, before_live.genesis + 1);
        assert_eq!(after_live.continuation, before_live.continuation);

        let bytes = serde_json::to_vec(&root).unwrap();
        let durable: RemoteSmbSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert!(!durable.has_live_handle());
        remote.apply(&ButtonChord::new(0x40, 3));
        let before_durable = remote.restore_counters();
        remote.restore(&durable).unwrap();
        let after_durable = remote.restore_counters();
        assert_eq!(after_durable.genesis, before_durable.genesis + 1);
        assert_eq!(after_durable.continuation, before_durable.continuation);
    }

    #[test]
    fn restored_continuation_repeats_each_chord_state_hash() {
        let rom = synthetic_nrom();
        let mut remote = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        remote.apply(&ButtonChord::new(0x81, 4));
        let branch = remote.snapshot().unwrap();
        let suffix = [ButtonChord::new(0, 2), ButtonChord::new(0x40, 3)];
        let mut uninterrupted = Vec::new();
        for action in suffix {
            remote.apply(&action);
            uninterrupted.push(remote.state_hash().unwrap());
        }

        remote.restore(&branch).unwrap();
        assert_eq!(remote.state_hash().unwrap(), branch.state_hash());
        let mut restored = Vec::new();
        for action in suffix {
            remote.apply(&action);
            restored.push(remote.state_hash().unwrap());
        }
        assert_eq!(restored, uninterrupted);

        let mut altered = branch;
        altered.state_hash[0] ^= 0x80;
        let error = remote
            .restore(&altered)
            .expect_err("altered snapshot hash must fail");
        assert!(error.to_string().contains("whole-state hash"));
    }

    #[test]
    fn remote_target_rejects_a_wrong_guest_frame_delta() {
        let rom = synthetic_nrom();
        let mut control = control(&rom);
        control.reported_deadline_offset = 1;
        let error = RemoteSmbTarget::from_machine(control).unwrap_err();
        assert!(error.to_string().contains("boot walk stopped unexpectedly"));
    }

    #[test]
    fn remote_target_accepts_only_terminal_early_lifecycle_yields() {
        let rom = synthetic_nrom();
        let mut terminal = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        terminal.machine.early_yield_after = Some(2);
        terminal.machine.early_yield_is_terminal = true;
        terminal.apply(&ButtonChord::new(0x81, 4));
        assert_eq!(terminal.exit_kind(), ExitKind::Ok);
        assert!(terminal.is_dead());
        assert_eq!(terminal.observe().frame_count, 2);

        let mut nonterminal = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        nonterminal.machine.early_yield_after = Some(2);
        nonterminal.apply(&ButtonChord::new(0x81, 4));
        assert_eq!(nonterminal.exit_kind(), ExitKind::Crash);
    }

    #[test]
    fn remote_target_rejects_a_malformed_wram_publication() {
        let rom = synthetic_nrom();
        let mut control = control(&rom);
        control.published_wram_len = (WRAM_SIZE - 1) as u32;
        let error = RemoteSmbTarget::from_machine(control).unwrap_err();
        assert!(error.to_string().contains("expected 2048"));
    }

    #[test]
    fn remote_reset_fails_closed_when_wram_cannot_be_read() {
        let rom = synthetic_nrom();
        let mut remote = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        remote.apply(&ButtonChord::new(0x81, 4));
        let prior = remote.wram();
        remote.machine.fail_read_call = Some(remote.machine.read_calls.get().saturating_add(1));

        remote.reset();

        assert_eq!(remote.exit_kind(), ExitKind::Crash);
        assert_eq!(
            remote.wram(),
            prior,
            "a read error must not become zero WRAM"
        );
    }

    #[test]
    fn evicted_live_handles_fall_back_to_durable_lineage() {
        let rom = synthetic_nrom();
        let mut remote = RemoteSmbTarget::from_machine(control(&rom)).unwrap();
        let oldest = remote.snapshot().unwrap();
        for _ in 0..LIVE_SNAPSHOT_LIMIT {
            remote.snapshot().unwrap();
        }
        remote.apply(&ButtonChord::new(0x81, 4));
        remote.restore(&oldest).unwrap();
        assert_eq!(remote.wram().as_slice(), oldest.wram());
        assert!(remote.restore_counters().genesis > 0);
    }
}
