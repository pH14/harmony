// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whole-VM Consonance implementation of the deterministic machine boundary.
//!
//! This module is the workload-neutral bridge between the NES machine contract
//! and a Consonance control server.  It owns one server and therefore one VM;
//! payload bytes are the only game-specific operation it performs.  The guest
//! play-agent publishes a fixed billboard, which is copied once after a run and
//! then serves the small NES observation windows without touching the VM again.

use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    path::Path,
    sync::Arc,
};

use control_proto::{
    Reply, Request, SnapId as ControlSnapId, StopConditions as ControlStopConditions,
    StopMask as ControlStopMask,
};
use environment::{EnvSpec, FaultPolicy};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeserializeError};
use sha2::{Digest, Sha256};
#[cfg(target_arch = "aarch64")]
use vmm_backend::Arm64 as HostArch;
use vmm_backend::Backend;
#[cfg(target_arch = "x86_64")]
use vmm_backend::X86 as HostArch;
use vmm_core::control::{ControlServer, RestoreMode, VmmFactory, host_minor_faults, server_caps};
use vmm_core::snapshot::DEFAULT_MAX_CHAIN_LEN;
#[cfg(target_arch = "aarch64")]
use vmm_core::vendor::arm64::{board, bringup::boot_selected_control};
#[cfg(target_arch = "x86_64")]
use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;

use crate::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason, nes,
};

type Server = ControlServer<Box<dyn Backend<A = HostArch>>>;
type PortablePages = Vec<(u64, Arc<[u8; PAGE_SIZE]>)>;
type SparseExport = (PortablePages, Vec<u8>);

#[cfg(target_arch = "x86_64")]
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "aarch64")]
const RAM: usize = 128 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const RAM_GPA_BASE: u64 = 0;
#[cfg(target_arch = "aarch64")]
const RAM_GPA_BASE: u64 = board::RAM_BASE;
#[cfg(target_arch = "x86_64")]
const BOOT_BUDGET: u64 = 2_000_000_000;
#[cfg(target_arch = "aarch64")]
const BOOT_BUDGET: u64 = 20_000_000_000;
const RUN_BUDGET: u64 = BOOT_BUDGET;
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

/// Fixed billboard header length.
pub const BILLBOARD_HEADER_LEN: usize = 32;
/// Billboard magic.
pub const BILLBOARD_MAGIC: &[u8; 4] = b"HBBD";
/// Billboard wire-layout version.
pub const BILLBOARD_VERSION: u16 = 2;
/// Offset of the 120-slot work-RAM ring.
pub const BILLBOARD_WORK_RAM_OFFSET: usize = BILLBOARD_HEADER_LEN;
/// Length of the work-RAM ring.
pub const BILLBOARD_WORK_RAM_LEN: usize = nes::MAX_HOLD_FRAMES as usize * nes::WRAM_SIZE;
/// Offset of the cached save-RAM window.
pub const BILLBOARD_SAVE_RAM_OFFSET: usize = BILLBOARD_WORK_RAM_OFFSET + BILLBOARD_WORK_RAM_LEN;
/// Length of the cached save-RAM window.
pub const BILLBOARD_SAVE_RAM_LEN: usize = 8 * 1024;
/// Bytes read for a coherent billboard observation.
pub const BILLBOARD_OBSERVATION_LEN: usize = BILLBOARD_SAVE_RAM_OFFSET + BILLBOARD_SAVE_RAM_LEN;
const PAGE_SIZE: usize = 4096;

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
        // A one-layer seal with a complete dirty drain is the bounded-chain
        // flatten path. The initial full base has no drained parent window.
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

/// A portable sparse Consonance snapshot.
///
/// `pages` contains only target pages that differ from the setup base, sorted
/// by GFN.  `sidecar` is the opaque control-server VM/SDK state.  Page and
/// sidecar chunks are shared with an optional export base in-process; sharing
/// metadata is deliberately absent from the Serde representation. Its memory
/// charge is the full self-contained footprint rather than only newly owned
/// chunks, because archive eviction can remove the base before this value.
pub struct ConsonancePortable {
    base: SnapId,
    image_identity: [u8; 32],
    pages: Vec<(u64, Arc<[u8; PAGE_SIZE]>)>,
    sidecar: crate::SharedState,
}

impl Clone for ConsonancePortable {
    fn clone(&self) -> Self {
        Self {
            base: self.base,
            image_identity: self.image_identity,
            pages: self.pages.clone(),
            sidecar: self.sidecar.clone(),
        }
    }
}

impl fmt::Debug for ConsonancePortable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsonancePortable")
            .field("base", &self.base)
            .field("image_identity", &self.image_identity)
            .field("pages", &self.pages.len())
            .field("sidecar", &self.sidecar)
            .finish()
    }
}

impl PartialEq for ConsonancePortable {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.image_identity == other.image_identity
            && self.pages == other.pages
            && self.sidecar == other.sidecar
    }
}

impl Eq for ConsonancePortable {}

impl ConsonancePortable {
    fn from_parts(
        base: SnapId,
        image_identity: [u8; 32],
        pages: Vec<(u64, Arc<[u8; PAGE_SIZE]>)>,
        sidecar: &[u8],
        export_base: Option<&Self>,
    ) -> Result<Self, MachineError> {
        if let Some(export_base) = export_base {
            validate_portable_identity(export_base, base, image_identity)?;
        }
        validate_pages(&pages)?;
        let pages = pages
            .into_iter()
            .map(|(gfn, page)| {
                let shared = export_base
                    .and_then(|portable| {
                        portable
                            .pages
                            .binary_search_by_key(&gfn, |(existing, _)| *existing)
                            .ok()
                            .and_then(|index| portable.pages.get(index))
                    })
                    .filter(|(_, existing)| existing.as_ref() == page.as_ref())
                    .map(|(_, existing)| Arc::clone(existing));
                match shared {
                    Some(existing) => (gfn, existing),
                    None => (gfn, page),
                }
            })
            .collect::<Vec<_>>();
        let sidecar = crate::SharedState::from_bytes(
            sidecar.to_vec(),
            export_base.map(|portable| &portable.sidecar),
        );
        Ok(Self {
            base,
            image_identity,
            pages,
            sidecar,
        })
    }

    /// The setup snapshot handle this sparse value is based on.
    #[must_use]
    pub fn base(&self) -> SnapId {
        self.base
    }

    /// Alias for [`Self::base`], naming the setup/base relationship explicitly.
    #[must_use]
    pub fn setup(&self) -> SnapId {
        self.base
    }

    /// The image identity this value was exported from.
    #[must_use]
    pub fn image_identity(&self) -> [u8; 32] {
        self.image_identity
    }

    /// Sorted sparse page list.
    #[must_use]
    pub fn pages(&self) -> &[(u64, Arc<[u8; PAGE_SIZE]>)] {
        &self.pages
    }

    /// Opaque VM/SDK sidecar bytes.
    #[must_use]
    pub fn sidecar(&self) -> Vec<u8> {
        self.sidecar.materialize()
    }

    /// Conservative resident bytes referenced by this portable value.
    ///
    /// Shared chunks are charged in full so the bound remains valid if an
    /// archive independently evicts the export base.
    #[must_use]
    pub fn memory_charge(&self) -> usize {
        self.pages
            .len()
            .saturating_mul(PAGE_SIZE)
            .saturating_add(self.sidecar.memory_charge())
    }
}

#[derive(Deserialize, Serialize)]
struct PortableWire {
    base: u64,
    image_identity: [u8; 32],
    pages: Vec<PortablePageWire>,
    sidecar: crate::SharedState,
}

#[derive(Deserialize, Serialize)]
struct PortablePageWire {
    gfn: u64,
    bytes: Vec<u8>,
}

impl Serialize for ConsonancePortable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let pages = self
            .pages
            .iter()
            .map(|(gfn, page)| PortablePageWire {
                gfn: *gfn,
                bytes: page.to_vec(),
            })
            .collect::<Vec<_>>();
        PortableWire {
            base: self.base.0,
            image_identity: self.image_identity,
            pages,
            sidecar: self.sidecar.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ConsonancePortable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PortableWire::deserialize(deserializer)?;
        let mut pages = Vec::new();
        if let Err(error) = pages.try_reserve(wire.pages.len()) {
            return Err(D::Error::custom(format!(
                "portable pages allocation failed: {error}"
            )));
        }
        for page in wire.pages {
            if page.bytes.len() != PAGE_SIZE {
                return Err(D::Error::custom("portable page is not exactly 4096 bytes"));
            }
            let bytes: [u8; PAGE_SIZE] = page
                .bytes
                .try_into()
                .map_err(|_| D::Error::custom("portable page conversion failed"))?;
            pages.push((page.gfn, Arc::new(bytes)));
        }
        validate_pages(&pages).map_err(|error| D::Error::custom(error.to_string()))?;
        Ok(Self {
            base: SnapId(wire.base),
            image_identity: wire.image_identity,
            pages,
            sidecar: wire.sidecar,
        })
    }
}

fn validate_pages(pages: &[(u64, Arc<[u8; PAGE_SIZE]>)]) -> Result<(), MachineError> {
    if pages.windows(2).any(|window| window[0].0 >= window[1].0) {
        return Err(MachineError::Backend(
            "portable pages are not strictly sorted".to_owned(),
        ));
    }
    Ok(())
}

fn validate_portable_identity(
    portable: &ConsonancePortable,
    setup: SnapId,
    image_identity: [u8; 32],
) -> Result<(), MachineError> {
    if portable.base != setup || portable.image_identity != image_identity {
        return Err(MachineError::Backend(
            "portable snapshot setup/base identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

struct BillboardObservation {
    work_frames: Vec<[u8; nes::WRAM_SIZE]>,
    endpoint_work_ram: [u8; nes::WRAM_SIZE],
    save_ram: [u8; BILLBOARD_SAVE_RAM_LEN],
}

/// One Consonance VM implementing [`Machine`].
pub struct ConsonanceMachine {
    server: Server,
    setup: SnapId,
    image_identity: [u8; 32],
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
        let image_identity = image_identity(kernel, initramfs);
        let mut profile =
            ConsonanceProfile::new(std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some());
        let boot = |kernel: &[u8], initramfs: &[u8]| {
            #[cfg(target_arch = "x86_64")]
            let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)
                .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            #[cfg(target_arch = "aarch64")]
            let mut vmm = boot_selected_control(kernel, initramfs, CMDLINE, RAM)
                .map_err(|error| format!("Consonance boot compose failed: {error:?}"))?;
            vmm.wire_snapshot_hashing();
            Ok::<_, String>(vmm)
        };
        let live = boot(kernel, initramfs).map_err(MachineError::Backend)?;
        let factory_kernel = kernel.to_vec();
        let factory_initramfs = initramfs.to_vec();
        let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> = Box::new(move || {
            boot(&factory_kernel, &factory_initramfs)
                .map_err(vmm_core::vmm::VmmError::ContractViolation)
        });
        let mut server = ControlServer::new(live, factory);
        server.set_restore_mode(RestoreMode::InPlace);
        match drive_profiled(&mut server, &Request::Hello(server_caps()), &mut profile)? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => {
                return Err(MachineError::Backend(format!(
                    "Consonance hello returned {other:?}"
                )));
            }
        }
        let (genesis, genesis_vtime) = snapshot(&mut server, &mut profile)?;
        expect_unit(
            drive_profiled(
                &mut server,
                &Request::Branch {
                    snap: control_snap(genesis),
                    env: control_reproducer(payload_env(vec![vec![0, 1]; 16])),
                },
                &mut profile,
            )?,
            "bootstrap branch",
        )?;
        run_to_snapshot(&mut server, &mut profile)?;
        if profile.enabled {
            // The profile's setup-page measurement should describe a complete
            // setup image rather than a derive delta. Restore the normal bound
            // immediately after this opt-in-only measurement.
            server.set_max_chain_len(0);
        }
        let (setup, setup_vtime) = snapshot(&mut server, &mut profile)?;
        if profile.enabled {
            server.set_max_chain_len(DEFAULT_MAX_CHAIN_LEN);
        }
        // Ensure setup is retained as a live in-place restore point before the
        // first observation; this also proves the one-VM restore path at boot.
        expect_unit(
            drive_profiled(
                &mut server,
                &Request::Replay(control_snap(setup)),
                &mut profile,
            )?,
            "setup replay",
        )?;
        let registers = state_registers(&mut server, &mut profile)?;
        let billboard_gpa = register(&registers, REG_BILLBOARD_GPA)?;
        let billboard_len = u32::try_from(register(&registers, REG_BILLBOARD_LEN)?)
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
            &mut server,
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
        let setup_nonzero_pages = server
            .snapshot_stats(control_snap(setup))
            .map(|stats| stats.owned_pages)
            .ok_or_else(|| {
                MachineError::Backend("setup snapshot statistics unavailable".to_owned())
            })?;
        profile.set_setup(setup_nonzero_pages, billboard_gpa, u64::from(billboard_len));
        let snapshot_vtimes = BTreeMap::from([(genesis, genesis_vtime), (setup, setup_vtime)]);
        Ok(Self {
            server,
            setup,
            image_identity,
            billboard_gpa,
            billboard_len,
            endpoint_work_ram: observed.endpoint_work_ram,
            save_ram: observed.save_ram,
            ring: Vec::new(),
            lifetime_frames: 0,
            last_vtime: setup_vtime,
            snapshot_vtimes,
            observation_valid: true,
            profile,
        })
    }

    fn export_sparse(&self, base: SnapId, target: SnapId) -> Result<SparseExport, MachineError> {
        let sparse = self
            .server
            .export_sparse_snapshot(control_snap(base), control_snap(target))
            .map_err(|error| MachineError::Backend(format!("sparse export failed: {error}")))?;
        Ok((sparse.pages, sparse.sidecar))
    }

    fn import_sparse(
        &mut self,
        base: SnapId,
        portable: &ConsonancePortable,
    ) -> Result<SnapId, MachineError> {
        let receipt = self
            .server
            .import_sparse_snapshot_parts(
                control_snap(base),
                &portable.pages,
                &portable.sidecar.materialize(),
            )
            .map_err(|error| MachineError::Backend(format!("sparse import failed: {error}")))?;
        let snapshot = SnapId(receipt.id.0);
        self.snapshot_vtimes.insert(snapshot, receipt.at.0);
        Ok(snapshot)
    }
}

impl Machine for ConsonanceMachine {
    type Portable = ConsonancePortable;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let (snapshot, vtime) = snapshot(&mut self.server, &mut self.profile)?;
        self.snapshot_vtimes.insert(snapshot, vtime);
        Ok(snapshot)
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        expect_unit(
            drive_profiled(
                &mut self.server,
                &Request::Drop(control_snap(snap)),
                &mut self.profile,
            )?,
            "drop snapshot",
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
        expect_unit(
            drive_profiled(
                &mut self.server,
                &Request::Branch {
                    snap: control_snap(snap),
                    env: control_reproducer(payload_env(payloads)),
                },
                &mut self.profile,
            )?,
            "branch",
        )?;
        self.last_vtime = restored_vtime;
        self.observation_valid = false;
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let restored_vtime = *self.snapshot_vtimes.get(&snap).ok_or_else(|| {
            MachineError::Backend("replayed snapshot has no recorded V-time".to_owned())
        })?;
        expect_unit(
            drive_profiled(
                &mut self.server,
                &Request::Replay(control_snap(snap)),
                &mut self.profile,
            )?,
            "replay",
        )?;
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
        let before_doorbells = self
            .server
            .vmm()
            .map(|vmm| vmm.doorbell_exits())
            .unwrap_or(0);
        let stop = match drive_profiled(
            &mut self.server,
            &Request::Run {
                until: ControlStopConditions {
                    deadline: Some(control_proto::Moment(deadline)),
                    on: ControlStopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
                },
                resolve: None,
            },
            &mut self.profile,
        )? {
            Reply::Stop(stop) => stop,
            other => {
                return Err(MachineError::Backend(format!("run returned {other:?}")));
            }
        };
        let mapped = map_stop(stop);
        self.last_vtime = stop_vtime(&mapped).0;
        if !matches!(mapped, StopReason::SnapshotPoint { .. }) {
            return Ok(mapped);
        }
        let observed = read_billboard(
            &mut self.server,
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
            self.server
                .vmm()
                .map(|vmm| vmm.doorbell_exits().saturating_sub(before_doorbells))
                .unwrap_or(0),
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
        if let Some(base) = base {
            validate_portable_identity(base, self.setup, self.image_identity)?;
        }
        let (pages, sidecar) = self.export_sparse(self.setup, snap)?;
        ConsonancePortable::from_parts(self.setup, self.image_identity, pages, &sidecar, base)
    }

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
        validate_portable_identity(portable, self.setup, self.image_identity)?;
        self.import_sparse(self.setup, portable)
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

fn control_snap(snap: SnapId) -> ControlSnapId {
    ControlSnapId(snap.0)
}

fn control_reproducer(env: Reproducer) -> control_proto::Reproducer {
    control_proto::Reproducer {
        blob_version: env.blob_version,
        bytes: env.bytes,
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
) -> Result<Reply, MachineError> {
    // This is explicitly opt-in.  The disabled path does not call a wall
    // clock or getrusage, so measurements cannot influence machine behavior.
    #[allow(clippy::disallowed_methods)] // not order-observable: opt-in profiling only.
    let started = profile.enabled.then(std::time::Instant::now);
    let result = server.handle(request);
    #[allow(clippy::disallowed_methods)] // not order-observable: opt-in profiling only.
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
        Ok(Err(error)) => Err(MachineError::Backend(format!(
            "{request:?} returned {error:?}"
        ))),
        Err(error) => Err(MachineError::Backend(format!(
            "{request:?} ended the session: {error:?}"
        ))),
    }
}

impl ConsonanceProfile {
    fn record_verb(&mut self, request: &Request, wall_ns: u128) {
        let verb = match request {
            Request::Branch { .. } | Request::Replay(_) => Some(ProfileVerb::Branch),
            Request::Run { .. } => Some(ProfileVerb::Run),
            Request::Snapshot => Some(ProfileVerb::Snapshot),
            Request::Read { .. } => Some(ProfileVerb::Read),
            Request::SdkEvents { .. } => Some(ProfileVerb::SdkEvents),
            _ => None,
        };
        if let Some(verb) = verb {
            self.record_verb_kind(verb, wall_ns);
        }
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1).saturating_mul(percentile) / 100]
}

fn expect_unit(reply: Reply, operation: &str) -> Result<(), MachineError> {
    match reply {
        Reply::Unit => Ok(()),
        other => Err(MachineError::Backend(format!(
            "{operation} returned {other:?}"
        ))),
    }
}

fn snapshot(
    server: &mut Server,
    profile: &mut ConsonanceProfile,
) -> Result<(SnapId, u64), MachineError> {
    match drive_profiled(server, &Request::Snapshot, profile)? {
        Reply::Snapshot {
            id,
            at,
            tainted: false,
            ..
        } => Ok((SnapId(id.0), at.0)),
        Reply::Snapshot { tainted: true, .. } => Err(MachineError::Backend(
            "Consonance timeline is tainted".to_owned(),
        )),
        other => Err(MachineError::Backend(format!(
            "snapshot returned {other:?}"
        ))),
    }
}

fn run_to_snapshot(
    server: &mut Server,
    profile: &mut ConsonanceProfile,
) -> Result<Moment, MachineError> {
    match drive_profiled(
        server,
        &Request::Run {
            until: ControlStopConditions {
                deadline: Some(control_proto::Moment(BOOT_BUDGET)),
                on: ControlStopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        },
        profile,
    )? {
        Reply::Stop(control_proto::StopReason::SnapshotPoint { vtime }) => Ok(Moment(vtime.0)),
        other => Err(MachineError::Backend(format!(
            "expected Consonance snapshot point, received {other:?}"
        ))),
    }
}

fn state_registers(
    server: &mut Server,
    profile: &mut ConsonanceProfile,
) -> Result<BTreeMap<u32, u64>, MachineError> {
    let mut registers = BTreeMap::<u32, u64>::new();
    let mut offset = 0_u32;
    loop {
        let events = match drive_profiled(server, &Request::SdkEvents { offset }, profile)? {
            Reply::SdkEvents(events) => events,
            other => {
                return Err(MachineError::Backend(format!(
                    "SDK event fetch returned {other:?}"
                )));
            }
        };
        if events.is_empty() {
            break;
        }
        offset = offset
            .checked_add(
                u32::try_from(events.len())
                    .map_err(|_| MachineError::Backend("SDK event page is too large".to_owned()))?,
            )
            .ok_or_else(|| MachineError::Backend("SDK event offset overflow".to_owned()))?;
        for (_, event_id, bytes) in events {
            let namespace = (event_id >> SDK_NS_SHIFT) as u8;
            let register_id = event_id & ((1 << SDK_NS_SHIFT) - 1);
            if namespace != SDK_NS_STATE || bytes.len() != 9 {
                continue;
            }
            let value =
                u64::from_le_bytes(bytes[1..9].try_into().map_err(|_| {
                    MachineError::Backend("SDK state payload is truncated".to_owned())
                })?);
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
                _ => {
                    return Err(MachineError::Backend(
                        "SDK state event has an unknown operation".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(registers)
}

fn register(registers: &BTreeMap<u32, u64>, id: u32) -> Result<u64, MachineError> {
    registers
        .get(&id)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("SDK register {id} is absent")))
}

fn read_billboard(
    server: &mut Server,
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
    let bytes = match drive_profiled(
        server,
        &Request::Read {
            gpa: billboard_gpa,
            len: u32::try_from(BILLBOARD_OBSERVATION_LEN).map_err(|_| {
                MachineError::Backend("billboard observation length exceeds u32".to_owned())
            })?,
        },
        profile,
    )? {
        Reply::Bytes(bytes) if bytes.len() == BILLBOARD_OBSERVATION_LEN => bytes,
        Reply::Bytes(_) => {
            return Err(MachineError::Backend(
                "billboard read was truncated".to_owned(),
            ));
        }
        other => {
            return Err(MachineError::Backend(format!(
                "billboard read returned {other:?}"
            )));
        }
    };
    parse_billboard(&bytes, require_frames)
}

fn parse_billboard(
    bytes: &[u8],
    require_frames: bool,
) -> Result<BillboardObservation, MachineError> {
    if bytes.len() < BILLBOARD_OBSERVATION_LEN {
        return Err(MachineError::Backend(
            "billboard bytes are truncated".to_owned(),
        ));
    }
    if bytes.get(0..4) != Some(BILLBOARD_MAGIC.as_slice()) {
        return Err(MachineError::Backend(
            "billboard magic is absent".to_owned(),
        ));
    }
    if read_u16(bytes, 4)? != BILLBOARD_VERSION {
        return Err(MachineError::Backend(
            "unsupported billboard version".to_owned(),
        ));
    }
    if read_u16(bytes, 6)? & !0b11 != 0 {
        return Err(MachineError::Backend(
            "billboard has unknown endpoint flags".to_owned(),
        ));
    }
    if bytes.get(14..16) != Some(&[0, 0]) {
        return Err(MachineError::Backend(
            "billboard reserved bytes are nonzero".to_owned(),
        ));
    }
    let frame_count = u64::from(read_u32(bytes, 8)?);
    let frames_run = bytes.get(13).copied().ok_or_else(|| {
        MachineError::Backend("billboard frames-run field is truncated".to_owned())
    })?;
    if frames_run > nes::MAX_HOLD_FRAMES || (require_frames && frames_run == 0) {
        return Err(MachineError::Backend(
            "billboard frame count is malformed".to_owned(),
        ));
    }
    let work_offset = usize::try_from(read_u32(bytes, 16)?)
        .map_err(|_| MachineError::Backend("billboard work offset overflow".to_owned()))?;
    let work_len = usize::try_from(read_u32(bytes, 20)?)
        .map_err(|_| MachineError::Backend("billboard work length overflow".to_owned()))?;
    let save_offset = usize::try_from(read_u32(bytes, 24)?)
        .map_err(|_| MachineError::Backend("billboard save offset overflow".to_owned()))?;
    let save_len = usize::try_from(read_u32(bytes, 28)?)
        .map_err(|_| MachineError::Backend("billboard save length overflow".to_owned()))?;
    if (work_offset, work_len, save_offset, save_len)
        != (
            BILLBOARD_WORK_RAM_OFFSET,
            BILLBOARD_WORK_RAM_LEN,
            BILLBOARD_SAVE_RAM_OFFSET,
            BILLBOARD_SAVE_RAM_LEN,
        )
    {
        return Err(MachineError::Backend(
            "billboard observation regions are malformed".to_owned(),
        ));
    }
    let slot = |index: usize| -> Result<[u8; nes::WRAM_SIZE], MachineError> {
        let start = work_offset
            .checked_add(index.checked_mul(nes::WRAM_SIZE).ok_or_else(|| {
                MachineError::Backend("billboard work slot offset overflow".to_owned())
            })?)
            .ok_or_else(|| {
                MachineError::Backend("billboard work slot offset overflow".to_owned())
            })?;
        let end = start
            .checked_add(nes::WRAM_SIZE)
            .ok_or_else(|| MachineError::Backend("billboard work slot end overflow".to_owned()))?;
        bytes
            .get(start..end)
            .ok_or_else(|| MachineError::Backend("billboard work slot is truncated".to_owned()))?
            .try_into()
            .map_err(|_| MachineError::Backend("billboard work slot is malformed".to_owned()))
    };
    let work_frames = (0..usize::from(frames_run))
        .map(slot)
        .collect::<Result<Vec<_>, _>>()?;
    let endpoint_work_ram = slot(usize::from(frames_run.saturating_sub(1)))?;
    let save_end = save_offset
        .checked_add(save_len)
        .ok_or_else(|| MachineError::Backend("billboard save end overflow".to_owned()))?;
    let save_ram = bytes
        .get(save_offset..save_end)
        .ok_or_else(|| MachineError::Backend("billboard save window is truncated".to_owned()))?
        .try_into()
        .map_err(|_| MachineError::Backend("billboard save window is malformed".to_owned()))?;
    if frame_count < u64::from(frames_run) {
        return Err(MachineError::Backend(
            "billboard frame count is malformed".to_owned(),
        ));
    }
    Ok(BillboardObservation {
        work_frames,
        endpoint_work_ram,
        save_ram,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, MachineError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| MachineError::Backend("billboard u16 offset overflow".to_owned()))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| MachineError::Backend("billboard u16 field is truncated".to_owned()))?
        .try_into()
        .map(u16::from_le_bytes)
        .map_err(|_| MachineError::Backend("billboard u16 field is malformed".to_owned()))
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

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MachineError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| MachineError::Backend("billboard u32 offset overflow".to_owned()))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| MachineError::Backend("billboard u32 field is truncated".to_owned()))?
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| MachineError::Backend("billboard u32 field is malformed".to_owned()))
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

fn image_identity(kernel: &[u8], initramfs: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(kernel);
    digest.update(initramfs);
    digest.finalize().into()
}

/// Stable identity string for a Consonance whole-VM campaign stream.
#[must_use]
pub fn identity(kernel: &[u8], initramfs: &[u8]) -> String {
    format!(
        "consonance-whole-vm-v1;kernel-sha256={:x};initramfs-sha256={:x};sdk-input=payload-v1;snapshot=sparse-pages-plus-sidecar-v1",
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
    use serde::de::DeserializeOwned;
    use serde::ser::{Impossible, SerializeSeq};

    #[derive(Debug)]
    struct ByteCodecError;

    impl fmt::Display for ByteCodecError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("byte codec error")
        }
    }

    impl std::error::Error for ByteCodecError {}

    impl serde::ser::Error for ByteCodecError {
        fn custom<T: fmt::Display>(_message: T) -> Self {
            Self
        }
    }

    impl serde::de::Error for ByteCodecError {
        fn custom<T: fmt::Display>(_message: T) -> Self {
            Self
        }
    }

    struct ByteSequence<'a> {
        bytes: &'a mut Vec<u8>,
    }

    impl SerializeSeq for ByteSequence<'_> {
        type Ok = ();
        type Error = ByteCodecError;

        fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
        where
            T: ?Sized + Serialize,
        {
            value.serialize(ByteSequence {
                bytes: &mut *self.bytes,
            })
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            Ok(())
        }
    }

    impl serde::Serializer for ByteSequence<'_> {
        type Ok = ();
        type Error = ByteCodecError;
        type SerializeSeq = Self;
        type SerializeTuple = Impossible<Self::Ok, Self::Error>;
        type SerializeTupleStruct = Impossible<Self::Ok, Self::Error>;
        type SerializeTupleVariant = Impossible<Self::Ok, Self::Error>;
        type SerializeMap = Impossible<Self::Ok, Self::Error>;
        type SerializeStruct = Impossible<Self::Ok, Self::Error>;
        type SerializeStructVariant = Impossible<Self::Ok, Self::Error>;

        fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
            self.bytes.push(value);
            Ok(())
        }

        fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            Ok(self)
        }

        fn serialize_bool(self, _: bool) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_i8(self, _: i8) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_i16(self, _: i16) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_i32(self, _: i32) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_i64(self, _: i64) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_i128(self, _: i128) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_u16(self, _: u16) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_u32(self, _: u32) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_u64(self, _: u64) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_u128(self, _: u128) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_f32(self, _: f32) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_f64(self, _: f64) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_char(self, _: char) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_str(self, _: &str) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_some<T>(self, _: &T) -> Result<Self::Ok, Self::Error>
        where
            T: ?Sized + Serialize,
        {
            Err(ByteCodecError)
        }

        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_unit_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_newtype_struct<T>(
            self,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Self::Error>
        where
            T: ?Sized + Serialize,
        {
            Err(ByteCodecError)
        }

        fn serialize_newtype_variant<T>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Self::Error>
        where
            T: ?Sized + Serialize,
        {
            Err(ByteCodecError)
        }

        fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_tuple_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Err(ByteCodecError)
        }

        fn serialize_struct_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            Err(ByteCodecError)
        }
    }

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
    fn portable_pages_share_but_charge_survives_base_eviction() {
        let mut first = [0_u8; PAGE_SIZE];
        first[0] = 1;
        let mut second = [0_u8; PAGE_SIZE];
        second[0] = 2;
        let base = ConsonancePortable::from_parts(
            SnapId(1),
            [3; 32],
            vec![(1, Arc::new(first)), (2, Arc::new(second))],
            &[7; 513],
            None,
        )
        .expect("base");
        let child = ConsonancePortable::from_parts(
            SnapId(1),
            [3; 32],
            base.pages.clone(),
            &[7; 513],
            Some(&base),
        )
        .expect("child");
        assert_eq!(child.memory_charge(), 2 * PAGE_SIZE + 2 * 512);
        assert_eq!(child, base);
        let debug = format!("{child:?}");
        assert!(!debug.contains("00000001"));

        let wrong_setup = ConsonancePortable::from_parts(SnapId(2), [3; 32], Vec::new(), &[], None)
            .expect("wrong setup artifact");
        assert!(validate_portable_identity(&wrong_setup, SnapId(1), [3; 32]).is_err());
    }

    #[test]
    fn portable_serde_is_vec_compatible_and_deserialize_charges() {
        let sidecar = vec![4_u8; 513];
        let state = crate::SharedState::from_bytes(sidecar.clone(), None);
        let mut encoded = Vec::new();
        state
            .serialize(ByteSequence {
                bytes: &mut encoded,
            })
            .expect("serialize sidecar");
        assert_eq!(encoded, sidecar, "sidecar wire format must match Vec<u8>");
        let sequence =
            serde::de::value::SeqDeserializer::<_, ByteCodecError>::new(encoded.into_iter());
        let restored = crate::SharedState::deserialize(sequence).expect("deserialize sidecar");
        assert_eq!(restored.materialize(), sidecar);
        assert_eq!(restored.memory_charge(), 2 * 512);

        let page = Arc::new([9_u8; PAGE_SIZE]);
        let portable =
            ConsonancePortable::from_parts(SnapId(4), [8; 32], vec![(10, page)], &[4; 512], None)
                .expect("portable");
        assert_eq!(portable.memory_charge(), PAGE_SIZE + 512);
        fn assert_bounds<
            T: Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned,
        >() {
        }
        assert_bounds::<ConsonancePortable>();
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
