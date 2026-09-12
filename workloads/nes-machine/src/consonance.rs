// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whole-VM Consonance implementation of the deterministic machine boundary.
//!
//! This module is the NES observation/controller adapter over the generic
//! Consonance client session. The client owns the VM lifecycle and snapshot
//! protocol; this adapter supplies the action payload encoding, publication
//! discovery, billboard decoding, and cached NES observation windows. The guest
//! play-agent publishes a fixed billboard, which is copied once after a run and
//! then serves the small NES observation windows without touching the VM again.

use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    path::Path,
};

use consonance_client::session::{Session, SessionConfig, host_minor_faults};
use control_proto::{
    SnapId as ControlSnapId, StopConditions as ControlStopConditions, StopMask as ControlStopMask,
};
use sha2::{Digest, Sha256};

use crate::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason, nes,
};

#[cfg(target_arch = "x86_64")]
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "aarch64")]
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const BOOT_BUDGET: u64 = 2_000_000_000;
#[cfg(target_arch = "aarch64")]
const BOOT_BUDGET: u64 = 20_000_000_000;
const RUN_BUDGET: u64 = BOOT_BUDGET;
const RAM_GPA_BASE: u64 = consonance_client::session::RAM_GPA_BASE;
const SEED: u64 = 0x4e4f_5641_5f53_4541;
#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

pub use nes_protocol::{
    BILLBOARD_OBSERVATION_LEN, BILLBOARD_SAVE_RAM_LEN, BILLBOARD_SAVE_RAM_OFFSET,
    BILLBOARD_VERSION, BILLBOARD_WORK_RAM_LEN, BILLBOARD_WORK_RAM_OFFSET,
    HEADER_LEN as BILLBOARD_HEADER_LEN,
};
pub const BILLBOARD_MAGIC: &[u8; 4] = &nes_protocol::BILLBOARD_MAGIC;
const PAGE_SIZE: usize = consonance_client::session::PAGE_SIZE;

#[derive(Clone, Copy)]
enum ProfileVerb {
    Branch,
    Run,
    Snapshot,
    Read,
    SdkEvents,
}

impl ProfileVerb {
    const fn index(self) -> usize {
        match self {
            Self::Branch => 0,
            Self::Run => 1,
            Self::Snapshot => 2,
            Self::Read => 3,
            Self::SdkEvents => 4,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Branch => "Branch",
            Self::Run => "Run",
            Self::Snapshot => "Snapshot",
            Self::Read => "Read",
            Self::SdkEvents => "SdkEvents",
        }
    }
}

#[derive(Default)]
struct ConsonanceProfile {
    enabled: bool,
    ram_gpa_base: u64,
    wall_ns: [u128; 5],
    calls: [u64; 5],
    branch_wall_samples_ns: Vec<u128>,
    snapshot_wall_samples_ns: Vec<u128>,
    last_snapshot_wall_ns: u128,
    flatten_wall_samples_ns: Vec<u128>,
    restore_calls: u64,
    restore_bytes: u64,
    in_place_fallbacks: u64,
    setup_nonzero_pages: Option<u64>,
    billboard: Option<(u64, u64)>,
    seals: u64,
    dirty_available_seals: u64,
    dirty_pages: u64,
    dirty_billboard_pages: u64,
    dirty_other_pages: u64,
    action_dirty_pages: u64,
    action_dirty_billboard_pages: u64,
    action_dirty_other_pages: u64,
    actions: u64,
    frames: u64,
    doorbell_exits: u64,
    touched_pages: u64,
}

impl ConsonanceProfile {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ram_gpa_base: RAM_GPA_BASE,
            ..Self::default()
        }
    }

    fn record_verb_kind(&mut self, verb: ProfileVerb, wall_ns: u128) {
        if !self.enabled {
            return;
        }
        let index = verb.index();
        self.wall_ns[index] = self.wall_ns[index].saturating_add(wall_ns);
        self.calls[index] = self.calls[index].saturating_add(1);
        match verb {
            ProfileVerb::Branch => self.branch_wall_samples_ns.push(wall_ns),
            ProfileVerb::Snapshot => {
                self.snapshot_wall_samples_ns.push(wall_ns);
                self.last_snapshot_wall_ns = wall_ns;
            }
            ProfileVerb::Run | ProfileVerb::Read | ProfileVerb::SdkEvents => {}
        }
    }

    fn record_restore(&mut self, bytes: u64, fallbacks: u64) {
        if !self.enabled {
            return;
        }
        self.restore_calls = self.restore_calls.saturating_add(1);
        self.restore_bytes = self.restore_bytes.saturating_add(bytes);
        self.in_place_fallbacks = fallbacks;
    }

    fn set_setup(&mut self, setup_nonzero_pages: u64, billboard_gpa: u64, billboard_len: u64) {
        if !self.enabled {
            return;
        }
        self.setup_nonzero_pages = Some(setup_nonzero_pages);
        self.billboard = Some((billboard_gpa, billboard_len));
    }

    fn record_seal(&mut self, dirty_gfns: Option<&[u64]>, chain_len: Option<u32>) {
        if !self.enabled {
            return;
        }
        self.seals = self.seals.saturating_add(1);
        let Some(dirty_gfns) = dirty_gfns else {
            return;
        };
        self.dirty_available_seals = self.dirty_available_seals.saturating_add(1);
        if chain_len == Some(1) {
            self.flatten_wall_samples_ns
                .push(self.last_snapshot_wall_ns);
        }
        for &gfn in dirty_gfns {
            let gpa = self
                .ram_gpa_base
                .saturating_add(gfn.saturating_mul(PAGE_SIZE as u64));
            self.dirty_pages = self.dirty_pages.saturating_add(1);
            if self.overlaps_billboard(gpa) {
                self.dirty_billboard_pages = self.dirty_billboard_pages.saturating_add(1);
            } else {
                self.dirty_other_pages = self.dirty_other_pages.saturating_add(1);
            }
        }
    }

    fn overlaps_billboard(&self, gpa: u64) -> bool {
        self.billboard.is_some_and(|(start, len)| {
            let end = start.saturating_add(len);
            gpa < end && start < gpa.saturating_add(PAGE_SIZE as u64)
        })
    }

    fn dirty_totals(&self) -> [u64; 3] {
        [
            self.dirty_pages,
            self.dirty_billboard_pages,
            self.dirty_other_pages,
        ]
    }

    fn record_action(
        &mut self,
        frames: u64,
        doorbell_exits: u64,
        touched_pages: Option<u64>,
        dirty_before: [u64; 3],
    ) {
        if !self.enabled {
            return;
        }
        let dirty_after = self.dirty_totals();
        self.actions = self.actions.saturating_add(1);
        self.frames = self.frames.saturating_add(frames);
        self.doorbell_exits = self.doorbell_exits.saturating_add(doorbell_exits);
        self.touched_pages = self
            .touched_pages
            .saturating_add(touched_pages.unwrap_or(0));
        self.action_dirty_pages = self
            .action_dirty_pages
            .saturating_add(dirty_after[0].saturating_sub(dirty_before[0]));
        self.action_dirty_billboard_pages = self
            .action_dirty_billboard_pages
            .saturating_add(dirty_after[1].saturating_sub(dirty_before[1]));
        self.action_dirty_other_pages = self
            .action_dirty_other_pages
            .saturating_add(dirty_after[2].saturating_sub(dirty_before[2]));
    }

    fn render(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let mut line = String::from("consonance-profile");
        for verb in [
            ProfileVerb::Branch,
            ProfileVerb::Run,
            ProfileVerb::Snapshot,
            ProfileVerb::Read,
            ProfileVerb::SdkEvents,
        ] {
            let index = verb.index();
            let _ = write!(
                line,
                " {}_calls={} {}_wall_ns={}",
                verb.name().to_ascii_lowercase(),
                self.calls[index],
                verb.name().to_ascii_lowercase(),
                self.wall_ns[index]
            );
        }
        let mut branch_samples = self.branch_wall_samples_ns.clone();
        branch_samples.sort_unstable();
        let mut snapshot_samples = self.snapshot_wall_samples_ns.clone();
        snapshot_samples.sort_unstable();
        let mut flatten_samples = self.flatten_wall_samples_ns.clone();
        flatten_samples.sort_unstable();
        let flatten_wall_ns = flatten_samples.iter().copied().sum::<u128>();
        let _ = write!(
            line,
            " branch_median_ns={} branch_p99_ns={} snapshot_median_ns={} snapshot_p99_ns={} restore_calls={} restore_bytes={} in_place_fallbacks={} seals={} dirty_available_seals={} flatten_calls={} flatten_wall_ns={} flatten_median_ns={} flatten_p99_ns={} dirty_pages={} dirty_billboard_pages={} dirty_other_pages={} action_dirty_pages={} action_dirty_billboard_pages={} action_dirty_other_pages={} setup_nonzero_pages={} billboard={} actions={} frames={} doorbell_exits={} touched_pages={}",
            percentile(&branch_samples, 50),
            percentile(&branch_samples, 99),
            percentile(&snapshot_samples, 50),
            percentile(&snapshot_samples, 99),
            self.restore_calls,
            self.restore_bytes,
            self.in_place_fallbacks,
            self.seals,
            self.dirty_available_seals,
            flatten_samples.len(),
            flatten_wall_ns,
            percentile(&flatten_samples, 50),
            percentile(&flatten_samples, 99),
            self.dirty_pages,
            self.dirty_billboard_pages,
            self.dirty_other_pages,
            self.action_dirty_pages,
            self.action_dirty_billboard_pages,
            self.action_dirty_other_pages,
            self.setup_nonzero_pages.unwrap_or(0),
            self.billboard.map_or_else(
                || "none".to_owned(),
                |(gpa, len)| format!("{gpa:#x}+{len:#x}"),
            ),
            self.actions,
            self.frames,
            self.doorbell_exits,
            self.touched_pages,
        );
        Some(line)
    }

    #[cfg(test)]
    fn test_record_verb(&mut self, verb: ProfileVerb, wall_ns: u128) {
        self.record_verb_kind(verb, wall_ns);
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len() - 1).saturating_mul(percentile) / 100;
    sorted[index]
}

fn control_snap(snapshot: SnapId) -> ControlSnapId {
    ControlSnapId(snapshot.0)
}

fn machine_snap(snapshot: ControlSnapId) -> SnapId {
    SnapId(snapshot.0)
}

/// Sparse portable snapshots are owned by the neutral Consonance client.
pub use consonance_client::session::SparseSnapshot as ConsonancePortable;

use nes_protocol::BillboardObservation;

fn parse_billboard(
    bytes: &[u8],
    require_frames: bool,
) -> Result<BillboardObservation, MachineError> {
    nes_protocol::parse_billboard(bytes, require_frames)
        .map_err(|error| MachineError::Backend(error.to_string()))
}

/// One Consonance VM implementing [`Machine`].
pub struct ConsonanceMachine {
    session: Session,
    power_on_publication: bool,
    setup: SnapId,
    billboard_gpa: u64,
    billboard_len: u32,
    endpoint_work_ram: [u8; nes::WRAM_SIZE],
    save_ram: [u8; BILLBOARD_SAVE_RAM_LEN],
    ring: Vec<[u8; nes::WRAM_SIZE]>,
    lifetime_frames: u64,
    last_vtime: u64,
    snapshot_vtimes: BTreeMap<SnapId, u64>,
    observation_valid: bool,
    profile: ConsonanceProfile,
}

impl fmt::Debug for ConsonanceMachine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsonanceMachine")
            .field("setup", &self.setup)
            .field("billboard_gpa", &self.billboard_gpa)
            .field("billboard_len", &self.billboard_len)
            .field("lifetime_frames", &self.lifetime_frames)
            .field("ring_frames", &self.ring.len())
            .finish_non_exhaustive()
    }
}

impl Drop for ConsonanceMachine {
    fn drop(&mut self) {
        if let Some(line) = self.profile.render() {
            eprintln!("{line}");
        }
    }
}

impl ConsonanceMachine {
    /// Boot one VM and retain its setup snapshot.
    pub fn new(kernel: &[u8], initramfs: &[u8]) -> Result<Self, MachineError> {
        let config = SessionConfig::new(RAM, SEED, RUN_BUDGET, CMDLINE)
            .with_identity_tag("consonance-nes-execution-v2");
        let setup_payloads = vec![vec![0, 1]; 16];
        let mut session =
            Session::new_with_config_and_payloads(kernel, initramfs, config, setup_payloads)
                .map_err(|error| MachineError::Backend(error.to_string()))?;
        let mut profile =
            ConsonanceProfile::new(std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some());
        let (setup_handle, setup_vtime) = session.setup_handle();
        let setup = machine_snap(setup_handle);
        drive_profiled(
            &mut session,
            &mut profile,
            ProfileVerb::Branch,
            "setup replay",
            |s| s.replay_snapshot(control_snap(setup)),
        )?;
        let registers = state_registers(&mut session, &mut profile)?;
        let power_on_publication = registers.contains("nes.publication.gpa");
        let (gpa_name, len_name) = if power_on_publication {
            ("nes.publication.gpa", "nes.publication.len")
        } else {
            ("nova_billboard_gpa", "nova_billboard_len")
        };
        let billboard_gpa = registers.get(gpa_name).map_err(MachineError::Backend)?;
        let billboard_len = u32::try_from(registers.get(len_name).map_err(MachineError::Backend)?)
            .map_err(|_| MachineError::Backend("billboard length exceeds u32".to_owned()))?;
        if usize::try_from(billboard_len)
            .ok()
            .is_none_or(|len| len < BILLBOARD_OBSERVATION_LEN)
        {
            return Err(MachineError::Backend(
                "billboard is shorter than its fixed observation region".to_owned(),
            ));
        }
        let observed = read_billboard(
            &mut session,
            billboard_gpa,
            billboard_len,
            &mut profile,
            false,
        )?;
        if !observed.work_frames.is_empty() {
            return Err(MachineError::Backend(
                "setup billboard unexpectedly contains action frames".to_owned(),
            ));
        }
        let setup_nonzero_pages = session
            .snapshot_owned_pages(control_snap(setup))
            .ok_or_else(|| {
                MachineError::Backend("setup snapshot statistics unavailable".to_owned())
            })?;
        profile.set_setup(setup_nonzero_pages, billboard_gpa, u64::from(billboard_len));
        Ok(Self {
            session,
            power_on_publication,
            setup,
            billboard_gpa,
            billboard_len,
            endpoint_work_ram: observed.endpoint_work_ram,
            save_ram: observed.save_ram,
            ring: Vec::new(),
            lifetime_frames: 0,
            last_vtime: setup_vtime,
            snapshot_vtimes: BTreeMap::from([(setup, setup_vtime)]),
            observation_valid: true,
            profile,
        })
    }

    /// Whether the guest exposes a game-neutral power-on core.
    pub fn starts_at_power_on(&self) -> bool {
        self.power_on_publication
    }
}

impl Machine for ConsonanceMachine {
    type Portable = ConsonancePortable;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let (control_snapshot, vtime) = drive_profiled(
            &mut self.session,
            &mut self.profile,
            ProfileVerb::Snapshot,
            "snapshot",
            Session::snapshot,
        )?;
        let snapshot = machine_snap(control_snapshot);
        self.profile.record_seal(
            self.session.last_seal_dirty_gfns().as_deref(),
            self.session.snapshot_chain_len(control_snap(snapshot)),
        );
        self.snapshot_vtimes.insert(snapshot, vtime);
        Ok(snapshot)
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        drive_profiled(
            &mut self.session,
            &mut self.profile,
            ProfileVerb::Branch,
            "drop snapshot",
            |session| session.drop_snapshot(control_snap(snap)),
        )?;
        self.snapshot_vtimes.remove(&snap);
        Ok(())
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let restored_vtime = *self.snapshot_vtimes.get(&snap).ok_or_else(|| {
            MachineError::Backend("branched snapshot has no recorded V-time".to_owned())
        })?;
        let actions = nes::actions_of(env)?;
        let mut payloads = Vec::new();
        payloads
            .try_reserve(actions.len().saturating_add(1))
            .map_err(|error| {
                MachineError::Backend(format!("payload tape allocation failed: {error}"))
            })?;
        payloads.extend(
            actions
                .iter()
                .map(|action| vec![action.buttons, action.bounded_hold_frames()]),
        );
        payloads.push(vec![0, 1]);
        drive_profiled(
            &mut self.session,
            &mut self.profile,
            ProfileVerb::Branch,
            "branch",
            |session| session.branch_payloads(control_snap(snap), payloads),
        )?;
        let (bytes, fallbacks) = self.session.last_restore_stats();
        self.profile.record_restore(bytes, fallbacks);
        self.last_vtime = restored_vtime;
        self.observation_valid = false;
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let restored_vtime = *self.snapshot_vtimes.get(&snap).ok_or_else(|| {
            MachineError::Backend("replayed snapshot has no recorded V-time".to_owned())
        })?;
        drive_profiled(
            &mut self.session,
            &mut self.profile,
            ProfileVerb::Branch,
            "replay",
            |session| session.replay_snapshot(control_snap(snap)),
        )?;
        let (bytes, fallbacks) = self.session.last_restore_stats();
        self.profile.record_restore(bytes, fallbacks);
        self.last_vtime = restored_vtime;
        self.observation_valid = false;
        Ok(())
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        if resolve.is_some() {
            return Err(MachineError::ResolveWithoutDecision);
        }
        validate_supported_stop_conditions(until)?;
        self.ring.clear();
        self.observation_valid = false;
        let deadline = run_deadline(self.last_vtime, RUN_BUDGET)?;
        let before_faults = self.profile.enabled.then(host_minor_faults).flatten();
        let before_dirty = self.profile.dirty_totals();
        let before_doorbells = self.session.doorbell_exits();
        let stop = drive_profiled(
            &mut self.session,
            &mut self.profile,
            ProfileVerb::Run,
            "run",
            |session| {
                session.run(
                    ControlStopConditions {
                        deadline: Some(control_proto::Moment(deadline)),
                        on: ControlStopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
                    },
                    None,
                )
            },
        )?;
        let mapped = map_stop(stop);
        self.last_vtime = stop_vtime(&mapped).0;
        if !matches!(mapped, StopReason::SnapshotPoint { .. }) {
            return Ok(mapped);
        }
        let observed = read_billboard(
            &mut self.session,
            self.billboard_gpa,
            self.billboard_len,
            &mut self.profile,
            true,
        )?;
        let frames = u64::try_from(observed.work_frames.len())
            .map_err(|_| MachineError::Backend("billboard frame count exceeds u64".to_owned()))?;
        let after_faults = before_faults.and_then(|_| host_minor_faults());
        let touched_pages =
            before_faults.and_then(|before| after_faults.map(|after| after.saturating_sub(before)));
        self.profile.record_action(
            frames,
            self.session
                .doorbell_exits()
                .saturating_sub(before_doorbells),
            touched_pages,
            before_dirty,
        );
        self.lifetime_frames = self.lifetime_frames.saturating_add(frames);
        self.ring = observed.work_frames;
        self.endpoint_work_ram = observed.endpoint_work_ram;
        self.save_ram = observed.save_ram;
        self.observation_valid = true;
        Ok(mapped)
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        read_observation(
            self.observation_valid,
            &self.endpoint_work_ram,
            &self.save_ram,
            addr,
            len,
        )
    }

    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError> {
        self.session
            .export_sparse_snapshot(control_snap(snap), base)
            .map_err(|error| MachineError::Backend(error.to_string()))
    }

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
        let control_snapshot = self
            .session
            .import_sparse_snapshot(portable)
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        let vtime = self
            .session
            .snapshot_time(control_snapshot)
            .ok_or_else(|| MachineError::Backend("imported snapshot has no V-time".to_owned()))?;
        let snapshot = machine_snap(control_snapshot);
        self.snapshot_vtimes.insert(snapshot, vtime);
        Ok(snapshot)
    }

    fn portable_memory_charge(portable: &Self::Portable) -> usize {
        portable.memory_charge()
    }

    fn now(&self) -> Moment {
        Moment(self.lifetime_frames)
    }

    fn frames(&self) -> &[[u8; nes::WRAM_SIZE]] {
        &self.ring
    }
}

fn drive_profiled<T>(
    session: &mut Session,
    profile: &mut ConsonanceProfile,
    verb: ProfileVerb,
    operation: &'static str,
    operation_fn: impl FnOnce(&mut Session) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, MachineError> {
    #[allow(clippy::disallowed_methods)]
    let started = profile.enabled.then(std::time::Instant::now);
    let result = operation_fn(session);
    #[allow(clippy::disallowed_methods)]
    if let Some(started) = started {
        profile.record_verb_kind(verb, started.elapsed().as_nanos());
    }
    result.map_err(|error| MachineError::Backend(format!("{operation}: {error}")))
}

fn state_registers(
    session: &mut Session,
    profile: &mut ConsonanceProfile,
) -> Result<consonance_client::catalog::StateCatalog, MachineError> {
    let mut catalog = consonance_client::catalog::StateCatalog::default();
    let events = drive_profiled(
        session,
        profile,
        ProfileVerb::SdkEvents,
        "SDK event fetch",
        Session::sdk_events,
    )?;
    for (_, event_id, bytes) in events {
        catalog
            .observe(event_id, &bytes)
            .map_err(MachineError::Backend)?;
    }
    Ok(catalog)
}

fn read_billboard(
    session: &mut Session,
    billboard_gpa: u64,
    billboard_len: u32,
    profile: &mut ConsonanceProfile,
    require_frames: bool,
) -> Result<BillboardObservation, MachineError> {
    if usize::try_from(billboard_len)
        .ok()
        .is_none_or(|len| len < BILLBOARD_OBSERVATION_LEN)
    {
        return Err(MachineError::Backend(
            "billboard is shorter than its fixed observation region".to_owned(),
        ));
    }
    let bytes = drive_profiled(
        session,
        profile,
        ProfileVerb::Read,
        "billboard read",
        |session| {
            let len = u32::try_from(BILLBOARD_OBSERVATION_LEN)
                .map_err(|_| "billboard observation length exceeds u32")?;
            session.read(billboard_gpa, len)
        },
    )?;
    if bytes.len() != BILLBOARD_OBSERVATION_LEN {
        return Err(MachineError::Backend(
            "billboard read was truncated".to_owned(),
        ));
    }
    parse_billboard(&bytes, require_frames)
}

fn read_cached(
    work_ram: &[u8; nes::WRAM_SIZE],
    save_ram: &[u8; BILLBOARD_SAVE_RAM_LEN],
    addr: u64,
    len: u32,
) -> Result<Vec<u8>, MachineError> {
    let end = addr
        .checked_add(u64::from(len))
        .ok_or(MachineError::ReadOutOfBounds)?;
    let length = usize::try_from(len).map_err(|_| MachineError::ReadOutOfBounds)?;
    if end <= nes::WRAM_SIZE as u64 {
        let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
        let finish = start
            .checked_add(length)
            .filter(|finish| *finish <= work_ram.len())
            .ok_or(MachineError::ReadOutOfBounds)?;
        return Ok(work_ram[start..finish].to_vec());
    }
    if addr >= 0x6000 && end <= 0x6000 + BILLBOARD_SAVE_RAM_LEN as u64 {
        let start = usize::try_from(addr - 0x6000).map_err(|_| MachineError::ReadOutOfBounds)?;
        let finish = start
            .checked_add(length)
            .filter(|finish| *finish <= save_ram.len())
            .ok_or(MachineError::ReadOutOfBounds)?;
        return Ok(save_ram[start..finish].to_vec());
    }
    Err(MachineError::ReadOutOfBounds)
}

fn read_observation(
    valid: bool,
    work_ram: &[u8; nes::WRAM_SIZE],
    save_ram: &[u8; BILLBOARD_SAVE_RAM_LEN],
    addr: u64,
    len: u32,
) -> Result<Vec<u8>, MachineError> {
    if !valid {
        return Err(MachineError::Backend(
            "no cached observation at the current stop".to_owned(),
        ));
    }
    read_cached(work_ram, save_ram, addr, len)
}

fn map_stop(stop: control_proto::StopReason) -> StopReason {
    match stop {
        control_proto::StopReason::Deadline { vtime } => StopReason::Deadline {
            vtime: Moment(vtime.0),
        },
        control_proto::StopReason::Quiescent { vtime } => StopReason::Quiescent {
            vtime: Moment(vtime.0),
        },
        control_proto::StopReason::Crash { vtime, info } => StopReason::Crash {
            vtime: Moment(vtime.0),
            info: crate::CrashInfo {
                kind: match info.kind {
                    control_proto::CrashKind::Panic => crate::CrashKind::Panic,
                    control_proto::CrashKind::UnrecoverableFault => {
                        crate::CrashKind::UnrecoverableFault
                    }
                    control_proto::CrashKind::Shutdown => crate::CrashKind::Shutdown,
                },
                detail: info.detail,
            },
        },
        control_proto::StopReason::Decision { vtime, id, ctx } => StopReason::Decision {
            vtime: Moment(vtime.0),
            id: crate::DecisionId(id.0),
            ctx,
        },
        control_proto::StopReason::SnapshotPoint { vtime } => StopReason::SnapshotPoint {
            vtime: Moment(vtime.0),
        },
        control_proto::StopReason::Assertion { vtime, ev } => StopReason::Assertion {
            vtime: Moment(vtime.0),
            ev: crate::EventRef {
                id: ev.id,
                data: ev.data,
            },
        },
    }
}

fn stop_vtime(stop: &StopReason) -> Moment {
    match stop {
        StopReason::Deadline { vtime }
        | StopReason::Quiescent { vtime }
        | StopReason::Crash { vtime, .. }
        | StopReason::Decision { vtime, .. }
        | StopReason::SnapshotPoint { vtime }
        | StopReason::Assertion { vtime, .. } => *vtime,
    }
}

fn validate_supported_stop_conditions(until: StopConditions) -> Result<(), MachineError> {
    if until != StopConditions::default() {
        return Err(MachineError::Backend(
            "ConsonanceMachine supports only default stop conditions".to_owned(),
        ));
    }
    Ok(())
}

fn run_deadline(last_vtime: u64, budget: u64) -> Result<u64, MachineError> {
    last_vtime
        .checked_add(budget)
        .ok_or_else(|| MachineError::Backend("Consonance per-run deadline overflow".to_owned()))
}

/// Stable identity string for a Consonance whole-VM campaign stream.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    format!(
        "consonance-whole-vm-v2;kernel-sha256={:x};initramfs-sha256={:x};sdk-input=payload-v1;snapshot=sparse-pages-plus-sidecar-v3;environment=inputs-v5;service-state=channel-v1",
        Sha256::digest(kernel),
        Sha256::digest(initramfs),
    )
}

/// Construct a Consonance machine from guest image paths.
pub fn from_paths(kernel: &Path, initramfs: &Path) -> Result<ConsonanceMachine, MachineError> {
    let kernel = std::fs::read(kernel)
        .map_err(|error| MachineError::Backend(format!("read kernel: {error}")))?;
    let initramfs = std::fs::read(initramfs)
        .map_err(|error| MachineError::Backend(format!("read initramfs: {error}")))?;
    ConsonanceMachine::new(&kernel, &initramfs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn billboard(frames_run: u8) -> Vec<u8> {
        let mut bytes = vec![0_u8; BILLBOARD_OBSERVATION_LEN];
        bytes[0..4].copy_from_slice(BILLBOARD_MAGIC);
        bytes[4..6].copy_from_slice(&BILLBOARD_VERSION.to_le_bytes());
        bytes[8..12].copy_from_slice(&u32::from(frames_run).to_le_bytes());
        bytes[13] = frames_run;
        bytes[16..20].copy_from_slice(&(BILLBOARD_WORK_RAM_OFFSET as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&(BILLBOARD_WORK_RAM_LEN as u32).to_le_bytes());
        bytes[24..28].copy_from_slice(&(BILLBOARD_SAVE_RAM_OFFSET as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&(BILLBOARD_SAVE_RAM_LEN as u32).to_le_bytes());
        for index in 0..usize::from(frames_run) {
            bytes[BILLBOARD_WORK_RAM_OFFSET + index * nes::WRAM_SIZE] = index as u8;
        }
        parse_billboard(&bytes, frames_run != 0).expect("valid billboard");
        bytes
    }

    #[test]
    fn billboard_validates_fixed_layout_and_truncations() {
        let valid = billboard(2);
        let observed = parse_billboard(&valid, true).expect("parse");
        assert_eq!(observed.work_frames.len(), 2);
        assert_eq!(observed.endpoint_work_ram[0], 1);
        for end in [0, 4, BILLBOARD_OBSERVATION_LEN - 1] {
            assert!(parse_billboard(&valid[..end], false).is_err());
        }
        let mut bad = valid.clone();
        bad[20..24].copy_from_slice(&0_u32.to_le_bytes());
        assert!(parse_billboard(&bad, false).is_err());
        let mut bad_version = valid.clone();
        bad_version[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_billboard(&bad_version, false).is_err());
        let mut bad_flags = valid;
        bad_flags[6..8].copy_from_slice(&0b100_u16.to_le_bytes());
        assert!(parse_billboard(&bad_flags, false).is_err());
    }

    #[test]
    fn read_windows_reject_crossing_and_overflow() {
        let work_ram = [0_u8; nes::WRAM_SIZE];
        let save_ram = [0_u8; BILLBOARD_SAVE_RAM_LEN];
        assert_eq!(
            read_cached(&work_ram, &save_ram, 0, 2).expect("wram").len(),
            2
        );
        assert_eq!(
            read_cached(&work_ram, &save_ram, 0x6000, 2)
                .expect("save")
                .len(),
            2
        );
        assert!(read_cached(&work_ram, &save_ram, u64::MAX - 1, 4).is_err());
        assert!(read_cached(&work_ram, &save_ram, 0x07ff, 2).is_err());
        assert!(read_cached(&work_ram, &save_ram, 0x7fff, 2).is_err());
    }

    #[test]
    fn run_budget_is_relative_to_the_restored_lineage_vtime() {
        let deep_vtime = BOOT_BUDGET.checked_mul(37).expect("fixture fits");
        let deadline = run_deadline(deep_vtime, RUN_BUDGET).expect("deep deadline");
        assert_eq!(deadline, deep_vtime + RUN_BUDGET);
        assert!(deadline > BOOT_BUDGET);
        assert!(run_deadline(u64::MAX, 1).is_err());
        assert!(run_deadline(u64::MAX - 1, 2).is_err());
    }

    #[test]
    fn only_default_stop_conditions_are_supported() {
        assert!(validate_supported_stop_conditions(StopConditions::default()).is_ok());
        assert!(
            validate_supported_stop_conditions(StopConditions {
                deadline: Some(Moment(1)),
                ..StopConditions::default()
            })
            .is_err()
        );
        assert!(
            validate_supported_stop_conditions(StopConditions {
                on: crate::StopMask::NONE.arm(crate::class_bit::ASSERTION),
                ..StopConditions::default()
            })
            .is_err()
        );
    }

    #[test]
    fn invalidated_observation_cache_cannot_serve_stale_ram() {
        let work_ram = [0xA5_u8; nes::WRAM_SIZE];
        let save_ram = [0x5A_u8; BILLBOARD_SAVE_RAM_LEN];
        assert!(matches!(
            read_observation(false, &work_ram, &save_ram, 0, 1),
            Err(MachineError::Backend(message))
                if message == "no cached observation at the current stop"
        ));
        assert_eq!(
            read_observation(true, &work_ram, &save_ram, 0, 1).unwrap(),
            vec![0xA5]
        );
    }

    #[test]
    fn profile_disabled_does_not_record() {
        let mut profile = ConsonanceProfile::new(false);
        profile.test_record_verb(ProfileVerb::Branch, 3);
        profile.record_action(1, 2, None, [0; 3]);
        assert!(profile.render().is_none());
        assert_eq!(profile.calls, [0; 5]);
    }
}
