// SPDX-License-Identifier: AGPL-3.0-or-later

//! Consonance whole-VM backend for the Nova campaign adapter.
//!
//! Dissonance retains portable, game-owned input prefixes. Each evaluator
//! thread lazily owns one Consonance VM session and maps those prefixes to real
//! whole-VM snapshots. A mutation therefore restores a Consonance snapshot,
//! supplies one opaque controller chord through the guest SDK, and decodes only
//! the guest-published progress markers. The generic search coordinator never
//! learns Nova rules or memory addresses.

use std::{
    cell::RefCell, collections::BTreeMap, error::Error, fmt::Write as _, mem::size_of, path::Path,
    sync::Arc,
};

use control_proto::{
    Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{EnvSpec, FaultPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_arch = "aarch64")]
use vmm_backend::Arm64 as HostArch;
use vmm_backend::Backend;
#[cfg(target_arch = "x86_64")]
use vmm_backend::X86 as HostArch;
#[cfg(target_arch = "aarch64")]
use vmm_core::vendor::arm64::{board, bringup::boot_selected_control};
#[cfg(target_arch = "x86_64")]
use vmm_core::vendor::x86::bringup::{
    boot_linux_stock_virtual_time, compose_stock_virtual_time_restore_target,
};
use vmm_core::{
    control::{ControlServer, RestoreMode, VmmFactory, host_minor_faults, server_caps},
    snapshot::DEFAULT_MAX_CHAIN_LEN,
};

use crate::{
    nova::target::{
        ButtonChord, MAX_HOLD_FRAMES, NovaMechanicalState, NovaObservations, WRAM_SIZE,
        decode_state, preference_tuple, spatial_bucket,
    },
    target::ExitKind,
};

type Server = ControlServer<Box<dyn Backend<A = HostArch>>>;

#[cfg(target_arch = "x86_64")]
/// Guest RAM for the x86 Nova workload. The setup image and its 2 MiB
/// billboard fit well below this bound; keeping the capacity here (rather
/// than in the action/observation path) leaves search streams unchanged.
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "aarch64")]
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const RAM_GPA_BASE: u64 = 0;
#[cfg(target_arch = "aarch64")]
const RAM_GPA_BASE: u64 = board::RAM_BASE;
#[cfg(target_arch = "x86_64")]
const DEADLINE: u64 = 2_000_000_000;
// The arm64 game kernel reaches `/init` at roughly 2 billion modeled
// nanoseconds on msr1. Leave a bounded 10x envelope for QuickNES setup; this
// remains deterministic because the limit is V-time, never wall-clock time.
#[cfg(target_arch = "aarch64")]
const DEADLINE: u64 = 20_000_000_000;
const SEED: u64 = 0x4e4f_5641_5f53_4541;
#[cfg(target_arch = "x86_64")]
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

const SDK_NS_SHIFT: u32 = 24;
const SDK_NS_STATE: u8 = 2;
const SDK_STATE_SET: u8 = 0;
const SDK_STATE_MAX: u8 = 1;
const REG_BILLBOARD_GPA: u32 = 11;
const REG_BILLBOARD_LEN: u32 = 12;

const BILLBOARD_HEADER_LEN: usize = 32;
const BILLBOARD_MAGIC: &[u8; 4] = b"HBBD";
const BILLBOARD_VERSION: u16 = 2;
const BILLBOARD_WORK_RAM_OFFSET: usize = BILLBOARD_HEADER_LEN;
const BILLBOARD_WORK_RAM_LEN: usize = MAX_HOLD_FRAMES as usize * WRAM_SIZE;
const BILLBOARD_SAVE_RAM_OFFSET: usize = BILLBOARD_WORK_RAM_OFFSET + BILLBOARD_WORK_RAM_LEN;
const BILLBOARD_SAVE_RAM_LEN: usize = 8 * 1024;
const BILLBOARD_OBSERVATION_LEN: usize = BILLBOARD_SAVE_RAM_OFFSET + BILLBOARD_SAVE_RAM_LEN;
const PAGE_SIZE: u64 = 4096;

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
    agent_ranges: Vec<(u64, u64)>,
    seals: u64,
    dirty_available_seals: u64,
    dirty_pages: u64,
    dirty_billboard_pages: u64,
    dirty_agent_pages: u64,
    dirty_other_pages: u64,
    action_dirty_pages: u64,
    action_dirty_billboard_pages: u64,
    action_dirty_agent_pages: u64,
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
            // The guest-agent mappings are intentionally not read from this
            // host process. They must come from the guest setup report and be
            // translated to guest GPA before classification; the current
            // control protocol has no such out-of-band report yet.
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
        if matches!(verb, ProfileVerb::Branch) {
            self.branch_wall_samples_ns.push(wall_ns);
        } else if matches!(verb, ProfileVerb::Snapshot) {
            self.snapshot_wall_samples_ns.push(wall_ns);
            self.last_snapshot_wall_ns = wall_ns;
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
        // A one-layer seal with a complete dirty drain is the bounded-chain
        // flatten path. The initial full base has no drained parent window.
        if chain_len == Some(1) {
            self.flatten_wall_samples_ns
                .push(self.last_snapshot_wall_ns);
        }
        for &gfn in dirty_gfns {
            let gpa = self
                .ram_gpa_base
                .saturating_add(gfn.saturating_mul(PAGE_SIZE));
            self.dirty_pages = self.dirty_pages.saturating_add(1);
            if self.overlaps_billboard(gpa) {
                self.dirty_billboard_pages = self.dirty_billboard_pages.saturating_add(1);
            } else if self
                .agent_ranges
                .iter()
                .any(|&(start, end)| gpa < end && start < gpa.saturating_add(PAGE_SIZE))
            {
                self.dirty_agent_pages = self.dirty_agent_pages.saturating_add(1);
            } else {
                self.dirty_other_pages = self.dirty_other_pages.saturating_add(1);
            }
        }
    }

    fn overlaps_billboard(&self, gpa: u64) -> bool {
        self.billboard.is_some_and(|(start, len)| {
            let end = start.saturating_add(len);
            gpa < end && start < gpa.saturating_add(PAGE_SIZE)
        })
    }

    fn dirty_totals(&self) -> [u64; 4] {
        [
            self.dirty_pages,
            self.dirty_billboard_pages,
            self.dirty_agent_pages,
            self.dirty_other_pages,
        ]
    }

    fn record_action(
        &mut self,
        frames: u64,
        doorbell_exits: u64,
        touched_pages: Option<u64>,
        dirty_before: [u64; 4],
    ) {
        if !self.enabled {
            return;
        }
        let dirty_after = self.dirty_totals();
        self.action_dirty_pages = self
            .action_dirty_pages
            .saturating_add(dirty_after[0].saturating_sub(dirty_before[0]));
        self.action_dirty_billboard_pages = self
            .action_dirty_billboard_pages
            .saturating_add(dirty_after[1].saturating_sub(dirty_before[1]));
        self.action_dirty_agent_pages = self
            .action_dirty_agent_pages
            .saturating_add(dirty_after[2].saturating_sub(dirty_before[2]));
        self.action_dirty_other_pages = self
            .action_dirty_other_pages
            .saturating_add(dirty_after[3].saturating_sub(dirty_before[3]));
        self.actions = self.actions.saturating_add(1);
        self.frames = self.frames.saturating_add(frames);
        self.doorbell_exits = self.doorbell_exits.saturating_add(doorbell_exits);
        self.touched_pages = self
            .touched_pages
            .saturating_add(touched_pages.unwrap_or(0));
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
        let ranges = self
            .agent_ranges
            .iter()
            .map(|&(start, end)| format!("{start:#x}-{end:#x}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut branch_samples = self.branch_wall_samples_ns.clone();
        branch_samples.sort_unstable();
        let branch_median_ns = percentile(&branch_samples, 50);
        let branch_p99_ns = percentile(&branch_samples, 99);
        let mut snapshot_samples = self.snapshot_wall_samples_ns.clone();
        snapshot_samples.sort_unstable();
        let snapshot_median_ns = percentile(&snapshot_samples, 50);
        let snapshot_p99_ns = percentile(&snapshot_samples, 99);
        let mut flatten_samples = self.flatten_wall_samples_ns.clone();
        flatten_samples.sort_unstable();
        let flatten_wall_ns = flatten_samples.iter().copied().sum::<u128>();
        let flatten_median_ns = percentile(&flatten_samples, 50);
        let flatten_p99_ns = percentile(&flatten_samples, 99);
        let _ = write!(
            line,
            " branch_median_ns={} branch_p99_ns={} snapshot_median_ns={} snapshot_p99_ns={} restore_calls={} restore_bytes={} in_place_fallbacks={} seals={} dirty_available_seals={} flatten_calls={} flatten_wall_ns={} flatten_median_ns={} flatten_p99_ns={} dirty_pages={} dirty_billboard_pages={} dirty_agent_pages={} dirty_other_pages={} action_dirty_pages={} action_dirty_billboard_pages={} action_dirty_agent_pages={} action_dirty_other_pages={} setup_nonzero_pages={} billboard={} agent_ranges={} actions={} frames={} doorbell_exits={} touched_pages={}",
            branch_median_ns,
            branch_p99_ns,
            snapshot_median_ns,
            snapshot_p99_ns,
            self.restore_calls,
            self.restore_bytes,
            self.in_place_fallbacks,
            self.seals,
            self.dirty_available_seals,
            flatten_samples.len(),
            flatten_wall_ns,
            flatten_median_ns,
            flatten_p99_ns,
            self.dirty_pages,
            self.dirty_billboard_pages,
            self.dirty_agent_pages,
            self.dirty_other_pages,
            self.action_dirty_pages,
            self.action_dirty_billboard_pages,
            self.action_dirty_agent_pages,
            self.action_dirty_other_pages,
            self.setup_nonzero_pages.unwrap_or(0),
            self.billboard.map_or_else(
                || "none".to_owned(),
                |(gpa, len)| { format!("{gpa:#x}+{len:#x}") }
            ),
            ranges,
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

/// Portable campaign snapshot for a Consonance-backed Nova endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsonanceNovaSnapshot {
    actions: Vec<ButtonChord>,
    observation: NovaObservations,
    work_ram: Vec<u8>,
    failed: bool,
}

impl ConsonanceNovaSnapshot {
    pub(crate) fn resident_memory_charge(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.actions.len().saturating_mul(size_of::<ButtonChord>()))
            .saturating_add(
                self.observation
                    .changed_indices
                    .len()
                    .saturating_mul(size_of::<u16>()),
            )
            .saturating_add(self.observation.log_line.len())
            .saturating_add(self.work_ram.len())
    }
}

#[derive(Debug)]
struct Config {
    key: [u8; 32],
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
}

struct Session {
    key: [u8; 32],
    server: Server,
    setup: SnapId,
    snapshots: BTreeMap<Vec<ButtonChord>, SnapId>,
    billboard_gpa: u64,
    billboard_len: u32,
    profile: ConsonanceProfile,
}

struct BillboardObservation {
    frame_count: u64,
    work_frames: Vec<[u8; WRAM_SIZE]>,
    endpoint_work_ram: [u8; WRAM_SIZE],
    save_ram: [u8; BILLBOARD_SAVE_RAM_LEN],
    dead: bool,
    cleared: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(line) = self.profile.render() {
            eprintln!("{line}");
        }
    }
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// One game-aware target handle; the live KVM session stays thread-local.
#[derive(Debug)]
pub struct ConsonanceNovaTarget {
    config: Arc<Config>,
    actions: Vec<ButtonChord>,
    observation: NovaObservations,
    action_observations: Vec<NovaObservations>,
    work_ram: Vec<u8>,
    failed: bool,
    frames_clocked: u64,
    genesis_cleared: u8,
}

impl ConsonanceNovaTarget {
    /// Boot or join the current evaluator thread's Consonance Nova session.
    pub fn new(kernel: &[u8], initramfs: &[u8]) -> Result<Self, String> {
        let mut digest = Sha256::new();
        digest.update(kernel);
        digest.update(initramfs);
        let key: [u8; 32] = digest.finalize().into();
        let config = Arc::new(Config {
            key,
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
        });
        let observed = with_session(&config, |session| session.observe())?;
        if !observed.work_frames.is_empty() {
            return Err("Nova setup billboard unexpectedly contains action frames".to_owned());
        }
        let state = decode_state(&observed.endpoint_work_ram, &observed.save_ram)
            .map_err(|error| error.to_string())?;
        if observed.dead != (state.health == 0) {
            return Err("Nova setup billboard dead flag disagrees with memory".to_owned());
        }
        let work_ram = observed.endpoint_work_ram.to_vec();
        let observation = make_observation(observed.frame_count, state, &work_ram, &[]);
        Ok(Self {
            config,
            actions: Vec::new(),
            action_observations: vec![observation.clone()],
            observation,
            work_ram,
            failed: false,
            frames_clocked: 0,
            genesis_cleared: state.cleared_count(),
        })
    }

    /// Current decoded, source-derived game state.
    #[must_use]
    pub fn mechanical_state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }

    /// Whether health reached zero.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.decoded.health == 0
    }

    /// Whether this branch durably cleared a level beyond genesis.
    #[must_use]
    pub fn cleared_a_level(&self) -> bool {
        self.observation.decoded.cleared_count() > self.genesis_cleared
    }

    /// Total emulated frames evaluated by this target handle.
    #[must_use]
    pub fn frames_clocked(&self) -> u64 {
        self.frames_clocked
    }

    /// Endpoint observations emitted by the most recent opaque chord.
    #[must_use]
    pub fn last_action_observations(&self) -> &[NovaObservations] {
        &self.action_observations
    }

    /// Restore one portable prefix through this thread's Consonance snapshot cache.
    pub fn restore(&mut self, snapshot: &ConsonanceNovaSnapshot) -> Result<(), Box<dyn Error>> {
        with_session(&self.config, |session| {
            let snap = session.ensure_prefix(&snapshot.actions)?;
            expect_unit(session.drive(&Request::Replay(snap))?, "replay")
        })?;
        self.actions = snapshot.actions.clone();
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.work_ram = snapshot.work_ram.clone();
        self.failed = snapshot.failed;
        Ok(())
    }

    /// Capture a portable reference to the real whole-VM endpoint snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Option<ConsonanceNovaSnapshot> {
        (!self.failed).then(|| ConsonanceNovaSnapshot {
            actions: self.actions.clone(),
            observation: self.observation.clone(),
            work_ram: self.work_ram.clone(),
            failed: false,
        })
    }

    /// Test a fixed neutral/button continuation, then return to the caller's prefix.
    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return false;
        }
        let saved = self.snapshot();
        let Some(saved) = saved else {
            return false;
        };
        let mut remaining = frames;
        while remaining > 0 && !self.is_dead() && !self.failed {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            let Ok(hold) = u8::try_from(hold) else {
                self.failed = true;
                break;
            };
            self.apply(ButtonChord::new(buttons, hold));
            remaining -= u16::from(hold);
        }
        let survived = !self.failed && !self.is_dead();
        if self.restore(&saved).is_err() {
            self.failed = true;
            false
        } else {
            survived
        }
    }

    /// Restore the sealed gameplay genesis.
    pub fn reset(&mut self) {
        let result = with_session(&self.config, Session::reset_to_setup);
        match result {
            Ok(observed) if observed.work_frames.is_empty() => {
                let decoded = decode_state(&observed.endpoint_work_ram, &observed.save_ram);
                if let Ok(state) = decoded
                    && observed.dead == (state.health == 0)
                {
                    self.actions.clear();
                    self.work_ram = observed.endpoint_work_ram.to_vec();
                    self.observation =
                        make_observation(observed.frame_count, state, &self.work_ram, &[]);
                    self.action_observations = vec![self.observation.clone()];
                    self.failed = false;
                } else {
                    self.failed = true;
                }
            }
            Ok(_) | Err(_) => self.failed = true,
        }
    }

    /// Apply one opaque controller chord through the SDK payload service.
    pub fn apply(&mut self, action: ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.is_dead() || self.cleared_a_level() {
            return;
        }
        let mut prior_work_ram = self.work_ram.clone();
        let mut prior_state = self.observation.decoded;
        let before_frame = self.observation.frame_count;
        let result = with_session(&self.config, |session| {
            let before_faults = session.profile.enabled.then(host_minor_faults).flatten();
            let dirty_before = session.profile.dirty_totals();
            let (_, doorbell_exits) = session.advance(&self.actions, action)?;
            let observed = session.observe()?;
            session.profile.record_action(
                u64::try_from(observed.work_frames.len()).unwrap_or(u64::MAX),
                doorbell_exits,
                before_faults.and_then(|before| {
                    host_minor_faults().map(|after| after.saturating_sub(before))
                }),
                dirty_before,
            );
            Ok(observed)
        });
        match result {
            Ok(observed) => {
                let Ok(frame_count) = u64::try_from(observed.work_frames.len()) else {
                    self.failed = true;
                    return;
                };
                if observed.frame_count != before_frame.saturating_add(frame_count)
                    || observed.work_frames.is_empty()
                {
                    self.failed = true;
                    return;
                }
                for (index, work_ram) in observed.work_frames.iter().enumerate() {
                    let Ok(state) = decode_state(work_ram, &observed.save_ram) else {
                        self.failed = true;
                        return;
                    };
                    let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                        || preference_tuple(state) != preference_tuple(prior_state)
                        || state.level_reload_pending != prior_state.level_reload_pending;
                    if boundary {
                        let frame = before_frame
                            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
                            .saturating_add(1);
                        self.action_observations.push(make_observation(
                            frame,
                            state,
                            work_ram,
                            &prior_work_ram,
                        ));
                        prior_work_ram.clear();
                        prior_work_ram.extend_from_slice(work_ram);
                        prior_state = state;
                    }
                    if state.health == 0 || state.cleared_count() > self.genesis_cleared {
                        break;
                    }
                }
                let Ok(endpoint_state) =
                    decode_state(&observed.endpoint_work_ram, &observed.save_ram)
                else {
                    self.failed = true;
                    return;
                };
                if observed.dead != (endpoint_state.health == 0)
                    || observed.cleared != (endpoint_state.cleared_count() > self.genesis_cleared)
                {
                    self.failed = true;
                    return;
                }
                if !self
                    .action_observations
                    .last()
                    .is_some_and(|observation| observation.frame_count == observed.frame_count)
                {
                    self.action_observations.push(make_observation(
                        observed.frame_count,
                        endpoint_state,
                        &observed.endpoint_work_ram,
                        &prior_work_ram,
                    ));
                }
                self.actions.push(action);
                self.frames_clocked = self.frames_clocked.saturating_add(frame_count);
                self.work_ram = observed.endpoint_work_ram.to_vec();
                if let Some(observation) = self.action_observations.last() {
                    self.observation = observation.clone();
                }
            }
            Err(_) => self.failed = true,
        }
    }

    /// Game-neutral target exit classification.
    #[must_use]
    pub fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }
}

fn with_session<T>(
    config: &Arc<Config>,
    operation: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|session| session.key != config.key)
        {
            *slot = Some(Session::boot(config)?);
        }
        operation(
            slot.as_mut()
                .ok_or("Consonance session was not initialized")?,
        )
    })
}

impl Session {
    fn boot(config: &Config) -> Result<Self, String> {
        let mut profile =
            ConsonanceProfile::new(std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some());
        let boot = |kernel: &[u8], initramfs: &[u8]| {
            #[cfg(target_arch = "x86_64")]
            let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)?;
            #[cfg(target_arch = "aarch64")]
            let mut vmm = boot_selected_control(kernel, initramfs, CMDLINE, RAM)?;
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        };
        let live = boot(&config.kernel, &config.initramfs)
            .map_err(|error| format!("Consonance Nova boot compose failed: {error:?}"))?;
        let factory_kernel = config.kernel.clone();
        let factory_initramfs = config.initramfs.clone();
        let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> =
            Box::new(move || boot(&factory_kernel, &factory_initramfs));
        let mut server = ControlServer::new(live, factory);
        #[cfg(target_arch = "x86_64")]
        if profile.enabled {
            server.set_remap_factory(Box::new(move |mapping| {
                let mut vmm = compose_stock_virtual_time_restore_target(mapping, SEED)?;
                vmm.wire_snapshot_hashing();
                Ok(vmm)
            }));
        }
        server.set_restore_mode(RestoreMode::InPlace);
        match drive_profiled(&mut server, &Request::Hello(server_caps()), &mut profile)? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("Consonance hello returned {other:?}")),
        }
        let genesis = snapshot(&mut server, &mut profile)?;
        expect_unit(
            drive_profiled(
                &mut server,
                &Request::Branch {
                    snap: genesis,
                    env: payload_env(vec![vec![0, 1]; 16]),
                },
                &mut profile,
            )?,
            "bootstrap branch",
        )?;
        run_to_snapshot(&mut server, &mut profile)?;
        if profile.enabled {
            // Flatten the measured setup point once so `owned_pages` is the
            // full setup image's non-zero-page count, not one delta layer.
            server.set_max_chain_len(0);
        }
        let setup = snapshot(&mut server, &mut profile)?;
        if profile.enabled {
            server.set_max_chain_len(DEFAULT_MAX_CHAIN_LEN);
        }
        let registers = state_registers(&mut server, &mut profile)?;
        let billboard_gpa = register(&registers, REG_BILLBOARD_GPA)?;
        let billboard_len = u32::try_from(register(&registers, REG_BILLBOARD_LEN)?)
            .map_err(|_| "Nova billboard length does not fit u32".to_owned())?;
        let setup_nonzero_pages = server
            .snapshot_stats(setup)
            .map(|stats| stats.owned_pages)
            .ok_or("Consonance setup snapshot statistics are unavailable")?;
        profile.set_setup(setup_nonzero_pages, billboard_gpa, u64::from(billboard_len));
        let mut snapshots = BTreeMap::new();
        snapshots.insert(Vec::new(), setup);
        Ok(Self {
            key: config.key,
            server,
            setup,
            snapshots,
            billboard_gpa,
            billboard_len,
            profile,
        })
    }

    fn drive(&mut self, request: &Request) -> Result<Reply, String> {
        drive_profiled(&mut self.server, request, &mut self.profile)
    }

    fn reset_to_setup(&mut self) -> Result<BillboardObservation, String> {
        expect_unit(self.drive(&Request::Replay(self.setup))?, "genesis replay")?;
        let descendants = self
            .snapshots
            .iter()
            .filter_map(|(actions, snap)| (!actions.is_empty()).then_some(*snap))
            .collect::<Vec<_>>();
        for snap in descendants {
            expect_unit(self.drive(&Request::Drop(snap))?, "drop cached prefix")?;
        }
        // A reset starts an independent sequence. Retaining prior prefixes
        // would silently turn a repeated action into Replay instead of the
        // two guest doorbells whose observations this adapter promises.
        self.snapshots.retain(|actions, _| actions.is_empty());
        self.observe()
    }

    fn ensure_prefix(&mut self, actions: &[ButtonChord]) -> Result<SnapId, String> {
        if let Some(snap) = self.snapshots.get(actions).copied() {
            return Ok(snap);
        }
        let start_len = (0..actions.len())
            .rev()
            .find(|length| self.snapshots.contains_key(&actions[..*length]))
            .unwrap_or(0);
        let parent = self
            .snapshots
            .get(&actions[..start_len])
            .copied()
            .ok_or("Consonance setup snapshot is missing")?;
        let mut payloads = actions[start_len..]
            .iter()
            .map(|action| vec![action.buttons, action.bounded_hold_frames()])
            .collect::<Vec<_>>();
        payloads.push(vec![0, 1]);
        expect_unit(
            self.drive(&Request::Branch {
                snap: parent,
                env: payload_env(payloads),
            })?,
            "prefix branch",
        )?;
        let mut last = parent;
        for length in start_len + 1..=actions.len() {
            run_to_snapshot(&mut self.server, &mut self.profile)?;
            last = snapshot(&mut self.server, &mut self.profile)?;
            self.snapshots.insert(actions[..length].to_vec(), last);
        }
        Ok(last)
    }

    fn advance(
        &mut self,
        prefix: &[ButtonChord],
        action: ButtonChord,
    ) -> Result<(SnapId, u64), String> {
        let mut next = prefix.to_vec();
        next.push(action);
        if let Some(snap) = self.snapshots.get(&next).copied() {
            expect_unit(self.drive(&Request::Replay(snap))?, "cached replay")?;
            return Ok((snap, 0));
        }
        let parent = self.ensure_prefix(prefix)?;
        expect_unit(
            self.drive(&Request::Branch {
                snap: parent,
                env: payload_env(vec![
                    vec![action.buttons, action.bounded_hold_frames()],
                    vec![0, 1],
                ]),
            })?,
            "action branch",
        )?;
        let before_exits = self
            .server
            .vmm()
            .map(|vmm| vmm.doorbell_exits())
            .unwrap_or(0);
        run_to_snapshot(&mut self.server, &mut self.profile)?;
        let snap = snapshot(&mut self.server, &mut self.profile)?;
        let after_exits = self
            .server
            .vmm()
            .map(|vmm| vmm.doorbell_exits())
            .unwrap_or(0);
        self.snapshots.insert(next, snap);
        Ok((snap, after_exits.saturating_sub(before_exits)))
    }

    fn observe(&mut self) -> Result<BillboardObservation, String> {
        if u64::try_from(BILLBOARD_OBSERVATION_LEN)
            .map_err(|_| "Nova billboard observation length does not fit u64")?
            > u64::from(self.billboard_len)
        {
            return Err("Nova billboard is shorter than its observation regions".to_owned());
        }
        // Header, frame ring, and endpoint save RAM form one coherent guest
        // publication and therefore one control-protocol read per action.
        let bytes = read_exact(
            &mut self.server,
            self.billboard_gpa,
            BILLBOARD_OBSERVATION_LEN,
            &mut self.profile,
        )?;
        if bytes.get(0..4) != Some(BILLBOARD_MAGIC.as_slice()) {
            return Err("Nova billboard magic is absent".to_owned());
        }
        let version = read_u16(&bytes, 4)?;
        if version != BILLBOARD_VERSION {
            return Err(format!("unsupported Nova billboard version {version}"));
        }
        let flags = read_u16(&bytes, 6)?;
        if flags & !0b11 != 0 {
            return Err("Nova billboard has unknown endpoint flags".to_owned());
        }
        let frame_count = u64::from(read_u32(&bytes, 8)?);
        let frames_run = bytes
            .get(13)
            .copied()
            .ok_or("Nova billboard frames-run field is truncated")?;
        if frames_run > MAX_HOLD_FRAMES || frame_count < u64::from(frames_run) {
            return Err("Nova billboard frame count is malformed".to_owned());
        }
        let work_offset = usize::try_from(read_u32(&bytes, 16)?)
            .map_err(|_| "Nova billboard work-RAM offset does not fit usize")?;
        let work_len = usize::try_from(read_u32(&bytes, 20)?)
            .map_err(|_| "Nova billboard work-RAM length does not fit usize")?;
        let save_offset = usize::try_from(read_u32(&bytes, 24)?)
            .map_err(|_| "Nova billboard save-RAM offset does not fit usize")?;
        let save_len = usize::try_from(read_u32(&bytes, 28)?)
            .map_err(|_| "Nova billboard save-RAM length does not fit usize")?;
        if (work_offset, work_len, save_offset, save_len)
            != (
                BILLBOARD_WORK_RAM_OFFSET,
                BILLBOARD_WORK_RAM_LEN,
                BILLBOARD_SAVE_RAM_OFFSET,
                BILLBOARD_SAVE_RAM_LEN,
            )
        {
            return Err("Nova billboard observation regions are malformed".to_owned());
        }

        let slot = |index: usize| -> Result<[u8; WRAM_SIZE], String> {
            let start = work_offset
                .checked_add(index.saturating_mul(WRAM_SIZE))
                .ok_or("Nova billboard work-RAM slot offset overflow")?;
            let end = start
                .checked_add(WRAM_SIZE)
                .ok_or("Nova billboard work-RAM slot end overflow")?;
            bytes
                .get(start..end)
                .ok_or_else(|| "Nova billboard work-RAM slot is truncated".to_owned())?
                .try_into()
                .map_err(|_| "Nova billboard work-RAM slot is malformed".to_owned())
        };
        let work_frames = (0..usize::from(frames_run))
            .map(slot)
            .collect::<Result<Vec<_>, _>>()?;
        let endpoint_work_ram = slot(usize::from(frames_run.saturating_sub(1)))?;
        let save_end = save_offset
            .checked_add(save_len)
            .ok_or("Nova billboard save-RAM end overflow")?;
        let save_ram = bytes
            .get(save_offset..save_end)
            .ok_or("Nova billboard save-RAM window is truncated")?
            .try_into()
            .map_err(|_| "Nova billboard save-RAM window is malformed")?;
        Ok(BillboardObservation {
            frame_count,
            work_frames,
            endpoint_work_ram,
            save_ram,
            dead: flags & 1 != 0,
            cleared: flags & 2 != 0,
        })
    }
}

fn make_observation(
    frame_count: u64,
    state: NovaMechanicalState,
    work_ram: &[u8],
    prior: &[u8],
) -> NovaObservations {
    let changed_indices = work_ram
        .iter()
        .zip(prior)
        .enumerate()
        .filter_map(|(index, (current, previous))| {
            (current != previous)
                .then(|| u16::try_from(index).ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    NovaObservations {
        frame_count,
        decoded: state,
        changed_indices: changed_indices.clone(),
        dead: state.health == 0,
        log_line: format!("frame={frame_count} changed={changed_indices:?}"),
    }
}

fn payload_env(payloads: Vec<Vec<u8>>) -> Reproducer {
    let mut spec = EnvSpec::Seeded {
        seed: SEED,
        policy: FaultPolicy::none(),
    };
    spec.set_payloads(Some(payloads));
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
}

fn drive_profiled(
    server: &mut Server,
    request: &Request,
    profile: &mut ConsonanceProfile,
) -> Result<Reply, String> {
    // Profiling is explicitly opt-in and these timestamps are never consulted
    // by the deterministic campaign path. The disabled branch does not call
    // the wall clock at all.
    #[allow(clippy::disallowed_methods)] // not order-observable: live profiling only.
    let started = profile.enabled.then(std::time::Instant::now);
    let result = server.handle(request);
    #[allow(clippy::disallowed_methods)] // not order-observable: live profiling only.
    if let Some(started) = started {
        profile.record_verb(request, started.elapsed().as_nanos());
    }
    if matches!(request, Request::Snapshot)
        && let Ok(Ok(Reply::Snapshot { id, .. })) = &result
    {
        profile.record_seal(
            server.last_seal_dirty_gfns(),
            server.snapshot_chain_len(*id),
        );
    }
    if matches!(request, Request::Branch { .. } | Request::Replay(_))
        && matches!(result, Ok(Ok(Reply::Unit)))
    {
        profile.record_restore(
            server.last_restore_bytes_written(),
            server.in_place_fallbacks(),
        );
    }
    match result {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
        Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
    }
}

fn profile_verb(request: &Request) -> Option<ProfileVerb> {
    match request {
        Request::Branch { .. } | Request::Replay(_) => Some(ProfileVerb::Branch),
        Request::Run { .. } => Some(ProfileVerb::Run),
        Request::Snapshot => Some(ProfileVerb::Snapshot),
        Request::Read { .. } => Some(ProfileVerb::Read),
        Request::SdkEvents { .. } => Some(ProfileVerb::SdkEvents),
        _ => None,
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len() - 1).saturating_mul(percentile) / 100;
    sorted[index]
}

impl ConsonanceProfile {
    fn record_verb(&mut self, request: &Request, wall_ns: u128) {
        if let Some(verb) = profile_verb(request) {
            self.record_verb_kind(verb, wall_ns);
        }
    }
}

fn expect_unit(reply: Reply, operation: &str) -> Result<(), String> {
    match reply {
        Reply::Unit => Ok(()),
        other => Err(format!("{operation} returned {other:?}")),
    }
}

fn run_to_snapshot(server: &mut Server, profile: &mut ConsonanceProfile) -> Result<Moment, String> {
    match drive_profiled(
        server,
        &Request::Run {
            until: StopConditions {
                deadline: Some(Moment(DEADLINE)),
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        },
        profile,
    )? {
        Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime),
        other => Err(format!("expected Nova snapshot point, received {other:?}")),
    }
}

fn snapshot(server: &mut Server, profile: &mut ConsonanceProfile) -> Result<SnapId, String> {
    match drive_profiled(server, &Request::Snapshot, profile)? {
        Reply::Snapshot {
            id, tainted: false, ..
        } => Ok(id),
        Reply::Snapshot { tainted: true, .. } => Err("Nova timeline is tainted".to_owned()),
        other => Err(format!("snapshot returned {other:?}")),
    }
}

fn state_registers(
    server: &mut Server,
    profile: &mut ConsonanceProfile,
) -> Result<BTreeMap<u32, u64>, String> {
    let mut registers = BTreeMap::<u32, u64>::new();
    let mut offset = 0_u32;
    loop {
        let events = match drive_profiled(server, &Request::SdkEvents { offset }, profile)? {
            Reply::SdkEvents(events) => events,
            other => return Err(format!("SDK event fetch returned {other:?}")),
        };
        if events.is_empty() {
            break;
        }
        offset = offset
            .checked_add(u32::try_from(events.len()).map_err(|_| "SDK event page is too large")?)
            .ok_or("SDK event offset overflow")?;
        for (_, event_id, bytes) in events {
            let namespace = (event_id >> SDK_NS_SHIFT) as u8;
            let register_id = event_id & ((1 << SDK_NS_SHIFT) - 1);
            if namespace != SDK_NS_STATE || bytes.len() != 9 {
                continue;
            }
            let value = u64::from_le_bytes(
                bytes[1..9]
                    .try_into()
                    .map_err(|_| "SDK state payload is truncated")?,
            );
            match bytes[0] {
                SDK_STATE_SET => {
                    registers.insert(register_id, value);
                }
                SDK_STATE_MAX => {
                    registers
                        .entry(register_id)
                        .and_modify(|current| *current = (*current).max(value))
                        .or_insert(value);
                }
                _ => return Err("SDK state event has an unknown operation".to_owned()),
            }
        }
    }
    Ok(registers)
}

fn register(registers: &BTreeMap<u32, u64>, id: u32) -> Result<u64, String> {
    registers
        .get(&id)
        .copied()
        .ok_or_else(|| format!("Nova SDK register {id} is absent"))
}

fn read_exact(
    server: &mut Server,
    gpa: u64,
    len: usize,
    profile: &mut ConsonanceProfile,
) -> Result<Vec<u8>, String> {
    let len = u32::try_from(len).map_err(|_| "Nova observation read is too large")?;
    match drive_profiled(server, &Request::Read { gpa, len }, profile)? {
        Reply::Bytes(bytes) if bytes.len() == len as usize => Ok(bytes),
        Reply::Bytes(_) => Err("Nova observation read was truncated".to_owned()),
        other => Err(format!("Nova observation read returned {other:?}")),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset.saturating_add(2);
    let field = bytes
        .get(offset..end)
        .ok_or("Nova billboard u16 field is truncated")?;
    Ok(u16::from_le_bytes(
        field
            .try_into()
            .map_err(|_| "Nova billboard u16 field is malformed")?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.saturating_add(4);
    let field = bytes
        .get(offset..end)
        .ok_or("Nova billboard u32 field is truncated")?;
    Ok(u32::from_le_bytes(
        field
            .try_into()
            .map_err(|_| "Nova billboard u32 field is malformed")?,
    ))
}

/// Stable identity string for the Consonance-backed Nova campaign stream.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    format!(
        "consonance-whole-vm-v1;kernel-sha256={:x};initramfs-sha256={:x};sdk-input=payload-v1;snapshot=portable-prefix-to-vm-snapshot-v1",
        Sha256::digest(kernel),
        Sha256::digest(initramfs),
    )
}

/// Read a Consonance Nova target from the two guest image paths.
pub fn from_paths(kernel: &Path, initramfs: &Path) -> Result<ConsonanceNovaTarget, String> {
    let kernel = std::fs::read(kernel).map_err(|error| format!("read kernel: {error}"))?;
    let initramfs = std::fs::read(initramfs).map_err(|error| format!("read initramfs: {error}"))?;
    ConsonanceNovaTarget::new(&kernel, &initramfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_disabled_path_has_no_output_or_counters() {
        let mut profile = ConsonanceProfile::new(false);
        profile.test_record_verb(ProfileVerb::Branch, 17);
        profile.record_action(12, 3, None, [0; 4]);
        profile.record_seal(Some(&[1, 2]), Some(1));
        assert!(profile.render().is_none());
        assert_eq!(profile.calls, [0; 5]);
        assert_eq!(profile.actions, 0);
        assert_eq!(profile.dirty_pages, 0);
    }

    #[test]
    fn profile_enabled_path_formats_verb_and_page_counters() {
        let mut profile = ConsonanceProfile::new(true);
        profile.billboard = Some((profile.ram_gpa_base + PAGE_SIZE, PAGE_SIZE));
        profile.agent_ranges = vec![(
            profile.ram_gpa_base + 2 * PAGE_SIZE,
            profile.ram_gpa_base + 3 * PAGE_SIZE,
        )];
        profile.test_record_verb(ProfileVerb::Branch, 17);
        profile.test_record_verb(ProfileVerb::Run, 23);
        profile.test_record_verb(ProfileVerb::Snapshot, 29);
        profile.record_seal(None, Some(1));
        profile.test_record_verb(ProfileVerb::Snapshot, 31);
        profile.record_seal(Some(&[1, 2, 3]), Some(1));
        profile.record_action(12, 3, Some(4), [0; 4]);
        let line = profile.render().expect("enabled profile renders");
        assert!(line.contains("branch_calls=1 branch_wall_ns=17"));
        assert!(line.contains("run_calls=1 run_wall_ns=23"));
        assert!(line.contains("seals=2 dirty_available_seals=1"));
        assert!(
            line.contains(
                "flatten_calls=1 flatten_wall_ns=31 flatten_median_ns=31 flatten_p99_ns=31"
            )
        );
        assert!(line.contains("actions=1 frames=12 doorbell_exits=3"));
        assert!(line.contains("touched_pages=4"));
        assert!(line.contains("dirty_pages=3"));
        assert!(line.contains("dirty_billboard_pages=1"));
        assert!(line.contains("dirty_agent_pages=1"));
        assert!(line.contains("dirty_other_pages=1"));
        assert!(line.contains("action_dirty_pages=3"));
        assert!(line.contains("action_dirty_billboard_pages=1"));
        assert!(line.contains("action_dirty_agent_pages=1"));
        assert!(line.contains("action_dirty_other_pages=1"));
    }
}
