// SPDX-License-Identifier: AGPL-3.0-or-later

use hypercall_proto::{
    MAX_PAYLOAD, SDK_COVERAGE_QUANTUM, SDK_COVERAGE_REQUEST_LEN, SDK_COVERAGE_RESPONSE_LEN,
    SeededEntropy, Service, ServiceId, Status, decode, encode_error, encode_response,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use vm_state::SnapshotRecords;
use vmm_backend::{Arch, Backend, CommonExit, Exit};
use vtime::{IdlePlanner, VClock, VClockConfig};

use crate::engine_state::EngineState;
use crate::snapshot::SnapshotError;
use crate::vendor::Vendor;
use crate::virtual_time::LiveVirtualTimeTrace;

use environment::{channel, input_spec::ServiceConfig};

pub type VcpuOf<B> = <<B as Backend>::A as Arch>::VcpuState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalReason {
    DebugExit { code: u8 },
    Idle,
    Shutdown,
    SdkStop,
}

const REQ_GPA: usize = 0x0000_E000;
const RESP_GPA: usize = 0x0000_F000;
const HC_PAGE: usize = 4096;
const DOORBELL_MAP_GPA: usize = 0x0000_C000;
const DOORBELL_MAP_LEN: usize = 4 * HC_PAGE;

const SDK_NS_SHIFT: u32 = 24;
const SDK_LOCAL_MASK: u32 = (1 << SDK_NS_SHIFT) - 1;
const SDK_NS_ASSERT: u8 = 1;
const SDK_NS_LIFECYCLE: u8 = 4;
const SDK_DISP_VIOLATION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdkStop {
    Quiescent,
    Assertion {
        id: u32,
        data: Vec<u8>,
    },
    Decision {
        moment: u64,
        seq: u32,
        question: channel::Question,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SdkEventAction {
    Stop(SdkStop),
    DeferSnapshot,
    Capture,
    Malformed,
}

#[derive(Debug, thiserror::Error)]
pub enum VmmError {
    #[error("backend error: {0}")]
    Backend(#[from] vmm_backend::BackendError),
    #[error("vendor boot error: {0}")]
    VendorBoot(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("contract violation: {0}")]
    ContractViolation(String),
    #[error("v-time error: {0}")]
    Vtime(#[from] vtime::VtimeError),
    #[error("snapshot error")]
    Snapshot(#[from] crate::snapshot::SnapshotError),
    #[error("service channel error")]
    Channel(#[from] channel::ChannelError),
}

impl VmmError {
    pub fn vendor_boot<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        VmmError::VendorBoot(Box::new(err))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    Continued,
    Terminal(TerminalReason),
    SdkStop,
}

struct ExitProgress<A: Arch> {
    step: Step,
    continuation: Option<Exit<A>>,
}

pub struct RunResult {
    pub reason: TerminalReason,
    pub sdk_stop: Option<SdkStop>,
    pub serial: Vec<u8>,
    pub exit_counts: vmm_backend::ExitCounts,
}

pub enum RamBacking {
    Owned(GuestRam),
    Snapshot(snapshot_store::Mapping),
}

impl RamBacking {
    pub fn len(&self) -> usize {
        match self {
            RamBacking::Owned(ram) => ram.len(),
            RamBacking::Snapshot(map) => map.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            RamBacking::Owned(ram) => ram.as_bytes(),
            RamBacking::Snapshot(map) => map.as_slice(),
        }
    }

    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        match self {
            RamBacking::Owned(ram) => ram.as_mut_bytes(),
            RamBacking::Snapshot(map) => map.as_mut_slice(),
        }
    }
}

fn valid_guest_ram_len(len: usize, page_size: usize) -> bool {
    len != 0 && len.is_multiple_of(page_size)
}

pub struct GuestRam {
    #[cfg(not(miri))]
    inner: memmap2::MmapMut,
    #[cfg(miri)]
    inner: Vec<u8>,
}

impl GuestRam {
    pub fn new(len: usize) -> Result<Self, VmmError> {
        if len == 0 || !len.is_multiple_of(4096) {
            return Err(VmmError::Backend(vmm_backend::BackendError::Memory(
                "guest RAM length must be a non-zero multiple of 4 KiB",
            )));
        }
        #[cfg(not(miri))]
        let inner = memmap2::MmapMut::map_anon(len)
            .map_err(|e| VmmError::Backend(vmm_backend::BackendError::Io(e)))?;
        #[cfg(miri)]
        let inner = vec![0u8; len];
        Ok(Self { inner })
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.inner
    }

    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.inner
    }
}

pub struct VtimeWiring {
    pub(crate) cfg: VClockConfig,
    pub(crate) clock: VClock,
    pub(crate) entropy: SeededEntropy,
    pub(crate) guest_clock_offset: u64,
}

impl VtimeWiring {
    pub fn new_virtual_time(cfg: VClockConfig, seed: u64) -> Result<VtimeWiring, VmmError> {
        Ok(VtimeWiring {
            cfg,
            clock: VClock::new(cfg)?,
            entropy: SeededEntropy::new(seed),
            guest_clock_offset: 0,
        })
    }

    pub fn advance_virtual_time(&mut self, vns_delta: u64) {
        self.clock.advance(vns_delta);
    }

    pub fn virtual_time_vns(&self) -> u64 {
        self.clock.vns()
    }

    #[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
    pub(crate) fn next_entropy_word(&mut self) -> Result<u64, VmmError> {
        let mut buf = [0u8; 8];
        let (status, got) = self.entropy.handle(1, &8u32.to_le_bytes(), &mut buf);
        if (status, got) != (Status::Ok, buf.len()) {
            return Err(VmmError::ContractViolation(format!(
                "seeded entropy draw failed (status {status:?}, got {got} of 8 bytes)"
            )));
        }
        Ok(u64::from_le_bytes(buf))
    }

    pub(crate) fn draw_entropy(&mut self, req: &[u8], resp: &mut [u8]) -> (Status, usize) {
        self.entropy.handle(1, req, resp)
    }

    pub(crate) fn guest_clock(&self) -> u64 {
        self.clock
            .guest_ticks()
            .wrapping_add(self.guest_clock_offset)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VtimeSnapshot {
    pub vns: u64,
    pub guest_clock_offset: u64,
    pub entropy: Vec<u8>,
}

const EVENT_TRACE_CAP: usize = 4096;

pub const PVCLOCK_REFRESH_TRACE_CAP: usize = EVENT_TRACE_CAP;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StampKind {
    Refresh,
    Canonical,
}

fn synchronous_checkpoint_due(checkpoint: bool, deferred: bool) -> bool {
    checkpoint && !deferred
}

enum IdleAction {
    Terminal,
    DeliverPending,
    JumpToDeadline(u64),
}

pub(crate) struct SdkChannel {
    env: channel::RecordedEnv<Box<dyn channel::ServiceHandler>>,
    events: Vec<(u64, u32, Vec<u8>)>,
    coverage_thresholds: BTreeMap<u32, u64>,
    coverage: Vec<(u64, u32, u64, u32, u32)>,
    pending_stop: Option<SdkStop>,
    pending_snapshot: bool,
}

pub(crate) struct PvclockChannel {
    gpa: Option<u64>,
    armed: bool,
    refreshes: Vec<(u64, u64)>,
}

#[derive(Clone, Debug)]
pub struct SdkSnapshot {
    pub(crate) recorded: channel::RecordedState,
    pub(crate) events: Vec<(u64, u32, Vec<u8>)>,
    pub(crate) pending_snapshot: bool,
    pub(crate) pending_stop: Option<SdkStop>,
    pub(crate) coverage_thresholds: BTreeMap<u32, u64>,
}

impl SdkSnapshot {
    pub(crate) fn remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.recorded.remaining_payloads()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PvclockSnapshot {
    pub(crate) gpa: Option<u64>,
    pub(crate) registrable: bool,
    pub(crate) armed: bool,
}

pub struct Vmm<B: Backend>
where
    B::A: Vendor,
{
    pub(crate) backend: B,
    pub(crate) ram: RamBacking,
    pub(crate) ram_base_gpa: u64,
    pub(crate) doorbell_pages: Option<GuestRam>,
    pub(crate) devices: <B::A as Vendor>::Devices,
    pub(crate) host_dirty: std::collections::BTreeSet<u64>,
    pub(crate) host_dirty_wholesale: bool,
    pub(crate) report_stream: Vec<u32>,
    pub(crate) idle_landings: Vec<u64>,
    pub(crate) terminal: Option<TerminalReason>,
    pub(crate) saved_state: Option<VcpuOf<B>>,
    pub(crate) vtime: Option<VtimeWiring>,
    pub(crate) virtual_time_trace: Option<LiveVirtualTimeTrace>,
    pub(crate) doorbell_exits: u64,
    pub(crate) deferred_virtual_time_checkpoints: bool,
    virtual_time_checkpoint_events: BTreeSet<u64>,
    pub(crate) completion_staged: bool,
    pub(crate) sdk_snapshot_reentry_required: bool,
    pub(crate) snapshot_hashing: bool,
    pub(crate) idle_wake_vns: Option<u64>,
    pub(crate) sdk: Option<SdkChannel>,
    pub(crate) pvclock: Option<PvclockChannel>,
}

struct RestorePreparation<B: Backend>
where
    B::A: Vendor,
{
    engine_state: EngineState,
    vcpu: VcpuOf<B>,
    clock_offset: u64,
    vendor: <B::A as Vendor>::RestorePrep,
    vtime: Option<(VClockConfig, VClock, SeededEntropy)>,
}

impl<B: Backend> Vmm<B>
where
    B::A: Vendor,
{
    pub fn new(backend: B, guest_ram: GuestRam) -> Self {
        Self::with_backing(backend, RamBacking::Owned(guest_ram))
    }

    pub fn vcpu_record(&self) -> Result<VcpuOf<B>, VmmError> {
        Ok(self.backend.save()?)
    }

    pub(crate) fn backend(&self) -> &B {
        &self.backend
    }

    pub(crate) fn devices(&self) -> &<B::A as Vendor>::Devices {
        &self.devices
    }

    pub fn with_backing(backend: B, ram: RamBacking) -> Self {
        Self {
            backend,
            ram,
            ram_base_gpa: 0,
            doorbell_pages: None,
            devices: <B::A as Vendor>::new_devices(),
            host_dirty: std::collections::BTreeSet::new(),
            host_dirty_wholesale: false,
            report_stream: Vec::new(),
            idle_landings: Vec::new(),
            terminal: None,
            saved_state: None,
            vtime: None,
            virtual_time_trace: None,
            doorbell_exits: 0,
            deferred_virtual_time_checkpoints: false,
            virtual_time_checkpoint_events: BTreeSet::new(),
            completion_staged: false,
            sdk_snapshot_reentry_required: false,
            snapshot_hashing: false,
            idle_wake_vns: None,
            sdk: None,
            pvclock: None,
        }
    }

    pub fn wire_vtime(&mut self, wiring: VtimeWiring) -> &mut Self {
        self.virtual_time_trace = Some(LiveVirtualTimeTrace::default());
        self.virtual_time_checkpoint_events.clear();
        self.vtime = Some(wiring);
        self
    }

    pub fn virtual_time_trace(&self) -> Option<&LiveVirtualTimeTrace> {
        self.virtual_time_trace.as_ref()
    }

    pub(crate) fn take_virtual_time_trace(&mut self) -> Option<LiveVirtualTimeTrace> {
        self.virtual_time_checkpoint_events.clear();
        self.virtual_time_trace.as_mut().map(std::mem::take)
    }

    pub fn defer_virtual_time_checkpoint_hashes(&mut self) -> Result<(), VmmError> {
        let trace = self.virtual_time_trace.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "deferred checkpoint hashing without virtual_time trace".to_string(),
            )
        })?;
        if !trace.raw_log().is_empty() || !trace.normalized_log().events.is_empty() {
            return Err(VmmError::ContractViolation(
                "deferred checkpoint hashing enabled after the trace started".to_string(),
            ));
        }
        self.deferred_virtual_time_checkpoints = true;
        Ok(())
    }

    #[must_use]
    pub fn virtual_time_checkpoint_due(&self, event_index: u64) -> bool {
        self.virtual_time_checkpoint_events.contains(&event_index)
    }

    pub fn checkpoint_virtual_time_trace_at(
        &mut self,
        event_index: u64,
        state_hash: [u8; 32],
    ) -> Result<(), VmmError> {
        if !self.deferred_virtual_time_checkpoints {
            return Err(VmmError::ContractViolation(
                "deferred checkpoint installed while synchronous hashing is active".to_string(),
            ));
        }
        if !self.virtual_time_checkpoint_due(event_index) {
            return Err(VmmError::ContractViolation(format!(
                "deferred checkpoint event {event_index} is not a completed checkpoint boundary"
            )));
        }
        self.virtual_time_trace
            .as_mut()
            .ok_or_else(|| {
                VmmError::ContractViolation(
                    "deferred checkpoint installed without virtual_time trace".to_string(),
                )
            })?
            .checkpoint_at(event_index, state_hash)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub fn checkpoint_virtual_time_trace(&mut self) -> Result<(), VmmError> {
        let hash = self.state_hash()?;
        self.virtual_time_trace
            .as_mut()
            .ok_or_else(|| {
                VmmError::ContractViolation(
                    "virtual_time trace checkpoint without virtual_time V-time".to_string(),
                )
            })?
            .checkpoint_last(hash)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub(crate) fn trace_arm_clockevent_schedule(
        &mut self,
        deadline_ticks: u64,
        interrupt_id: u32,
    ) -> Result<(), VmmError> {
        let deadline_vns = self.guest_clock_deadline_vns(deadline_ticks)?;
        self.trace_clockevent_schedule_vns(deadline_vns, interrupt_id)
    }

    pub(crate) fn trace_clockevent_schedule_vns(
        &mut self,
        deadline_vns: u64,
        interrupt_id: u32,
    ) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .schedule_clockevent(deadline_vns, interrupt_id)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub(crate) fn guest_clock_deadline_vns(&self, deadline_ticks: u64) -> Result<u64, VmmError> {
        let vt = self.vtime.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "virtual_time clockevent schedule without V-time wiring".to_string(),
            )
        })?;
        if vt.guest_clock_offset != 0 {
            return Err(VmmError::ContractViolation(
                "virtual_time clockevent schedule with nonzero guest-clock offset".to_string(),
            ));
        }
        let ticks = deadline_ticks.saturating_sub(vt.cfg.guest_base);
        let numerator = u128::from(ticks) * 1_000_000_000_u128;
        let hz = u128::from(vt.cfg.guest_hz);
        if hz == 0 {
            return Err(VmmError::ContractViolation(
                "virtual_time clockevent schedule with zero guest frequency".to_string(),
            ));
        }
        let deadline_vns = numerator
            .saturating_add(hz - 1)
            .checked_div(hz)
            .unwrap_or(u128::MAX)
            .min(u128::from(u64::MAX)) as u64;
        Ok(deadline_vns)
    }

    pub(crate) fn trace_clockevent_cancel(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .cancel_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub(crate) fn trace_arm_clockevent_defer(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .defer_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub(crate) fn trace_clockevent_delivery(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .deliver_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    pub(crate) fn advance_virtual_time_vtime(&mut self, delta_vns: u64) -> Result<(), VmmError> {
        let Some(vt) = self.vtime.as_mut() else {
            return Err(VmmError::ContractViolation(
                "virtual_time exit advancement without V-time wiring".to_string(),
            ));
        };
        vt.advance_virtual_time(delta_vns);
        Ok(())
    }

    pub fn vtime_wired(&self) -> bool {
        self.vtime.is_some()
    }

    pub(crate) fn virtual_time_vtime_enabled(&self) -> bool {
        self.vtime.is_some()
    }

    pub(crate) fn map_doorbell_pages(&mut self) -> Result<(), VmmError> {
        let mut pages = GuestRam::new(DOORBELL_MAP_LEN)?;
        // SAFETY: `pages` is moved into `self.doorbell_pages` below; its backing
        // is pinned (an mmap/Vec that does not move when `self` moves) and lives
        // for the backend's lifetime because `self` owns it, and the run loop
        // holds `&mut self` so it is never aliased mid-run — the `map_memory`
        // contract, exactly as the main RAM mapping upholds it.
        unsafe {
            self.backend.map_memory(
                vmm_backend::Gpa(DOORBELL_MAP_GPA as u64),
                pages.as_mut_bytes(),
            )?;
        }
        self.doorbell_pages = Some(pages);
        Ok(())
    }

    pub fn wire_snapshot_hashing(&mut self) -> &mut Self {
        self.snapshot_hashing = true;
        self
    }

    pub fn snapshot_hashing_wired(&self) -> bool {
        self.snapshot_hashing
    }

    pub fn has_pending_guest_interrupt(&mut self) -> Result<bool, VmmError> {
        <B::A as Vendor>::has_pending_guest_interrupt(self)
    }

    pub fn guest_memory(&self) -> &[u8] {
        self.ram.as_bytes()
    }

    pub fn ram_backing_is_snapshot(&self) -> bool {
        matches!(self.ram, RamBacking::Snapshot(_))
    }

    pub fn write_guest_pages(&mut self, pages: &[(u64, [u8; 4096])]) -> Result<(), VmmError> {
        const PAGE_SIZE: usize = 4096;
        const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;

        if pages.is_empty() {
            return Ok(());
        }
        let ram_len = self.ram.len();
        if !valid_guest_ram_len(ram_len, PAGE_SIZE) {
            return Err(VmmError::ContractViolation(format!(
                "write_guest_pages: guest RAM length {ram_len} is not a non-zero multiple of {PAGE_SIZE}"
            )));
        }
        if !self.ram_base_gpa.is_multiple_of(PAGE_SIZE_U64) {
            return Err(VmmError::ContractViolation(format!(
                "write_guest_pages: main RAM base GPA {:#x} is not page-aligned",
                self.ram_base_gpa
            )));
        }

        let mut seen = std::collections::BTreeSet::new();
        let mut validated = Vec::with_capacity(pages.len());
        for (gfn, page) in pages {
            if !seen.insert(*gfn) {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: duplicate GFN {gfn}"
                )));
            }
            let gfn_usize = usize::try_from(*gfn).map_err(|_| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} does not fit the host address space"
                ))
            })?;
            let offset = gfn_usize.checked_mul(PAGE_SIZE).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page offset overflows the host address space"
                ))
            })?;
            let end = offset.checked_add(PAGE_SIZE).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page end overflows the host address space"
                ))
            })?;
            if end > ram_len {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} is outside guest RAM ({}) pages",
                    ram_len / PAGE_SIZE
                )));
            }
            let offset_u64 = u64::try_from(offset).map_err(|_| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page offset does not fit a GPA"
                ))
            })?;
            let gpa = self.ram_base_gpa.checked_add(offset_u64).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} GPA overflows the guest address space"
                ))
            })?;
            if gpa.checked_add(PAGE_SIZE_U64).is_none() {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} has an invalid GPA range"
                )));
            }
            validated.push((offset, end, page));
        }

        let ram = self.ram.as_mut_bytes();
        for &(offset, end, page) in &validated {
            ram[offset..end].copy_from_slice(page);
        }
        self.host_dirty_wholesale = true;
        Ok(())
    }

    pub(crate) fn retire_pending_completion(&mut self) -> Result<(), VmmError> {
        if !self.completion_staged {
            return Ok(());
        }
        self.backend.retire_pending_completion()?;
        self.completion_staged = false;
        self.sdk_snapshot_reentry_required = false;
        Ok(())
    }

    pub fn inject_serial_input(&mut self, bytes: &[u8]) {
        <B::A as Vendor>::inject_serial_input(&mut self.devices, bytes);
    }

    pub fn serial_output(&self) -> &[u8] {
        <B::A as Vendor>::serial_capture(&self.devices)
    }

    pub fn inspect_vcpu(&self) -> VcpuOf<B> {
        self.current_vcpu()
    }

    pub fn has_inflight_event_injection(&self) -> bool {
        <B::A as Vendor>::vcpu_has_inflight_injection(&self.current_vcpu())
    }

    pub fn has_active_event_injection(&self) -> bool {
        <B::A as Vendor>::vcpu_has_active_injection(&self.current_vcpu())
    }

    pub fn restore_guest_memory(&mut self, image: &[u8]) -> Result<(), VmmError> {
        let ram = self.ram.as_mut_bytes();
        if image.len() != ram.len() {
            return Err(VmmError::ContractViolation(format!(
                "restore_guest_memory: image is {} bytes, guest RAM is {} bytes",
                image.len(),
                ram.len()
            )));
        }
        ram.copy_from_slice(image);
        self.host_dirty_wholesale = true;
        Ok(())
    }

    pub fn save_vtime(&self) -> Result<Option<VtimeSnapshot>, VmmError> {
        match &self.vtime {
            None => Ok(None),
            Some(vt) => Ok(Some(VtimeSnapshot {
                vns: vt.clock.vns(),
                guest_clock_offset: vt.guest_clock_offset,
                entropy: vt.entropy.save_state(),
            })),
        }
    }

    pub fn effective_vns(&self) -> Option<u64> {
        self.vtime.as_ref().map(|vt| vt.clock.vns())
    }

    pub fn restore_vtime(&mut self, snap: &VtimeSnapshot) -> Result<(), VmmError> {
        let (clock, cfg, entropy) = {
            let vt = self.vtime.as_ref().ok_or_else(|| {
                VmmError::ContractViolation(
                    "restore_vtime called but V-time is not wired".to_string(),
                )
            })?;
            let mut cfg = vt.cfg;
            cfg.vns_base = snap.vns;
            let clock = VClock::new(cfg)?;
            let mut entropy = vt.entropy.clone();
            entropy.restore_state(&snap.entropy).map_err(|e| {
                VmmError::ContractViolation(format!("entropy snapshot rejected on restore: {e:?}"))
            })?;
            (clock, cfg, entropy)
        };
        let vt = self.vtime.as_mut().ok_or_else(|| {
            VmmError::ContractViolation("restore_vtime called but V-time is not wired".to_string())
        })?;
        vt.clock = clock;
        vt.cfg = cfg;
        vt.entropy = entropy;
        vt.guest_clock_offset = snap.guest_clock_offset;
        if self
            .pvclock
            .as_ref()
            .is_some_and(|pv| pv.gpa.is_some() && pv.armed)
        {
            self.pvclock_stamp(StampKind::Refresh)?;
        }
        Ok(())
    }

    pub fn save_vm_state(&self) -> Result<<B::A as Vendor>::Snapshot, VmmError> {
        let vcpu = match &self.saved_state {
            Some(s) => s.clone(),
            None => self.backend.save()?,
        };
        <B::A as Vendor>::check_sealable_vcpu(&vcpu)?;
        self.build_snapshot_state(&vcpu)
    }

    fn engine_state(&self) -> crate::engine_state::EngineState {
        crate::engine_state::EngineState {
            terminal: self.terminal,
            sdk_snapshot_reentry_required: self.sdk_snapshot_reentry_required,
        }
    }

    fn build_snapshot_state(
        &self,
        vcpu: &VcpuOf<B>,
    ) -> Result<<B::A as Vendor>::Snapshot, VmmError> {
        let mut state = <B::A as Vendor>::build_vm_state(self, vcpu);
        state.set_engine_state(self.engine_state().encode()?);
        Ok(state)
    }

    fn prepare_restore_vm_state(
        &self,
        s: &<B::A as Vendor>::Snapshot,
    ) -> Result<RestorePreparation<B>, VmmError> {
        let engine_state = EngineState::decode(s.engine_state())?;
        if *s.timers() != vm_state::TimerQueueState::default() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot carries a non-empty timer queue, but vmm-core has no \
                 TimerQueue to apply it — restoring would silently drop it. (A vmm-core snapshot \
                 always seals an empty timer queue; the fabric timer rides the device blob.)"
                    .to_string(),
            ));
        }
        let (vcpu, clock_offset, prep) = <B::A as Vendor>::validate_restore(self, s)?;
        self.backend
            .validate_restore_state(&vcpu)
            .map_err(|error| {
                VmmError::ContractViolation(format!(
                    "restore_vm_state: backend rejected snapshot shape: {error}"
                ))
            })?;
        let svt = s.vtime();
        let vtime_commit = match self.vtime.as_ref() {
            Some(vt) => {
                if svt.guest_hz != vt.cfg.guest_hz || svt.guest_base != vt.cfg.guest_base {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: V-time clock mismatch (the snapshot's guest_hz/\
                         guest_base differ from this VM's wired clock)."
                            .to_string(),
                    ));
                }
                let mut cfg = vt.cfg;
                cfg.vns_base = svt.snapshot_vns;
                let clock = VClock::new(cfg)?;
                let mut entropy = vt.entropy.clone();
                entropy.restore_state(s.entropy_bytes()).map_err(|e| {
                    VmmError::ContractViolation(format!(
                        "entropy snapshot rejected on restore: {e:?}"
                    ))
                })?;
                Some((cfg, clock, entropy))
            }
            None => {
                let is_unwired_sentinel = svt.guest_hz == 0
                    && svt.guest_base == 0
                    && svt.snapshot_vns == 0
                    && s.entropy_bytes().is_empty();
                if !is_unwired_sentinel {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: snapshot carries live V-time/entropy state but this VM \
                         has no V-time wired — restore into a VM composed like the snapshot source."
                            .to_string(),
                    ));
                }
                None
            }
        };
        Ok(RestorePreparation {
            engine_state,
            vcpu,
            clock_offset,
            vendor: prep,
            vtime: vtime_commit,
        })
    }

    pub(crate) fn preflight_restore_vm_state(
        &self,
        s: &<B::A as Vendor>::Snapshot,
    ) -> Result<(), VmmError> {
        self.prepare_restore_vm_state(s).map(|_| ())
    }

    pub fn restore_vm_state(&mut self, s: &<B::A as Vendor>::Snapshot) -> Result<(), VmmError> {
        if self.completion_staged {
            return Err(VmmError::ContractViolation(
                "restore_vm_state into a backend with a staged completion: the VM just serviced a \
                 read/MSR/CPUID exit whose completion is pending in kvm_run and is not \
                 cleared by restore — it would commit the old exit on the next run. Complete and \
                 retire the old exit first, or use a freshly-booted VM."
                    .to_string(),
            ));
        }
        let RestorePreparation {
            engine_state,
            vcpu,
            clock_offset,
            vendor,
            vtime,
        } = self.prepare_restore_vm_state(s)?;
        self.backend.restore(&vcpu)?;
        if let Some((cfg, clock, entropy)) = vtime {
            let vt = self.vtime.as_mut().expect("vtime_commit implies wired");
            vt.cfg = cfg;
            vt.clock = clock;
            vt.entropy = entropy;
            vt.guest_clock_offset = clock_offset;
        }
        <B::A as Vendor>::commit_restore(self, vendor);
        let restored_clockevent = <B::A as Vendor>::clockevent_trace_schedule(self);
        if let Some(trace) = self.virtual_time_trace.as_mut() {
            trace
                .restore_clockevent_schedule(restored_clockevent)
                .map_err(|message| {
                    VmmError::Backend(vmm_backend::BackendError::Internal(message))
                })?;
        }
        self.terminal = engine_state.terminal;
        self.saved_state = None;
        self.completion_staged = false;
        self.sdk_snapshot_reentry_required = engine_state.sdk_snapshot_reentry_required;
        Ok(())
    }

    pub fn restore_snapshot(
        &mut self,
        memory: &[u8],
        vm_state: &<B::A as Vendor>::Snapshot,
    ) -> Result<(), VmmError> {
        if memory.len() != self.ram.len() {
            return Err(VmmError::ContractViolation(format!(
                "restore_snapshot: image is {} bytes, guest RAM is {} bytes",
                memory.len(),
                self.ram.len()
            )));
        }
        self.restore_vm_state(vm_state)?;
        self.restore_guest_memory(memory)?;
        Ok(())
    }

    pub fn reseed_entropy(&mut self, seed: u64) -> Result<(), VmmError> {
        let Some(vt) = self.vtime.as_mut() else {
            return Err(VmmError::ContractViolation(
                "reseed_entropy: the seeded-entropy path is not wired, so there is no stream to fork for a branch."
                    .to_owned(),
            ));
        };

        vt.entropy = SeededEntropy::new(seed);
        Ok(())
    }

    pub fn step(&mut self) -> Result<Step, VmmError> {
        if self.pending_service_question().is_some() {
            return Ok(Step::SdkStop);
        }
        if let Some(reason) = self.terminal {
            return Ok(Step::Terminal(reason));
        }
        <B::A as Vendor>::service_pending_irqs(self)?;
        let mut exit = self.backend.run()?;
        self.sdk_snapshot_reentry_required = false;
        let mut stop = Step::Continued;
        let mut checkpoint_due = false;
        loop {
            let ExitProgress { step, continuation } =
                self.service_exit(exit, &mut checkpoint_due)?;
            if step != Step::Continued {
                stop = step;
            }
            match continuation {
                Some(next) => exit = next,
                None => return Ok(stop),
            }
        }
    }

    fn service_exit(
        &mut self,
        exit: Exit<B::A>,
        checkpoint_due: &mut bool,
    ) -> Result<ExitProgress<B::A>, VmmError> {
        if <B::A as Vendor>::is_doorbell_exit(&exit) {
            self.doorbell_exits = self.doorbell_exits.saturating_add(1);
        }
        let trace_started = if let Some(trace) = self.virtual_time_trace.as_mut() {
            if let Some((class, payload)) = <B::A as Vendor>::normalize_virtual_time_exit(&exit) {
                let reason = exit.reason();
                let backend_debug = format!("{reason:?}");
                trace
                    .begin(reason, backend_debug, class, payload)
                    .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
                true
            } else {
                let reason = exit.reason();
                let backend_debug = format!("{reason:?}");
                trace
                    .record_raw_only(reason, backend_debug)
                    .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
                false
            }
        } else {
            false
        };
        self.completion_staged = exit.stages_completion();
        <B::A as Vendor>::complete_irq_delivery(self);
        let step = match exit {
            Exit::Common(CommonExit::Idle) => self.on_idle(),
            Exit::Common(CommonExit::Shutdown) => Ok(self.terminate(TerminalReason::Shutdown)),
            Exit::Common(CommonExit::Mmio { gpa, size, write }) => {
                <B::A as Vendor>::dispatch_mmio(self, gpa, size, write)
            }
            Exit::Common(CommonExit::Hypercall(_)) => Err(VmmError::ContractViolation(
                "unmodeled hypercall-instruction exit (host handler is a later phase; the \
                 cooperating-guest channel rides the doorbell)"
                    .to_string(),
            )),
            Exit::Arch(e) => <B::A as Vendor>::dispatch_arch(self, e),
        }?;
        self.pvclock_refresh()?;
        <B::A as Vendor>::post_exit(self)?;
        let next = <B::A as Vendor>::finish_exit(self)?;
        if trace_started {
            let event_index = self
                .virtual_time_trace
                .as_ref()
                .expect("trace was started")
                .current_event_index()
                .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
            *checkpoint_due |= (event_index + 1).is_multiple_of(256);
            if *checkpoint_due && next.is_none() {
                self.virtual_time_checkpoint_events.insert(event_index);
            }
            let state_hash = synchronous_checkpoint_due(
                *checkpoint_due && next.is_none(),
                self.deferred_virtual_time_checkpoints,
            )
            .then(|| self.state_hash())
            .transpose()?;
            let vns_after = self
                .vtime
                .as_ref()
                .ok_or_else(|| {
                    VmmError::ContractViolation(
                        "virtual_time trace completed without V-time wiring".to_string(),
                    )
                })?
                .virtual_time_vns();
            self.virtual_time_trace
                .as_mut()
                .expect("trace was started")
                .finish(vns_after, state_hash)
                .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
        }
        Ok(ExitProgress {
            step,
            continuation: next,
        })
    }

    pub fn run(&mut self) -> Result<RunResult, VmmError> {
        let reason = loop {
            match self.step()? {
                Step::Terminal(r) => break r,
                Step::SdkStop => break TerminalReason::SdkStop,
                Step::Continued => {}
            }
        };
        let sdk_stop = if reason == TerminalReason::SdkStop {
            self.take_sdk_stop()
        } else {
            None
        };
        if reason == TerminalReason::SdkStop {
            self.saved_state = None;
        } else {
            self.saved_state = Some(self.backend.save()?);
        }
        Ok(RunResult {
            reason,
            sdk_stop,
            serial: <B::A as Vendor>::serial_capture(&self.devices).to_vec(),
            exit_counts: self.backend.exit_counts(),
        })
    }

    pub fn state_blob(&self) -> Result<Vec<u8>, VmmError> {
        let mut out = Vec::new();
        put_chunk(&mut out, b"MEM\0", self.ram.as_bytes());
        out.extend_from_slice(&self.state_blob_suffix()?);
        Ok(out)
    }

    pub(crate) fn state_blob_suffix(&self) -> Result<Vec<u8>, VmmError> {
        let mut out = Vec::new();
        let vcpu = match &self.saved_state {
            Some(state) => state.clone(),
            None => self.backend.save()?,
        };
        if let Some(db) = &self.doorbell_pages {
            put_chunk(&mut out, b"DOOR", db.as_bytes());
        }
        put_chunk(
            &mut out,
            b"VCPU",
            &<B::A as Vendor>::encode_vcpu_chunk(&vcpu),
        );
        put_chunk(
            &mut out,
            b"SERL",
            <B::A as Vendor>::serial_capture(&self.devices),
        );
        put_chunk(&mut out, b"DEV\0", &self.encode_device_terminal());
        if let Some(vt) = &self.vtime {
            put_chunk(&mut out, b"VTIM", &encode_vtime(vt));
        }
        <B::A as Vendor>::hash_device_chunks(&vcpu, &self.devices, &mut out);
        if let Some(sdk) = &self.sdk {
            put_chunk(&mut out, b"SDK\0", &encode_sdk_channel(sdk)?);
        }
        if let Some(pv) = &self.pvclock {
            let mut bytes = 1_u64.to_le_bytes().to_vec();
            match pv.gpa {
                Some(gpa) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&gpa.to_le_bytes());
                }
                None => bytes.push(0),
            }
            bytes.push(u8::from(self.pvclock_available()));
            bytes.push(u8::from(pv.armed));
            put_chunk(&mut out, b"PVCK", &bytes);
        }
        if self.sdk_snapshot_reentry_required {
            put_chunk(&mut out, b"SDRE", &[1]);
        }
        if self.snapshot_hashing {
            let snapshot = self.build_snapshot_state(&vcpu)?;
            let bytes = <<B::A as Vendor>::Snapshot as SnapshotRecords>::encode(&snapshot)
                .map_err(SnapshotError::from)?;
            put_chunk(&mut out, b"VMST", &bytes);
        }
        Ok(out)
    }

    fn encode_device_terminal(&self) -> Vec<u8> {
        let mut v = <B::A as Vendor>::encode_device_state(&self.devices);
        match self.terminal {
            None => v.push(0),
            Some(TerminalReason::DebugExit { code }) => {
                v.push(1);
                v.push(code);
            }
            Some(TerminalReason::Idle) => v.push(2),
            Some(TerminalReason::Shutdown) => v.push(3),
            Some(TerminalReason::SdkStop) => {
                unreachable!("SdkStop never latches as the VM terminal")
            }
        }
        v
    }

    pub fn state_hash(&self) -> Result<[u8; 32], VmmError> {
        let mut hasher = Sha256::new();
        hasher.update(b"MEM\0");
        hasher.update((self.ram.as_bytes().len() as u64).to_le_bytes());
        hasher.update(self.ram.as_bytes());
        hasher.update(self.state_blob_suffix()?);
        Ok(hasher.finalize().into())
    }

    pub fn state_components(&self) -> Vec<(&'static str, [u8; 32])> {
        fn dig(bytes: &[u8]) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(bytes);
            h.finalize().into()
        }
        let vcpu = self.current_vcpu();
        let mut out: Vec<(&'static str, [u8; 32])> = Vec::new();

        let ram = self.ram.as_bytes();
        let region = |lo: usize, hi: usize| {
            let (lo, hi) = (lo.min(ram.len()), hi.min(ram.len()));
            dig(&ram[lo..hi])
        };
        out.push(("RAM:0..64K", region(0, 0x1_0000)));
        out.push(("RAM:64K..1M", region(0x1_0000, 0x10_0000)));
        out.push(("RAM:1M..2M", region(0x10_0000, 0x20_0000)));
        out.push(("RAM:2M..16M", region(0x20_0000, 0x100_0000)));
        out.push(("RAM:16M..", region(0x100_0000, ram.len())));
        if let Some(db) = &self.doorbell_pages {
            out.push(("doorbell", dig(db.as_bytes())));
        }

        <B::A as Vendor>::vcpu_components(&vcpu, &mut out);

        out.push((
            "serial",
            dig(<B::A as Vendor>::serial_capture(&self.devices)),
        ));
        out.push(("dev", dig(&self.encode_device_terminal())));
        <B::A as Vendor>::device_components(&vcpu, &self.devices, &mut out);
        if let Some(vt) = &self.vtime {
            let mut cfg = 1_u64.to_le_bytes().to_vec();
            for x in [vt.cfg.guest_hz, vt.cfg.guest_base, vt.guest_clock_offset] {
                cfg.extend_from_slice(&x.to_le_bytes());
            }
            out.push(("vtim:cfg", dig(&cfg)));
            out.push(("vtim:eff-vns", dig(&vt.clock.vns().to_le_bytes())));
            out.push(("vtim:entropy", dig(&vt.entropy.save_state())));
        }
        out
    }

    pub fn report_stream(&self) -> &[u32] {
        &self.report_stream
    }

    pub fn idle_landings(&self) -> &[u64] {
        &self.idle_landings
    }

    pub fn serial(&self) -> &[u8] {
        <B::A as Vendor>::serial_capture(&self.devices)
    }

    pub fn exit_counts(&self) -> vmm_backend::ExitCounts {
        self.backend.exit_counts()
    }

    pub fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.backend.cancellation_flag()
    }

    pub fn doorbell_exits(&self) -> u64 {
        self.doorbell_exits
    }

    pub fn terminal_reason(&self) -> Option<TerminalReason> {
        self.terminal
    }

    pub fn observable_digest(&self) -> [u8; 32] {
        let report_stream = &self.report_stream;
        let serial = <B::A as Vendor>::serial_capture(&self.devices);
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"OBSV");
        hasher.update((report_stream.len() as u64).to_le_bytes());
        for v in report_stream {
            hasher.update(v.to_le_bytes());
        }
        hasher.update((serial.len() as u64).to_le_bytes());
        hasher.update(serial);
        hasher.finalize().into()
    }

    pub(crate) fn terminate(&mut self, reason: TerminalReason) -> Step {
        self.terminal = Some(reason);
        Step::Terminal(reason)
    }

    pub fn enable_sdk(
        &mut self,
        env: channel::RecordedEnv<Box<dyn channel::ServiceHandler>>,
        _config: &ServiceConfig,
    ) -> &mut Self {
        self.sdk = Some(SdkChannel {
            env,
            events: Vec::new(),
            coverage_thresholds: BTreeMap::new(),
            coverage: Vec::new(),
            pending_stop: None,
            pending_snapshot: false,
        });
        self
    }

    #[cfg(test)]
    pub(crate) fn sdk_is_enabled(&self) -> bool {
        self.sdk.is_some()
    }

    pub fn enable_pvclock(&mut self) -> &mut Self {
        self.pvclock = Some(PvclockChannel {
            gpa: None,
            armed: false,
            refreshes: Vec::new(),
        });
        self
    }

    pub fn pvclock_offered(&self) -> bool {
        self.pvclock.is_some()
    }

    fn pvclock_available(&self) -> bool {
        self.pvclock.is_some() && self.vtime.is_some()
    }

    fn doorbell_service_offered(&self, service: u16) -> bool {
        match service {
            s if s == ServiceId::Event as u16 => self.sdk.is_some(),
            s if s == ServiceId::Sdk as u16 => self.sdk.is_some(),
            s if s == ServiceId::Entropy as u16 => self.sdk.is_some(),
            s if s == ServiceId::Payload as u16 => self
                .sdk
                .as_ref()
                .is_some_and(|sdk| sdk.env.payload_configured()),
            s if s == ServiceId::Pvclock as u16 => self.pvclock_available(),
            _ => false,
        }
    }

    pub fn pvclock_registration(&self) -> Option<u64> {
        self.pvclock.as_ref().and_then(|pv| pv.gpa)
    }

    pub fn pvclock_snapshot(&self) -> Option<PvclockSnapshot> {
        let registrable = self.pvclock_available();
        self.pvclock.as_ref().map(|pv| PvclockSnapshot {
            gpa: pv.gpa,
            registrable,
            armed: pv.armed,
        })
    }

    pub(crate) fn pvclock_validate_restore(
        &self,
        rec: Option<&PvclockSnapshot>,
    ) -> Result<(), VmmError> {
        match (rec, self.pvclock.as_ref()) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(VmmError::ContractViolation(
                "restore_vm_state: this VM offers the pvclock page but the snapshot's VM did \
                 not — a guest registering here would fork the timeline off the sealed one; \
                 restore into a VM composed like the snapshot source."
                    .to_string(),
            )),
            (Some(snapshot), None) => Err(VmmError::ContractViolation(format!(
                "restore_vm_state: snapshot carries a pvclock channel (registration \
                 {:#x?}) but this VM was composed without enable_pvclock — \
                 restore into a VM composed like the snapshot source.",
                snapshot.gpa
            ))),
            (Some(snapshot), Some(_pv)) => {
                if snapshot.armed && snapshot.gpa.is_none() {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: pvclock record is armed without a registered page"
                            .to_string(),
                    ));
                }
                if snapshot.gpa.is_some() && !snapshot.registrable {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: pvclock record has a registration but is marked non-registrable"
                            .to_string(),
                    ));
                }
                if snapshot.registrable != self.pvclock_available() {
                    return Err(VmmError::ContractViolation(format!(
                        "restore_vm_state: pvclock registration capability mismatch (the \
                         snapshot's VM {} register a clock page; this VM {}) — the restored \
                         guest's next registration would take a different branch than the \
                         sealed timeline's. Restore into a VM composed like the snapshot \
                         source (V-time wired, deterministic virtual-time clock).",
                        if snapshot.registrable {
                            "could"
                        } else {
                            "could NOT"
                        },
                        if self.pvclock_available() {
                            "can"
                        } else {
                            "can NOT"
                        }
                    )));
                }
                if let Some(gpa) = snapshot.gpa {
                    self.pvclock_validate_gpa(gpa).map_err(|reason| {
                        VmmError::ContractViolation(format!(
                            "restore_vm_state: snapshot pvclock page GPA {gpa:#x} does not \
                             validate on this VM ({reason}) — restore into a VM composed like \
                             the snapshot source."
                        ))
                    })?;
                }
                Ok(())
            }
        }
    }

    pub(crate) fn pvclock_commit_restore(&mut self, rec: Option<&PvclockSnapshot>) {
        if let Some(pv) = self.pvclock.as_mut() {
            let (gpa, armed) = rec.map_or((None, false), |snapshot| (snapshot.gpa, snapshot.armed));
            pv.armed = armed;
            pv.gpa = gpa;
            pv.refreshes.clear();
        }
    }

    pub fn pvclock_clear_refreshes(&mut self) {
        if let Some(pv) = self.pvclock.as_mut() {
            pv.refreshes.clear();
        }
    }

    pub fn pvclock_refreshes(&self) -> &[(u64, u64)] {
        self.pvclock
            .as_ref()
            .map(|pv| pv.refreshes.as_slice())
            .unwrap_or(&[])
    }

    pub fn pvclock_page(&self) -> Option<&[u8]> {
        let off = self.ram_offset_of(self.pvclock_registration()?)?;
        self.ram
            .as_bytes()
            .get(off..off + vtime::pvclock::PVCLOCK_PAGE_LEN)
    }

    pub fn pvclock_check_oracle(&self) -> Result<(), VmmError> {
        let Some(page) = self.pvclock_page() else {
            return Ok(());
        };
        let vt = self.vtime.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "pvclock page registered but V-time is not wired — registration is gated on the \
                 determinism path, so this is unreachable state"
                    .to_string(),
            )
        })?;
        let want_vns = vt.clock.vns();
        let want_gc = vt.guest_clock();
        let want_hz = vt.cfg.guest_hz;
        let Some(f) = vtime::pvclock::read(page) else {
            return Err(VmmError::ContractViolation(
                "pvclock page is not a stable ABI-v1 frame (odd seq or foreign abi_version) at a \
                 host-quiescent read — the stamp protocol never leaves the page mid-update"
                    .to_string(),
            ));
        };
        if (f.vns, f.guest_clock, f.guest_clock_hz) != (want_vns, want_gc, want_hz) {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page diverges from the RDTSC-trap oracle: page \
                 (vns {}, guest_clock {}, hz {}) vs oracle (vns {want_vns}, guest_clock \
                 {want_gc}, hz {want_hz})",
                f.vns, f.guest_clock, f.guest_clock_hz
            )));
        }
        if f.flags != vtime::pvclock::PVCLOCK_FLAGS_V1 {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page flags {:#x} != the ABI-v1 MATERIALIZED|EXIT_COUNT_DERIVED word {:#x} — a \
                 placeholder or corrupted page, never a real exit-count-derived stamp",
                f.flags,
                vtime::pvclock::PVCLOCK_FLAGS_V1
            )));
        }
        Ok(())
    }

    fn pvclock_validate_gpa(&self, gpa: u64) -> Result<(), &'static str> {
        let page_len = vtime::pvclock::PVCLOCK_PAGE_LEN as u64;
        if !gpa.is_multiple_of(page_len) {
            return Err("not page-aligned");
        }
        let off = self.ram_offset_of(gpa).ok_or("below the guest RAM base")? as u64;
        let end = off.checked_add(page_len).ok_or("address overflow")?;
        if end > self.ram.as_bytes().len() as u64 {
            return Err("past the end of guest RAM");
        }
        if gpa == REQ_GPA as u64 || gpa == RESP_GPA as u64 {
            return Err("overlaps a doorbell frame page");
        }
        for &(hole, hole_len) in <B::A as Vendor>::mmio_holes() {
            let hole_end = hole.saturating_add(hole_len);
            if gpa < hole_end && hole < end {
                return Err("overlaps a device-MMIO hole (not backed as guest RAM)");
            }
        }
        Ok(())
    }

    pub(crate) fn pvclock_register(&mut self, gpa: u64) -> (Status, Option<u32>) {
        if !self.pvclock_available() {
            return (Status::UnknownService, None);
        }
        if self.pvclock_registration().is_some() {
            return (Status::BadRequest, None);
        }
        if self.pvclock_validate_gpa(gpa).is_err() {
            return (Status::OutOfRange, None);
        }
        let pv = self.pvclock.as_mut().expect("checked above");
        pv.gpa = Some(gpa);
        (Status::Ok, Some(vtime::pvclock::PVCLOCK_ABI_VERSION))
    }

    fn pvclock_stamp(&mut self, kind: StampKind) -> Result<(), VmmError> {
        let Some(pv) = self.pvclock.as_ref() else {
            return Ok(());
        };
        let Some(gpa) = pv.gpa else {
            return Ok(());
        };
        let Some(vt) = self.vtime.as_ref() else {
            return Err(VmmError::ContractViolation(
                "pvclock page registered but V-time is not wired".to_string(),
            ));
        };
        let vns = vt.clock.vns();
        let gc = vt.guest_clock();
        let hz = vt.cfg.guest_hz;
        let off = self.ram_offset_of(gpa);
        let ram = self.ram.as_mut_bytes();
        let Some(page) = off.and_then(|o| ram.get_mut(o..o + vtime::pvclock::PVCLOCK_PAGE_LEN))
        else {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page {gpa:#x} no longer inside guest RAM — registration validated it, so \
                 the RAM backing changed underneath the channel"
            )));
        };
        let changed = match kind {
            StampKind::Refresh => vtime::pvclock::stamp(page, vns, gc, hz),
            StampKind::Canonical => vtime::pvclock::stamp_canonical(page, vns, gc, hz),
        };
        if !changed {
            return Ok(());
        }
        let readback = vtime::pvclock::read(page);
        if readback.map(|f| (f.vns, f.guest_clock, f.guest_clock_hz)) != Some((vns, gc, hz)) {
            return Err(VmmError::ContractViolation(format!(
                "pvclock stamp read-back mismatch: wrote (vns {vns}, \
                 guest_clock {gc}, hz {hz}) but the page decodes to {readback:?}"
            )));
        }
        self.mark_host_dirty(gpa, vtime::pvclock::PVCLOCK_PAGE_LEN as u64);
        if kind == StampKind::Refresh {
            let pv = self.pvclock.as_mut().expect("checked above");
            if pv.refreshes.len() < EVENT_TRACE_CAP {
                pv.refreshes.push((vns, gc));
            }
        }
        Ok(())
    }

    fn pvclock_refresh(&mut self) -> Result<(), VmmError> {
        let Some(pv) = self.pvclock.as_ref() else {
            return Ok(());
        };
        if pv.gpa.is_none() {
            return Ok(());
        }
        if !pv.armed {
            self.pvclock.as_mut().expect("checked above").armed = true;
            return self.pvclock_stamp(StampKind::Canonical);
        }
        self.pvclock_stamp(StampKind::Refresh)
    }

    pub fn sdk_snapshot(&self) -> Result<Option<SdkSnapshot>, channel::ChannelError> {
        self.sdk
            .as_ref()
            .map(|s| {
                Ok(SdkSnapshot {
                    recorded: s.env.snapshot_state()?,
                    events: s.events.clone(),
                    pending_snapshot: s.pending_snapshot,
                    pending_stop: s.pending_stop.clone(),
                    coverage_thresholds: s.coverage_thresholds.clone(),
                })
            })
            .transpose()
    }

    pub fn sdk_restore(&mut self, snap: &SdkSnapshot) -> Result<(), channel::ChannelError> {
        if let Some(s) = self.sdk.as_mut() {
            snap.recorded.restore_into(&mut s.env)?;
            s.events = snap.events.clone();
            s.pending_snapshot = snap.pending_snapshot;
            s.pending_stop = snap.pending_stop.clone();
            s.coverage_thresholds = snap.coverage_thresholds.clone();
        }
        Ok(())
    }

    pub fn sdk_restore_events(&mut self, snap: &SdkSnapshot) {
        if let Some(s) = self.sdk.as_mut() {
            s.events = snap.events.clone();
            s.pending_snapshot = snap.pending_snapshot;
            s.pending_stop = snap.pending_stop.clone();
            s.coverage_thresholds = snap.coverage_thresholds.clone();
        }
    }

    pub fn sdk_events(&self) -> &[(u64, u32, Vec<u8>)] {
        self.sdk
            .as_ref()
            .map(|s| s.events.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn sdk_remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.sdk
            .as_ref()
            .and_then(|sdk| sdk.env.remaining_payloads())
    }

    pub fn take_sdk_stop(&mut self) -> Option<SdkStop> {
        self.sdk.as_mut().and_then(|s| {
            if matches!(s.pending_stop, Some(SdkStop::Decision { .. })) {
                s.pending_stop.clone()
            } else {
                s.pending_stop.take()
            }
        })
    }

    pub fn pending_service_question(&self) -> Option<(u64, &channel::Question)> {
        match self.sdk.as_ref()?.pending_stop.as_ref()? {
            SdkStop::Decision {
                moment, question, ..
            } => Some((*moment, question)),
            _ => None,
        }
    }

    pub fn resolve_service_answer(
        &mut self,
        answer: channel::Answer,
    ) -> Result<(u64, channel::Question), VmmError> {
        let Some(SdkStop::Decision {
            moment,
            seq,
            question,
        }) = self.sdk.as_ref().and_then(|sdk| sdk.pending_stop.clone())
        else {
            return Err(VmmError::ContractViolation(
                "no service request is pending".into(),
            ));
        };
        let payload = service_answer_bytes(&answer).ok_or_else(|| {
            VmmError::ContractViolation("service answer exceeds the doorbell frame".into())
        })?;
        let mut response = [0; HC_PAGE];
        let len = encode_response(ServiceId::Sdk, 3, seq, Status::Ok, &payload, &mut response)
            .map_err(|_| VmmError::ContractViolation("service answer cannot be encoded".into()))?;
        self.write_doorbell_response(&response[..len])?;
        let sdk = self
            .sdk
            .as_mut()
            .expect("pending request requires SDK channel");
        sdk.env.record_question(moment, &question, answer);
        sdk.pending_stop = None;
        Ok((moment, question))
    }

    pub fn sdk_coverage(&self) -> &[(u64, u32, u64, u32, u32)] {
        self.sdk
            .as_ref()
            .map(|s| s.coverage.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn service_doorbell(&mut self, req_len: u32) -> Result<Step, VmmError> {
        if req_len as usize > HC_PAGE {
            let mut resp = [0_u8; HC_PAGE];
            let n = encode_response(ServiceId::Event, 1, 0, Status::BadRequest, &[], &mut resp)
                .unwrap_or(0);
            self.write_doorbell_response(&resp[..n])?;
            return Ok(Step::Continued);
        }
        let req_len = req_len as usize;
        let Some(req) = self
            .guest_slice(REQ_GPA as u64, req_len)
            .map(<[u8]>::to_vec)
        else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell request page {REQ_GPA:#x}+{req_len} is not backed — x86 keeps the \
                 transport ABI pages in the GPA-0 RAM; an arm64 boot must map them (a dedicated \
                 low-GPA memslot; RAM is high)"
            )));
        };
        let moment = self.effective_vns().unwrap_or(0);
        let mut resp = [0_u8; HC_PAGE];
        let (resp_len, stop) = self.dispatch_doorbell(moment, &req, &mut resp);
        self.write_doorbell_response(&resp[..resp_len])?;
        match stop {
            Some(s) => {
                if let Some(sdk) = self.sdk.as_mut() {
                    sdk.pending_stop = Some(s);
                }
                Ok(Step::SdkStop)
            }
            None => Ok(Step::Continued),
        }
    }

    pub(crate) fn ram_offset_of(&self, gpa: u64) -> Option<usize> {
        usize::try_from(gpa.checked_sub(self.ram_base_gpa)?).ok()
    }

    pub(crate) fn guest_slice(&self, gpa: u64, len: usize) -> Option<&[u8]> {
        if let Some(off) = self.ram_offset_of(gpa) {
            return self.ram.as_bytes().get(off..off.checked_add(len)?);
        }
        let db = self.doorbell_pages.as_ref()?;
        let off = usize::try_from(gpa.checked_sub(DOORBELL_MAP_GPA as u64)?).ok()?;
        db.as_bytes().get(off..off.checked_add(len)?)
    }

    pub(crate) fn guest_slice_mut(&mut self, gpa: u64, len: usize) -> Option<&mut [u8]> {
        if let Some(off) = self.ram_offset_of(gpa) {
            let end = off.checked_add(len)?;
            return self.ram.as_mut_bytes().get_mut(off..end);
        }
        let db = self.doorbell_pages.as_mut()?;
        let off = usize::try_from(gpa.checked_sub(DOORBELL_MAP_GPA as u64)?).ok()?;
        let end = off.checked_add(len)?;
        db.as_mut_bytes().get_mut(off..end)
    }

    fn write_doorbell_response(&mut self, resp: &[u8]) -> Result<(), VmmError> {
        let Some(dst) = self.guest_slice_mut(RESP_GPA as u64, resp.len()) else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell response page {RESP_GPA:#x}+{} is not backed",
                resp.len()
            )));
        };
        dst.copy_from_slice(resp);
        self.mark_host_dirty(RESP_GPA as u64, resp.len() as u64);
        Ok(())
    }

    fn dispatch_doorbell(
        &mut self,
        moment: u64,
        req: &[u8],
        resp: &mut [u8],
    ) -> (usize, Option<SdkStop>) {
        let Ok((header, payload)) = decode(req) else {
            let n =
                encode_response(ServiceId::Event, 1, 0, Status::BadRequest, &[], resp).unwrap_or(0);
            return (n, None);
        };
        if !header.is_request() {
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::BadRequest,
                resp,
            );
            return (n, None);
        }
        if header.service == ServiceId::Pvclock as u16 {
            if !self.pvclock_available() {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownService,
                    resp,
                );
                return (n, None);
            }
            if header.opcode != 1 {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownOpcode,
                    resp,
                );
                return (n, None);
            }
            if payload.len() != 8 {
                let n = encode_response(
                    ServiceId::Pvclock,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let mut gpa_bytes = [0_u8; 8];
            gpa_bytes.copy_from_slice(payload);
            let (status, abi) = self.pvclock_register(u64::from_le_bytes(gpa_bytes));
            let body = abi.map(u32::to_le_bytes);
            let n = encode_response(
                ServiceId::Pvclock,
                1,
                header.seq,
                status,
                body.as_ref().map(<[u8; 4]>::as_slice).unwrap_or(&[]),
                resp,
            )
            .unwrap_or(0);
            return (n, None);
        }
        if header.service == ServiceId::Payload as u16 {
            if !self.doorbell_service_offered(header.service) {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownService,
                    resp,
                );
                return (n, None);
            }
            if header.opcode != 1 {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownOpcode,
                    resp,
                );
                return (n, None);
            }
            if payload.len() != 4 {
                let n = encode_response(
                    ServiceId::Payload,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let bytes = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            if bytes == 0 || bytes as usize > MAX_PAYLOAD {
                let n = encode_response(
                    ServiceId::Payload,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let pulled = self
                .sdk
                .as_mut()
                .expect("payload availability requires SDK")
                .env
                .pull_payload(bytes as usize);
            return match pulled {
                Ok(Some(entry)) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::Ok,
                        &entry,
                        resp,
                    )
                    .unwrap_or(0);
                    (n, None)
                }
                Ok(None) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::OutOfRange,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    (n, Some(SdkStop::Quiescent))
                }
                Err(_) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::BadRequest,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    (n, None)
                }
            };
        }
        if header.service == ServiceId::Event as u16 && header.opcode == 1 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Event,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            if payload.len() < 4 {
                let n = encode_response(
                    ServiceId::Event,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let data = &payload[4..];
            let (stop, defer) = match Self::classify_sdk_event(id, data) {
                SdkEventAction::Malformed => {
                    let n = encode_response(
                        ServiceId::Event,
                        1,
                        header.seq,
                        Status::BadRequest,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    return (n, None);
                }
                SdkEventAction::Stop(s) => (Some(s), false),
                SdkEventAction::DeferSnapshot => (None, true),
                SdkEventAction::Capture => (None, false),
            };
            if let Some(sdk) = self.sdk.as_mut() {
                sdk.events.push((moment, id, data.to_vec()));
                if defer {
                    sdk.pending_snapshot = true;
                }
            }
            if defer {
                self.sdk_snapshot_reentry_required = true;
            }
            let n = encode_response(ServiceId::Event, 1, header.seq, Status::Ok, &[], resp)
                .unwrap_or(0);
            return (n, stop);
        }
        if header.service == ServiceId::Sdk as u16 && header.opcode == 3 {
            let Some(sdk) = self.sdk.as_mut() else {
                return (
                    encode_error(
                        header.service,
                        header.opcode,
                        header.seq,
                        Status::UnknownService,
                        resp,
                    ),
                    None,
                );
            };
            if payload.len() < 10 {
                return (
                    encode_error(
                        header.service,
                        header.opcode,
                        header.seq,
                        Status::BadRequest,
                        resp,
                    ),
                    None,
                );
            }
            let service = u16::from_le_bytes([payload[0], payload[1]]);
            if service <= channel::SERVICE_SCHEDULER {
                return (
                    encode_error(
                        header.service,
                        header.opcode,
                        header.seq,
                        Status::BadRequest,
                        resp,
                    ),
                    None,
                );
            }
            let request_id =
                u64::from_le_bytes(payload[2..10].try_into().expect("validated request prefix"));
            let question =
                channel::Question::with_request_id(request_id, service, payload[10..].to_vec())
                    .expect("one doorbell frame fits channel bounds");
            let mut candidate = sdk.env.clone();
            candidate.set_moment(moment);
            match candidate.decide(&question) {
                Ok(channel::ServiceResponse::Answered(answer)) => {
                    let Some(bytes) = service_answer_bytes(&answer) else {
                        return (
                            encode_error(
                                header.service,
                                header.opcode,
                                header.seq,
                                Status::Internal,
                                resp,
                            ),
                            None,
                        );
                    };
                    sdk.env = candidate;
                    return (
                        encode_response(ServiceId::Sdk, 3, header.seq, Status::Ok, &bytes, resp)
                            .unwrap_or(0),
                        None,
                    );
                }
                Ok(channel::ServiceResponse::External) => {
                    sdk.env = candidate;
                    return (
                        0,
                        Some(SdkStop::Decision {
                            moment,
                            seq: header.seq,
                            question,
                        }),
                    );
                }
                Err(_) => {
                    return (
                        encode_error(
                            header.service,
                            header.opcode,
                            header.seq,
                            Status::Internal,
                            resp,
                        ),
                        None,
                    );
                }
            }
        }
        if header.service == ServiceId::Sdk as u16 && header.opcode == 2 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Sdk,
                    2,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            if payload.len() != SDK_COVERAGE_REQUEST_LEN {
                let n =
                    encode_response(ServiceId::Sdk, 2, header.seq, Status::BadRequest, &[], resp)
                        .unwrap_or(0);
                return (n, None);
            }
            let thread = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let observed = u64::from_le_bytes([
                payload[4],
                payload[5],
                payload[6],
                payload[7],
                payload[8],
                payload[9],
                payload[10],
                payload[11],
            ]);
            let ready = u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
            match self.decide_coverage(moment, thread, observed, ready) {
                Ok((next, selected)) => {
                    let mut answer = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
                    answer[0..8].copy_from_slice(&next.to_le_bytes());
                    answer[8..12].copy_from_slice(&selected.to_le_bytes());
                    let n =
                        encode_response(ServiceId::Sdk, 2, header.seq, Status::Ok, &answer, resp)
                            .unwrap_or(0);
                    return (n, None);
                }
                Err(status) => {
                    let n = encode_response(ServiceId::Sdk, 2, header.seq, status, &[], resp)
                        .unwrap_or(0);
                    return (n, None);
                }
            }
        }
        if header.service == ServiceId::Entropy as u16 && header.opcode == 1 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Entropy,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let mut buf = [0_u8; MAX_PAYLOAD];
            let (status, got) = match self.vtime.as_mut() {
                Some(vt) => vt.draw_entropy(payload, &mut buf),
                None => (Status::BadRequest, 0),
            };
            let m = encode_response(ServiceId::Entropy, 1, header.seq, status, &buf[..got], resp)
                .unwrap_or(0);
            return (m, None);
        }
        let known = matches!(
            header.service,
            s if s == ServiceId::Event as u16
                || s == ServiceId::Sdk as u16
                || s == ServiceId::Entropy as u16
                || s == ServiceId::Pvclock as u16
                || s == ServiceId::Payload as u16
        );
        if known && self.doorbell_service_offered(header.service) {
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                resp,
            );
            (n, None)
        } else {
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::UnknownService,
                resp,
            );
            (n, None)
        }
    }

    fn classify_sdk_event(id: u32, data: &[u8]) -> SdkEventAction {
        let ns = (id >> SDK_NS_SHIFT) as u8;
        let local = id & SDK_LOCAL_MASK;
        match ns {
            SDK_NS_ASSERT if data.first() == Some(&SDK_DISP_VIOLATION) => {
                let Some(len_bytes) = data.get(1..3) else {
                    return SdkEventAction::Malformed;
                };
                let dl = u16::from_le_bytes([len_bytes[0], len_bytes[1]]) as usize;
                match data.get(3..) {
                    Some(detail) if detail.len() == dl => {
                        SdkEventAction::Stop(SdkStop::Assertion {
                            id: local,
                            data: detail.to_vec(),
                        })
                    }
                    _ => SdkEventAction::Malformed,
                }
            }
            SDK_NS_LIFECYCLE if local == 0 => {
                if data.is_empty() {
                    SdkEventAction::DeferSnapshot
                } else {
                    SdkEventAction::Malformed
                }
            }
            SDK_NS_LIFECYCLE if local == 1 => {
                if data.len() == 8 {
                    SdkEventAction::DeferSnapshot
                } else {
                    SdkEventAction::Malformed
                }
            }
            _ => SdkEventAction::Capture,
        }
    }

    pub fn take_snapshot_point(&mut self) -> bool {
        if !self.sdk_snapshot_reentry_required
            && let Some(sdk) = self.sdk.as_mut()
            && sdk.pending_stop.is_none()
            && sdk.pending_snapshot
        {
            sdk.pending_snapshot = false;
            return true;
        }
        false
    }

    fn decide_coverage(
        &mut self,
        moment: u64,
        thread: u32,
        observed: u64,
        ready: u32,
    ) -> Result<(u64, u32), Status> {
        let Some(sdk) = self.sdk.as_ref() else {
            return Err(Status::UnknownService);
        };
        let expected = sdk
            .coverage_thresholds
            .get(&thread)
            .copied()
            .unwrap_or(SDK_COVERAGE_QUANTUM);
        if ready == 0 || observed != expected {
            return Err(Status::BadRequest);
        }
        let next = observed
            .checked_add(SDK_COVERAGE_QUANTUM)
            .ok_or(Status::OutOfRange)?;
        let mut env = sdk.env.clone();
        env.set_moment(moment);
        let question = channel::Question::with_request_id(
            u64::from(thread),
            channel::SERVICE_SCHEDULER,
            ready.to_le_bytes().to_vec(),
        )
        .map_err(|_| Status::BadRequest)?;
        let channel::ServiceResponse::Answered(channel::Answer::Data(bytes)) =
            env.decide(&question).map_err(|_| Status::Internal)?
        else {
            return Err(Status::Internal);
        };
        let selected_bytes: [u8; 4] = bytes.as_slice().try_into().map_err(|_| Status::Internal)?;
        let selected = u32::from_le_bytes(selected_bytes);
        if selected >= ready {
            return Err(Status::Internal);
        }
        let sdk = self.sdk.as_mut().expect("checked above");
        sdk.env = env;
        sdk.coverage_thresholds.insert(thread, next);
        sdk.coverage
            .push((moment, thread, observed, ready, selected));
        Ok((next, selected))
    }

    pub(crate) fn now_vns(&self) -> Result<u64, VmmError> {
        match &self.vtime {
            Some(vt) => Ok(vt.clock.vns()),
            None => Ok(0),
        }
    }

    pub(crate) fn set_idle_wake_vns(&mut self, vns: Option<u64>) {
        self.idle_wake_vns = vns;
    }

    pub fn entropy_state(&self) -> Option<u64> {
        self.vtime.as_ref().map(|vt| {
            let bytes = vt.entropy.save_state();
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[..8]);
            u64::from_le_bytes(buf)
        })
    }

    pub fn apply_effect(&mut self, effect: &channel::Effect) -> Result<(), VmmError> {
        match effect {
            channel::Effect::WriteMemory { gpa, bytes } => {
                let Some(dst) = self.guest_slice_mut(*gpa, bytes.len()) else {
                    return Err(VmmError::ContractViolation(format!(
                        "WriteMemory gpa {gpa:#x}+{} is not backed by guest RAM",
                        bytes.len()
                    )));
                };
                dst.copy_from_slice(bytes);
                self.mark_host_dirty(*gpa, bytes.len() as u64);
                Ok(())
            }
            channel::Effect::XorMemory { gpa, bytes } => {
                let Some(dst) = self.guest_slice_mut(*gpa, bytes.len()) else {
                    return Err(VmmError::ContractViolation(format!(
                        "XorMemory gpa {gpa:#x}+{} is not backed by guest RAM",
                        bytes.len()
                    )));
                };
                for (current, mask) in dst.iter_mut().zip(bytes) {
                    *current ^= mask;
                }
                self.mark_host_dirty(*gpa, bytes.len() as u64);
                Ok(())
            }
            channel::Effect::InjectInterrupt { vector } => {
                <B::A as Vendor>::inject_wire_interrupt(self, *vector)
            }
        }
    }

    pub(crate) fn mark_host_dirty(&mut self, gpa: u64, len: u64) {
        if len == 0 {
            return;
        }
        let first = gpa / 4096;
        let last = (gpa + len - 1) / 4096;
        self.host_dirty.extend(first..=last);
    }

    pub fn drain_dirty_pages(&mut self) -> Option<Vec<u64>> {
        if self.host_dirty_wholesale {
            return None;
        }
        let mut gfns = self.backend.drain_dirty_pages().ok()?;
        gfns.extend(self.host_dirty.iter().copied());
        self.host_dirty.clear();
        let base = self.ram_base_gpa / 4096;
        let pages = (self.ram.len() / 4096) as u64;
        let mut rel: Vec<u64> = gfns
            .into_iter()
            .filter_map(|g| g.checked_sub(base).filter(|&r| r < pages))
            .collect();
        rel.sort_unstable();
        rel.dedup();
        Some(rel)
    }

    pub fn reset_dirty_tracking(&mut self) -> bool {
        self.host_dirty.clear();
        self.host_dirty_wholesale = false;
        self.backend.drain_dirty_pages().is_ok()
    }

    pub(crate) fn on_idle(&mut self) -> Result<Step, VmmError> {
        match self.idle_action()? {
            IdleAction::DeliverPending => Ok(Step::Continued),
            IdleAction::JumpToDeadline(deadline_vns) => self.resume_idle(deadline_vns),
            IdleAction::Terminal => Ok(self.terminate(TerminalReason::Idle)),
        }
    }

    fn idle_action(&mut self) -> Result<IdleAction, VmmError> {
        let Some(_vt) = self.vtime.as_ref() else {
            return Ok(IdleAction::Terminal);
        };
        if !<B::A as Vendor>::guest_interruptible(self)? {
            return Ok(IdleAction::Terminal);
        }
        if <B::A as Vendor>::pending_deliverable_interrupt(self)? {
            return Ok(IdleAction::DeliverPending);
        }
        let timer = <B::A as Vendor>::deliverable_timer_deadline_vns(self);
        let wake = match (timer, self.idle_wake_vns) {
            (Some(timer), Some(host)) => Some(timer.min(host)),
            (Some(timer), None) => Some(timer),
            (None, host) => host,
        };
        match wake {
            Some(vns) => Ok(IdleAction::JumpToDeadline(vns)),
            None => Ok(IdleAction::Terminal),
        }
    }

    pub(crate) fn resume_idle(&mut self, deadline_vns: u64) -> Result<Step, VmmError> {
        let (landing, snap) = {
            let vt = self
                .vtime
                .as_ref()
                .expect("JumpToDeadline implies V-time wired");
            let now_vns = vt.clock.vns();
            let landing = IdlePlanner::new().plan(now_vns, deadline_vns).landed_vns;
            let snap = VtimeSnapshot {
                vns: landing,
                guest_clock_offset: vt.guest_clock_offset,
                entropy: vt.entropy.save_state(),
            };
            (landing, snap)
        };
        self.restore_vtime(&snap)?;
        if self.idle_landings.len() < EVENT_TRACE_CAP {
            self.idle_landings.push(landing);
        }
        Ok(Step::Continued)
    }

    pub(crate) fn current_vns(&self) -> Option<u64> {
        self.vtime.as_ref().map(VtimeWiring::virtual_time_vns)
    }

    pub(crate) fn current_vcpu(&self) -> VcpuOf<B> {
        match &self.saved_state {
            Some(s) => s.clone(),
            None => self.backend.save().unwrap_or_default(),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
impl Vmm<vmm_backend::HvfBackend> {
    pub fn hvf_exit_handle(&self) -> vmm_backend::HvfExitHandle {
        self.backend.exit_handle()
    }
}

pub(crate) fn put_chunk(out: &mut Vec<u8>, tag: &[u8; 4], bytes: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn encode_vtime(vt: &VtimeWiring) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(1);
    v.extend_from_slice(&1_u64.to_le_bytes());
    for x in [vt.cfg.guest_hz, vt.cfg.guest_base, vt.guest_clock_offset] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&vt.clock.vns().to_le_bytes());
    v.extend_from_slice(&vt.entropy.save_state());
    v
}

fn encode_sdk_channel(sdk: &SdkChannel) -> Result<Vec<u8>, channel::ChannelError> {
    let mut v = Vec::new();
    let recorded = sdk.env.snapshot_state()?.encode();
    v.extend_from_slice(&(recorded.len() as u64).to_le_bytes());
    v.extend_from_slice(&recorded);
    match &sdk.pending_stop {
        None => v.push(0),
        Some(SdkStop::Assertion { id, data }) => {
            v.push(1);
            v.extend_from_slice(&id.to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(data);
        }
        Some(SdkStop::Quiescent) => v.push(2),
        Some(SdkStop::Decision {
            moment,
            seq,
            question,
        }) => {
            v.push(3);
            v.extend_from_slice(&moment.to_le_bytes());
            v.extend_from_slice(&seq.to_le_bytes());
            v.extend_from_slice(&question.service().to_le_bytes());
            v.extend_from_slice(&question.request_id().to_le_bytes());
            v.extend_from_slice(&(question.payload().len() as u32).to_le_bytes());
            v.extend_from_slice(question.payload());
        }
    }
    v.push(u8::from(sdk.pending_snapshot));
    if !sdk.coverage_thresholds.is_empty() {
        v.extend_from_slice(b"COVR");
        let count = u64::try_from(sdk.coverage_thresholds.len()).unwrap_or(u64::MAX);
        v.extend_from_slice(&count.to_le_bytes());
        for (thread, threshold) in &sdk.coverage_thresholds {
            v.extend_from_slice(&thread.to_le_bytes());
            v.extend_from_slice(&threshold.to_le_bytes());
        }
    }
    Ok(v)
}

fn service_answer_bytes(answer: &channel::Answer) -> Option<Vec<u8>> {
    match answer {
        channel::Answer::Nominal => Some(vec![0]),
        channel::Answer::Data(bytes) if bytes.len() < hypercall_proto::MAX_PAYLOAD => {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(1);
            out.extend(bytes);
            Some(out)
        }
        channel::Answer::Data(_) => None,
    }
}

#[cfg(test)]
mod tests {

    use std::collections::VecDeque;

    use super::*;
    use crate::virtual_time::NormalizedEventClass;
    use vmm_backend::{ExitReason, Gpa, VcpuState, X86, X86Caps, X86Exit, X86Policy};

    use crate::vendor::x86::devices::REPORT_PORT;
    use crate::vendor::x86::dispatch::{
        APIC_MMIO_BASE, COM1_IRQ_VECTOR, DOORBELL_PORT, IA32_TSC_ADJUST, MsrDir, RFLAGS_IF,
        VIRTUAL_TIME_TICK_PORT, contract_vclock_config, lookup_cpuid,
    };
    use crate::vendor::x86::records as snapshot;

    const TEST_RAM: usize = if cfg!(miri) { 0x1_0000 } else { 0x2_0000 };

    #[test]
    fn msr_dir_renders_direction_and_exit_reason() {
        assert_eq!(MsrDir::Read.dir(), "RDMSR");
        assert_eq!(MsrDir::Write.dir(), "WRMSR");
        assert_eq!(MsrDir::Read.exit_reason(), "KVM_EXIT_X86_RDMSR");
        assert_eq!(MsrDir::Write.exit_reason(), "KVM_EXIT_X86_WRMSR");
    }

    #[test]
    fn lookup_cpuid_exact_leaf_only_and_default() {
        let l1 = lookup_cpuid(1, 0);
        assert_eq!(l1.leaf, 1);
        assert_eq!(l1.eax, 0x0009_06ec);
        assert_eq!(lookup_cpuid(4, 2).eax, 0x0000_0143);
        assert_eq!(lookup_cpuid(1, 99).eax, 0x0009_06ec);
        let d = lookup_cpuid(0xDEAD, 5);
        assert_eq!((d.leaf, d.subleaf, d.eax), (0xDEAD, 5, 0));
    }

    use vmm_backend::{Completion, CpuidModel, MockBackend, MsrFilter};

    const TEST_SERVICE_IDENTITY: &[u8] = b"vmm-test-service.v1";

    #[derive(Clone, Debug, Default)]
    struct SeededService {
        seed: u64,
        calls: u64,
    }

    impl SeededService {
        fn config() -> ServiceConfig {
            ServiceConfig {
                identity: b"vmm-seeded-service.v1".to_vec(),
                configuration: b"seeded".to_vec(),
            }
        }
    }

    impl channel::ServiceHandler for SeededService {
        fn identity(&self) -> &[u8] {
            b"vmm-seeded-service.v1"
        }

        fn configuration(&self) -> &[u8] {
            b"seeded"
        }

        fn respond(
            &mut self,
            _moment: environment::Moment,
            _question: &channel::Question,
        ) -> Result<channel::ServiceResponse, channel::ChannelError> {
            self.calls = self.calls.saturating_add(1);
            Ok(channel::ServiceResponse::Answered(channel::Answer::Nominal))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, channel::ChannelError> {
            let mut state = Vec::with_capacity(16);
            state.extend_from_slice(&self.seed.to_le_bytes());
            state.extend_from_slice(&self.calls.to_le_bytes());
            Ok(state)
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), channel::ChannelError> {
            let state: [u8; 16] = state
                .try_into()
                .map_err(|_| channel::ChannelError::Malformed)?;
            self.seed = u64::from_le_bytes(state[..8].try_into().unwrap());
            self.calls = u64::from_le_bytes(state[8..].try_into().unwrap());
            Ok(())
        }

        fn reseed(&mut self, seed: u64) {
            self.seed = seed;
        }

        fn clone_box(&self) -> Box<dyn channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone, Debug, Default)]
    struct OversizedService;

    impl channel::ServiceHandler for OversizedService {
        fn identity(&self) -> &[u8] {
            TEST_SERVICE_IDENTITY
        }

        fn configuration(&self) -> &[u8] {
            b"oversized"
        }

        fn respond(
            &mut self,
            _moment: environment::Moment,
            _question: &channel::Question,
        ) -> Result<channel::ServiceResponse, channel::ChannelError> {
            Ok(channel::ServiceResponse::Answered(channel::Answer::Data(
                vec![0; MAX_PAYLOAD],
            )))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, channel::ChannelError> {
            Ok(Vec::new())
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), channel::ChannelError> {
            if state.is_empty() {
                Ok(())
            } else {
                Err(channel::ChannelError::Malformed)
            }
        }

        fn clone_box(&self) -> Box<dyn channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }

    fn enable_nominal(vmm: &mut Vmm<MockBackend>, seed: u64) {
        let config = ServiceConfig::default();
        vmm.enable_sdk(
            channel::RecordedEnv::new(seed, Box::new(channel::NominalHandler)),
            &config,
        );
    }

    fn nominal_env(seed: u64) -> channel::RecordedEnv<Box<dyn channel::ServiceHandler>> {
        channel::RecordedEnv::new(seed, Box::new(channel::NominalHandler))
    }

    fn enable_oversized(vmm: &mut Vmm<MockBackend>, seed: u64) {
        let config = ServiceConfig {
            identity: TEST_SERVICE_IDENTITY.to_vec(),
            configuration: b"oversized".to_vec(),
        };
        vmm.enable_sdk(
            channel::RecordedEnv::new(seed, Box::new(OversizedService)),
            &config,
        );
    }

    fn enable_seeded_service(vmm: &mut Vmm<MockBackend>, seed: u64) {
        let config = SeededService::config();
        vmm.enable_sdk(
            channel::RecordedEnv::new(seed, Box::new(SeededService::default())),
            &config,
        );
    }

    #[test]
    fn cancellation_flag_is_the_backend_latch() {
        let latch = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let backend = configured_mock(Vec::new()).with_cancellation_flag(latch.clone());
        let vmm = Vmm::new(backend, GuestRam::new(0x1000).unwrap());
        let reported = vmm.cancellation_flag().expect("mock reports its latch");
        assert!(std::sync::Arc::ptr_eq(&reported, &latch));

        let unbounded = Vmm::new(configured_mock(Vec::new()), GuestRam::new(0x1000).unwrap());
        assert!(unbounded.cancellation_flag().is_none());
    }

    fn configured_mock(exits: Vec<Exit<X86>>) -> MockBackend {
        let mut m = MockBackend::with_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .expect("set_policy");
        m
    }

    fn canonical_xsave_image() -> Vec<u8> {
        let mut image = vec![0; 576];
        image[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        image[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
        image[28..32].copy_from_slice(&0x0000FFFFu32.to_le_bytes());
        image[512..520].copy_from_slice(&3u64.to_le_bytes());
        vmm_backend::arch::x86::canonicalize_xsave(&mut image);
        image
    }

    fn vtime_vmm(exits: Vec<Exit<X86>>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm
    }

    #[test]
    fn state_hash_streaming_keeps_the_frozen_blob_digest() {
        let vmm = vtime_vmm(Vec::new(), 0x5eed);
        let mut expected = Sha256::new();
        expected.update(vmm.state_blob().unwrap());
        let expected: [u8; 32] = expected.finalize().into();
        assert_eq!(vmm.state_hash().unwrap(), expected);
    }

    #[test]
    fn state_hash_covers_xsave_restore_provenance() {
        let state_for = |restore_bv: Option<u64>, snapshot_hashing| {
            let mut backend = configured_mock(Vec::new());
            backend.set_state(VcpuState {
                xsave: canonical_xsave_image(),
                xsave_restore_bv: restore_bv,
                ..Default::default()
            });
            let mut vmm = Vmm::new(backend, GuestRam::new(0x1000).unwrap());
            if snapshot_hashing {
                vmm.wire_snapshot_hashing();
            }
            (vmm.state_blob().unwrap(), vmm.state_hash().unwrap())
        };

        for snapshot_hashing in [false, true] {
            let (zero_blob, zero_hash) = state_for(Some(0), snapshot_hashing);
            let (three_blob, three_hash) = state_for(Some(3), snapshot_hashing);
            let (two_blob, two_hash) = state_for(Some(2), snapshot_hashing);
            let (none_blob, none_hash) = state_for(None, snapshot_hashing);
            assert_ne!(
                zero_blob, two_blob,
                "zero and SSE restore bitmaps must reach the canonical hash"
            );
            assert_ne!(
                zero_hash, two_hash,
                "zero and SSE restore bitmaps must change state_hash"
            );
            assert_ne!(
                zero_blob, three_blob,
                "zero and x87+SSE restore bitmaps must reach the canonical hash"
            );
            assert_ne!(
                zero_hash, three_hash,
                "zero and x87+SSE restore bitmaps must change state_hash"
            );
            assert_ne!(
                three_blob, two_blob,
                "distinct XSAVE restore bitmaps must reach the canonical hash"
            );
            assert_ne!(
                three_hash, two_hash,
                "distinct XSAVE restore bitmaps must change state_hash"
            );
            assert_ne!(
                three_blob, none_blob,
                "present XSAVE restore provenance must differ from legacy absence"
            );
            assert_ne!(
                three_hash, none_hash,
                "present XSAVE restore provenance must differ from legacy state_hash"
            );
            assert_ne!(zero_blob, none_blob);
            assert_ne!(zero_hash, none_hash);
            assert_ne!(two_blob, none_blob);
            assert_ne!(two_hash, none_hash);
            if !snapshot_hashing {
                assert_eq!(
                    none_hash,
                    [
                        0x82, 0x05, 0x10, 0xc1, 0x83, 0x86, 0xa3, 0xa5, 0xc3, 0x55, 0xd6, 0x77,
                        0x2f, 0x36, 0x72, 0x37, 0x22, 0xa2, 0x58, 0x9a, 0xad, 0x95, 0xbe, 0x93,
                        0x91, 0xa3, 0x43, 0x1c, 0x52, 0xb1, 0x69, 0x2d,
                    ],
                    "legacy None XSAVE provenance must preserve the vCPU hash"
                );
            }
        }
    }

    #[test]
    fn xsave_restore_provenance_stays_in_persisted_snapshot_bytes() {
        let state_for = |restore_bv| {
            let state = vm_state::VmState {
                xsave: vm_state::XsaveImage(canonical_xsave_image()),
                xsave_restore_bv: Some(restore_bv),
                ..Default::default()
            };
            state.encode().unwrap()
        };
        let encoded_three = state_for(3);
        let encoded_two = state_for(2);
        assert_ne!(
            encoded_three, encoded_two,
            "distinct restore provenance must remain distinct on the wire"
        );
        assert_eq!(
            vm_state::VmState::decode(&encoded_three)
                .unwrap()
                .xsave_restore_bv,
            Some(3)
        );
        assert_eq!(
            vm_state::VmState::decode(&encoded_two)
                .unwrap()
                .xsave_restore_bv,
            Some(2)
        );
    }

    #[test]
    fn xsave_hash_uses_complete_snapshot_records() {
        fn chunk_payload<'a>(blob: &'a [u8], wanted: &[u8; 4]) -> &'a [u8] {
            let mut offset = 0;
            while offset < blob.len() {
                assert!(
                    blob.len() - offset >= 12,
                    "truncated state blob chunk header at {offset}"
                );
                let tag = &blob[offset..offset + 4];
                let len = u64::from_le_bytes(
                    blob[offset + 4..offset + 12]
                        .try_into()
                        .expect("eight-byte chunk length"),
                );
                let payload_start = offset + 12;
                let payload_end = payload_start
                    .checked_add(usize::try_from(len).expect("chunk length fits usize"))
                    .expect("chunk end does not overflow");
                assert!(
                    payload_end <= blob.len(),
                    "chunk {tag:?} extends past the state blob"
                );
                if tag == wanted {
                    return &blob[payload_start..payload_end];
                }
                offset = payload_end;
            }
            panic!("state blob has no {wanted:?} chunk");
        }

        for restore_bv in [0, 2, 3] {
            let mut backend = configured_mock(Vec::new());
            backend.set_state(VcpuState {
                xsave: canonical_xsave_image(),
                xsave_restore_bv: Some(restore_bv),
                ..Default::default()
            });
            let mut vmm = Vmm::new(backend, GuestRam::new(0x1000).unwrap());
            vmm.wire_snapshot_hashing();

            let snapshot = vmm.save_vm_state().unwrap();
            let expected = <vm_state::VmState as SnapshotRecords>::encode(&snapshot).unwrap();
            let blob = vmm.state_blob().unwrap();
            let actual = chunk_payload(&blob, b"VMST");
            assert_eq!(
                actual,
                expected.as_slice(),
                "complete VMST for restore BV {restore_bv}"
            );
            assert_eq!(
                vm_state::VmState::decode(actual).unwrap().xsave_restore_bv,
                Some(restore_bv),
                "VMST must retain restore BV {restore_bv}"
            );
        }
    }

    #[test]
    fn deferred_checkpoint_hash_is_byte_identical_and_cannot_overwrite() {
        let exits = || {
            (0..256)
                .map(|_| Exit::Arch(X86Exit::Rdmsr { index: 0x10 }))
                .collect::<Vec<_>>()
        };
        let mut synchronous = vtime_vmm(exits(), 7);
        let mut deferred = vtime_vmm(exits(), 7);
        deferred.defer_virtual_time_checkpoint_hashes().unwrap();

        for _ in 0..256 {
            assert_eq!(synchronous.step().unwrap(), Step::Continued);
            assert_eq!(deferred.step().unwrap(), Step::Continued);
        }

        let expected = synchronous
            .virtual_time_trace()
            .unwrap()
            .normalized_log()
            .events[255]
            .state_hash
            .unwrap();
        assert_eq!(deferred.state_hash().unwrap(), expected);
        assert_eq!(
            deferred
                .virtual_time_trace()
                .unwrap()
                .normalized_log()
                .events[255]
                .state_hash,
            None
        );
        deferred
            .checkpoint_virtual_time_trace_at(255, expected)
            .unwrap();
        assert_eq!(
            synchronous.virtual_time_trace().unwrap().normalized_log(),
            deferred.virtual_time_trace().unwrap().normalized_log()
        );
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(255, expected)
                .is_err()
        );
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(254, expected)
                .is_err()
        );
    }

    #[test]
    fn deferral_chosen_before_the_first_run_covers_that_run() {
        let exits = || {
            (0..256)
                .map(|_| Exit::Arch(X86Exit::Rdmsr { index: 0x10 }))
                .collect::<Vec<_>>()
        };
        let mut vmm = vtime_vmm(exits(), 11);
        vmm.wire_snapshot_hashing();
        vmm.defer_virtual_time_checkpoint_hashes().unwrap();
        for _ in 0..256 {
            assert_eq!(vmm.step().unwrap(), Step::Continued);
        }
        assert!(vmm.snapshot_hashing_wired());
        assert_eq!(
            vmm.virtual_time_trace().unwrap().normalized_log().events[255].state_hash,
            None,
            "the checkpoint the run itself reached was deferred"
        );
        let hash = vmm.state_hash().unwrap();
        vmm.checkpoint_virtual_time_trace_at(255, hash).unwrap();
        assert_eq!(
            vmm.virtual_time_trace().unwrap().normalized_log().events[255].state_hash,
            Some(hash),
            "the deferred hash still materializes"
        );

        let mut late = vtime_vmm(exits(), 11);
        late.wire_snapshot_hashing();
        assert_eq!(late.step().unwrap(), Step::Continued);
        assert!(late.defer_virtual_time_checkpoint_hashes().is_err());
    }

    #[test]
    fn lifecycle_event_classifier_distinguishes_frame_complete_from_neighbors() {
        let id = |local| (u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT) | local;
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(1), &[0; 8]),
            SdkEventAction::DeferSnapshot
        );
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(2), &[0; 8]),
            SdkEventAction::Capture
        );
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(1), &[]),
            SdkEventAction::Malformed
        );
    }

    #[test]
    fn doorbell_offer_predicate_is_exact_for_an_unconfigured_composition() {
        let mut vmm = Vmm::new(
            configured_mock(Vec::new()),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        for service in [
            ServiceId::Event,
            ServiceId::Sdk,
            ServiceId::Entropy,
            ServiceId::Payload,
            ServiceId::Pvclock,
        ] {
            assert!(!vmm.doorbell_service_offered(service as u16));
        }
        enable_nominal(&mut vmm, 7);
        assert!(vmm.doorbell_service_offered(ServiceId::Event as u16));
        assert!(vmm.doorbell_service_offered(ServiceId::Sdk as u16));
        assert!(vmm.doorbell_service_offered(ServiceId::Entropy as u16));
        assert!(!vmm.doorbell_service_offered(ServiceId::Payload as u16));
        assert!(!vmm.doorbell_service_offered(ServiceId::Pvclock as u16));
    }

    #[test]
    fn drain_unions_backend_log_with_host_writes_and_drains() {
        let mut m = configured_mock(vec![]);
        m.push_dirty_gfns(vec![5, 3, 5]);
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.apply_effect(&channel::Effect::XorMemory {
            gpa: 7 * 4096 - 4,
            bytes: vec![0xff; 8],
        })
        .unwrap();
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![3, 5, 6, 7]));
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn doorbell_response_write_is_drained_as_host_dirty() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[0xAB; 16]).unwrap();
        assert_eq!(
            vmm.drain_dirty_pages(),
            Some(vec![(RESP_GPA as u64) / 4096])
        );
    }

    #[test]
    fn drain_rebases_high_base_gfns_and_excludes_the_doorbell_slot() {
        let mut m = configured_mock(vec![]);
        m.push_dirty_gfns(vec![0x4_0001, 0x4_0003, 15]);
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.ram_base_gpa = 0x4000_0000;

        let dirty = vmm.drain_dirty_pages().unwrap();
        assert_eq!(
            dirty,
            vec![1, 3],
            "high GFNs rebased to main-RAM indices; the doorbell slot's GFN excluded"
        );
        let pages = (vmm.ram.len() / 4096) as u64;
        assert!(
            dirty.iter().all(|&g| g < pages),
            "every drained GFN indexes the main RAM — no snapshot_derive fallback"
        );
    }

    #[test]
    fn wholesale_host_write_poisons_the_drain_until_reset() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.restore_guest_memory(&vec![7u8; TEST_RAM]).unwrap();
        assert_eq!(vmm.drain_dirty_pages(), None, "untrackable ⇒ no dirty set");
        assert!(vmm.reset_dirty_tracking(), "re-arm at the new baseline");
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn drain_declines_without_backend_tracking() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[1]).unwrap();
        assert_eq!(vmm.drain_dirty_pages(), None);
        assert!(!vmm.reset_dirty_tracking());
    }

    #[test]
    fn write_guest_pages_writes_pages_and_poison_dirty_drain_until_reset() {
        let mut backend = configured_mock(vec![]);
        backend.enable_dirty_tracking();
        let mut vmm = Vmm::new(backend, GuestRam::new(TEST_RAM).unwrap());
        let page_a = [0xA5_u8; 4096];
        let page_b = [0x5A_u8; 4096];

        vmm.write_guest_pages(&[(2, page_a), (5, page_b)]).unwrap();
        assert_eq!(&vmm.guest_memory()[2 * 4096..3 * 4096], &page_a);
        assert_eq!(&vmm.guest_memory()[5 * 4096..6 * 4096], &page_b);
        assert_eq!(vmm.drain_dirty_pages(), None);
        assert!(vmm.reset_dirty_tracking());
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn write_guest_pages_empty_input_is_a_noop() {
        let mut backend = configured_mock(vec![]);
        backend.enable_dirty_tracking();
        let mut vmm = Vmm::new(backend, GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();

        vmm.write_guest_pages(&[]).unwrap();

        assert_eq!(vmm.guest_memory(), &before);
        assert!(!vmm.host_dirty_wholesale);
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn write_guest_pages_out_of_range_is_atomic() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();
        let page = [0xCC_u8; 4096];
        let out_of_range = (TEST_RAM / 4096) as u64;

        assert!(matches!(
            vmm.write_guest_pages(&[(1, page), (out_of_range, page)]),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(vmm.guest_memory(), &before);
        assert!(vmm.host_dirty.is_empty());
        assert!(!vmm.host_dirty_wholesale);
    }

    #[test]
    fn write_guest_pages_duplicate_gfn_is_atomic() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();
        let page_a = [0x11_u8; 4096];
        let page_b = [0x22_u8; 4096];

        assert!(matches!(
            vmm.write_guest_pages(&[(3, page_a), (3, page_b)]),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(vmm.guest_memory(), &before);
        assert!(vmm.host_dirty.is_empty());
        assert!(!vmm.host_dirty_wholesale);
    }

    #[test]
    fn guest_page_write_validation_covers_each_ram_and_gpa_boundary() {
        assert!(!valid_guest_ram_len(0, 4096));
        assert!(!valid_guest_ram_len(4095, 4096));
        assert!(valid_guest_ram_len(4096, 4096));

        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.ram_base_gpa = u64::MAX - 4095;
        let before = vmm.guest_memory().to_vec();
        assert!(matches!(
            vmm.write_guest_pages(&[(0, [0xA5; 4096])]),
            Err(VmmError::ContractViolation(message))
                if message.contains("invalid GPA range")
        ));
        assert_eq!(vmm.guest_memory(), &before);
    }

    #[test]
    fn doorbell_exit_counter_reports_the_exact_observed_count() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.doorbell_exits = 7;
        assert_eq!(vmm.doorbell_exits(), 7);
    }

    #[test]
    fn doorbell_services_events_and_surfaces_stops() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        enable_nominal(&mut vmm, 7);

        fn ring(
            vmm: &mut Vmm<MockBackend>,
            service: ServiceId,
            payload: &[u8],
        ) -> (Step, u16, Vec<u8>) {
            let mut buf = [0u8; HC_PAGE];
            let n = hypercall_proto::encode_request(service, 1, 1, payload, &mut buf).unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            (step, hdr.status, pl.to_vec())
        }

        let hit_id = (1u32 << 24) | 1;
        let mut hit = hit_id.to_le_bytes().to_vec();
        hit.extend_from_slice(&[0, 0, 0]);
        assert_eq!(ring(&mut vmm, ServiceId::Event, &hit).0, Step::Continued);
        assert_eq!(vmm.sdk_events().len(), 1);
    }

    #[test]
    fn coverage_doorbell_uses_the_scheduler_vocabulary() {
        let seed = 0x6d36;
        let mut expected_env = channel::RecordedEnv::nominal(seed);
        let expected = match expected_env
            .decide(&channel::Question::scheduler(3).unwrap())
            .unwrap()
        {
            channel::ServiceResponse::Answered(channel::Answer::Data(bytes)) => {
                u32::from_le_bytes(bytes.try_into().expect("scheduler answer is four bytes"))
            }
            other => panic!("unexpected scheduler answer: {other:?}"),
        };

        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        enable_nominal(&mut vmm, seed);
        let mut request = [0_u8; SDK_COVERAGE_REQUEST_LEN];
        request[0..4].copy_from_slice(&7_u32.to_le_bytes());
        request[4..12].copy_from_slice(&SDK_COVERAGE_QUANTUM.to_le_bytes());
        request[12..16].copy_from_slice(&3_u32.to_le_bytes());
        let mut frame = [0_u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Sdk, 2, 9, &request, &mut frame)
            .expect("coverage request");
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        assert_eq!(vmm.service_doorbell(n as u32).unwrap(), Step::Continued);
        let page = &vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE];
        let (header, payload) = decode(page).expect("coverage response");
        assert_eq!(header.status, Status::Ok as u16);
        assert_eq!(
            u32::from_le_bytes(payload[8..12].try_into().unwrap()),
            expected
        );
        assert_eq!(
            vmm.sdk_coverage(),
            &[(0, 7, SDK_COVERAGE_QUANTUM, 3, expected)]
        );
    }

    #[test]
    fn coverage_threshold_is_enforced_and_snapshot_replay_reproduces() {
        let build = || {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            enable_nominal(&mut vmm, 0x51ced);
            vmm
        };
        let mut base = build();
        let first = base.decide_coverage(4, 1, 1, 2).unwrap();
        let snap = base.sdk_snapshot().unwrap().expect("SDK snapshot");
        let hash_after_first = base.state_hash().unwrap();
        assert_eq!(base.decide_coverage(5, 1, 3, 2), Err(Status::BadRequest));
        assert_eq!(base.state_hash().unwrap(), hash_after_first);
        let continuation = base.decide_coverage(5, 1, first.0, 2).unwrap();
        let mut replay = build();
        replay.sdk_restore(&snap).unwrap();
        assert_eq!(replay.state_hash().unwrap(), hash_after_first);
        assert_eq!(
            replay.decide_coverage(5, 1, first.0, 2).unwrap(),
            continuation
        );
    }

    #[test]
    fn malformed_scheduler_answer_does_not_advance_the_channel() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        enable_nominal(&mut vmm, 0x51ced);
        vmm.sdk
            .as_mut()
            .expect("SDK enabled")
            .env
            .record_service_request(
                41,
                channel::SERVICE_SCHEDULER,
                7,
                channel::Answer::Data(vec![1, 2, 3]),
            );
        let before = vmm.state_hash().unwrap();
        assert_eq!(vmm.decide_coverage(41, 7, 1, 2), Err(Status::Internal));
        assert_eq!(vmm.state_hash().unwrap(), before);
        assert!(vmm.sdk_coverage().is_empty());
    }

    #[test]
    fn generic_service_response_reserves_one_byte_for_its_tag() {
        let capacity = hypercall_proto::MAX_PAYLOAD;
        let bytes = vec![0x5a; capacity - 1];
        let frame = service_answer_bytes(&channel::Answer::Data(bytes.clone())).unwrap();
        assert_eq!(frame.len(), capacity);
        assert_eq!(frame[0], 1);
        assert_eq!(&frame[1..], bytes);
        assert_eq!(
            service_answer_bytes(&channel::Answer::Data(vec![0; capacity])),
            None
        );
    }

    #[test]
    fn payload_tape_is_exact_hash_visible_snapshot_complete_and_exhausts_loudly() {
        fn make(entries: Vec<Vec<u8>>) -> Vmm<MockBackend> {
            let mut env: channel::RecordedEnv<Box<dyn channel::ServiceHandler>> =
                channel::RecordedEnv::new(7, Box::new(channel::NominalHandler));
            env.set_payloads(Some(entries)).unwrap();
            let config = ServiceConfig::default();
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.enable_sdk(env, &config);
            vmm
        }
        fn ring(vmm: &mut Vmm<MockBackend>, bytes: u32) -> (Step, u16, Vec<u8>) {
            let mut frame = [0_u8; HC_PAGE];
            let n = hypercall_proto::encode_request(
                ServiceId::Payload,
                1,
                1,
                &bytes.to_le_bytes(),
                &mut frame,
            )
            .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = &vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE];
            let (header, payload) = decode(page).unwrap();
            (step, header.status, payload.to_vec())
        }
        let mut vmm = make(vec![vec![0x81, 4], vec![0, 2]]);
        assert_eq!(
            ring(&mut vmm, 2),
            (Step::Continued, Status::Ok as u16, vec![0x81, 4])
        );
        let snap = vmm.sdk_snapshot().unwrap().expect("SDK snapshot");
        assert_eq!(snap.remaining_payloads(), Some(vec![vec![0, 2]]));
        assert_eq!(
            ring(&mut vmm, 2),
            (Step::Continued, Status::Ok as u16, vec![0, 2])
        );
        vmm.sdk_restore(&snap).unwrap();
        assert_eq!(ring(&mut vmm, 2).2, vec![0, 2]);
        assert_eq!(ring(&mut vmm, 2).0, Step::SdkStop);
        assert_eq!(vmm.take_sdk_stop(), Some(SdkStop::Quiescent));
    }

    #[test]
    fn doorbell_uses_a_dedicated_memslot_when_ram_is_based_high() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.ram_base_gpa = 0x4000_0000;
        enable_nominal(&mut vmm, 7);
        assert!(vmm.service_doorbell(16).is_err());
        vmm.map_doorbell_pages().unwrap();
        let hit_id = (1u32 << 24) | 1;
        let mut payload = hit_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0, 0, 0]);
        let mut buf = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut buf).unwrap();
        let req_off = REQ_GPA - DOORBELL_MAP_GPA;
        vmm.doorbell_pages.as_mut().unwrap().as_mut_bytes()[req_off..req_off + n]
            .copy_from_slice(&buf[..n]);
        assert_eq!(vmm.service_doorbell(n as u32).unwrap(), Step::Continued);
        let resp_off = RESP_GPA - DOORBELL_MAP_GPA;
        let resp =
            vmm.doorbell_pages.as_ref().unwrap().as_bytes()[resp_off..resp_off + HC_PAGE].to_vec();
        let (hdr, _) = decode(&resp).expect("dedicated response");
        assert_eq!(hdr.status, Status::Ok as u16);
        assert!(
            vmm.guest_memory()[REQ_GPA..RESP_GPA + HC_PAGE]
                .iter()
                .all(|&b| b == 0)
        );
        assert!(vmm.host_dirty.contains(&(RESP_GPA as u64 / 4096)));
    }

    #[test]
    fn high_ram_base_resolves_absolute_gpas_and_hashes_the_doorbell() {
        let make = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.ram_base_gpa = 0x4000_0000;
            v.map_doorbell_pages().unwrap();
            v
        };
        let a = make();
        let mut b = make();
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        b.doorbell_pages.as_mut().unwrap().as_mut_bytes()[7] ^= 0xFF;
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
        let mut v = make();
        v.ram.as_mut_bytes()[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(v.guest_slice(0x4000_0100, 4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(v.guest_slice(0x100, 4), None);
        assert!(v.guest_slice(REQ_GPA as u64, HC_PAGE).is_some());
        v.apply_effect(&channel::Effect::XorMemory {
            gpa: 0x4000_0100,
            bytes: vec![0xFF],
        })
        .unwrap();
        assert_eq!(v.ram.as_bytes()[0x100], 1 ^ 0xFF);
        assert!(
            v.apply_effect(&channel::Effect::XorMemory {
                gpa: 0x500,
                bytes: vec![0xFF]
            })
            .is_err()
        );
    }

    #[test]
    fn arm64_save_restore_preserves_the_doorbell_pages() {
        use crate::vendor::arm64::board::RAM_BASE as ARM_RAM_BASE;
        use vmm_backend::{Arm64Policy, MockArm64Backend};

        let arm_vmm = || {
            let mut b = MockArm64Backend::new();
            b.set_policy(&Arm64Policy::default()).unwrap();
            let mut v = Vmm::new(b, GuestRam::new(0x10_0000).unwrap());
            v.ram_base_gpa = ARM_RAM_BASE;
            v.map_doorbell_pages().unwrap();
            v
        };

        let mut src = arm_vmm();
        src.doorbell_pages.as_mut().unwrap().as_mut_bytes()[..4]
            .copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let blob = src.save_vm_state().unwrap();

        let mut dst = arm_vmm();
        dst.restore_vm_state(&blob).unwrap();
        assert_eq!(
            &dst.doorbell_pages.as_ref().unwrap().as_bytes()[..4],
            &[0xAA, 0xBB, 0xCC, 0xDD]
        );

        let mut nodoor = {
            let mut b = MockArm64Backend::new();
            b.set_policy(&Arm64Policy::default()).unwrap();
            Vmm::new(b, GuestRam::new(0x10_0000).unwrap())
        };
        let err = nodoor.restore_vm_state(&blob).unwrap_err();
        assert!(
            format!("{err}").contains("doorbell wiring mismatch"),
            "{err}"
        );
    }

    #[test]
    fn state_components_localizes_a_doorbell_only_divergence() {
        let make = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.ram_base_gpa = 0x4000_0000;
            v.map_doorbell_pages().unwrap();
            v
        };
        let a = make();
        let mut b = make();
        b.doorbell_pages.as_mut().unwrap().as_mut_bytes()[3] ^= 0xFF;

        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());

        let ca = a.state_components();
        let cb = b.state_components();
        let da = ca
            .iter()
            .find(|(l, _)| *l == "doorbell")
            .expect("a doorbell component");
        let db_ = cb
            .iter()
            .find(|(l, _)| *l == "doorbell")
            .expect("a doorbell component");
        assert_ne!(da.1, db_.1, "the doorbell component must localize it");
        for (la, dga) in &ca {
            if *la == "doorbell" {
                continue;
            }
            let dgb = cb.iter().find(|(lb, _)| lb == la).map(|(_, d)| d);
            assert_eq!(
                Some(dga),
                dgb,
                "component {la} must match (only doorbell differs)"
            );
        }

        let plain = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        assert!(
            !plain
                .state_components()
                .iter()
                .any(|(l, _)| *l == "doorbell")
        );
    }

    #[test]
    fn classify_sdk_event_payload_matrix() {
        type C = SdkEventAction;
        let assert_id = (u32::from(SDK_NS_ASSERT) << SDK_NS_SHIFT) | 20;
        let setup_id = u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT;
        let frame_id = setup_id | 1;
        let state_id = (2u32 << SDK_NS_SHIFT) | 3;
        let classify = Vmm::<MockBackend>::classify_sdk_event;

        assert_eq!(
            classify(assert_id, &[1, 0, 0]),
            C::Stop(SdkStop::Assertion {
                id: 20,
                data: vec![]
            })
        );
        assert_eq!(
            classify(assert_id, &[1, 2, 0, 0xAB, 0xCD]),
            C::Stop(SdkStop::Assertion {
                id: 20,
                data: vec![0xAB, 0xCD]
            })
        );
        assert_eq!(classify(assert_id, &[1, 2, 0]), C::Malformed);
        assert_eq!(classify(assert_id, &[1, 0, 0, 0x99]), C::Malformed);
        assert_eq!(classify(assert_id, &[1]), C::Malformed);
        assert_eq!(classify(assert_id, &[1, 0]), C::Malformed);
        assert_eq!(classify(assert_id, &[0, 0, 0]), C::Capture);
        assert_eq!(classify(assert_id, &[9, 0, 0]), C::Capture);

        assert_eq!(classify(setup_id, &[]), C::DeferSnapshot);
        assert_eq!(classify(setup_id, &[0xAB]), C::Malformed);
        assert_eq!(classify(setup_id, &[0; 4]), C::Malformed);

        assert_eq!(classify(frame_id, &17_u64.to_le_bytes()), C::DeferSnapshot);
        assert_eq!(classify(frame_id, &[]), C::Malformed);
        assert_eq!(classify(frame_id, &[0; 7]), C::Malformed);
        assert_eq!(classify(frame_id, &[0; 9]), C::Malformed);

        assert_eq!(classify(state_id, &[0, 1, 2, 3]), C::Capture);
        assert_eq!(classify((9u32 << SDK_NS_SHIFT) | 7, &[1, 2, 3]), C::Capture);
    }

    #[test]
    fn doorbell_rejects_malformed_sdk_event_payloads() {
        fn ring(vmm: &mut Vmm<MockBackend>, event_id: u32, data: &[u8]) -> (u16, bool, usize) {
            let mut payload = event_id.to_le_bytes().to_vec();
            payload.extend_from_slice(data);
            let mut buf = [0u8; HC_PAGE];
            let n = hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut buf)
                .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, _) = decode(&page).expect("a response frame");
            (hdr.status, step == Step::SdkStop, vmm.sdk_events().len())
        }
        let mk = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.enable_sdk(nominal_env(1), &ServiceConfig::default());
            v
        };
        let assert_id = (u32::from(SDK_NS_ASSERT) << SDK_NS_SHIFT) | 20;
        let setup_id = u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT;
        let frame_id = setup_id | 1;

        let mut v = mk();
        assert_eq!(
            ring(&mut v, assert_id, &[1, 2, 0]),
            (Status::BadRequest as u16, false, 0),
            "a malformed assert violation is rejected, never a bug from garbage"
        );

        let mut v = mk();
        assert_eq!(
            ring(&mut v, setup_id, &[0xAB]),
            (Status::BadRequest as u16, false, 0),
            "a non-empty setup_complete is rejected, never arms the deferral"
        );

        let mut v = mk();
        assert_eq!(ring(&mut v, setup_id, &[]), (Status::Ok as u16, false, 1));

        let mut v = mk();
        assert_eq!(
            ring(&mut v, frame_id, &[0; 7]),
            (Status::BadRequest as u16, false, 0),
            "a short frame_complete is rejected, never arms the deferral"
        );

        let mut v = mk();
        assert_eq!(
            ring(&mut v, frame_id, &17_u64.to_le_bytes()),
            (Status::Ok as u16, false, 1)
        );
    }

    #[test]
    fn doorbell_probe_on_a_channel_less_vm_answers_unknown_service() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            1,
            5,
            &0x4000u64.to_le_bytes(),
            &mut buf,
        )
        .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(step, Step::Continued);
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written, not a silent drop");
        assert_eq!(
            hdr.status,
            Status::UnknownService as u16,
            "clean UnknownService"
        );
        assert_eq!(
            hdr.service,
            ServiceId::Pvclock as u16,
            "echoes the probed service id"
        );
        assert_eq!(
            (hdr.opcode, hdr.seq),
            (1, 5),
            "echoes the request opcode + seq"
        );
        assert!(pl.is_empty(), "an error frame carries no payload");
    }

    #[test]
    fn doorbell_unknown_service_returns_an_unknown_service_frame() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Sdk, 7, 99, &[], &mut buf).unwrap();
        let unknown: u16 = 0xABCD;
        buf[6..8].copy_from_slice(&unknown.to_le_bytes());
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(step, Step::Continued);
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written, not a silent drop");
        assert_eq!(
            hdr.status,
            Status::UnknownService as u16,
            "clean UnknownService"
        );
        assert_eq!(
            hdr.service, unknown,
            "echoes the raw service id so the guest correlates the reply"
        );
        assert_eq!(hdr.opcode, 7, "echoes the request opcode");
        assert_eq!(hdr.seq, 99, "echoes the request seq");
        assert!(pl.is_empty(), "an error frame carries no payload");
    }

    #[test]
    fn doorbell_rejects_a_non_request_frame() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_response(ServiceId::Sdk, 1, 42, Status::Ok, &[], &mut buf)
            .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(
            step,
            Step::Continued,
            "a rejected frame does not stop the run"
        );
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written");
        assert_eq!(
            hdr.status,
            Status::BadRequest as u16,
            "non-request → BadRequest"
        );
        assert_eq!(hdr.service, ServiceId::Sdk as u16, "echoes the raw service");
        assert_eq!(hdr.seq, 42, "echoes the raw seq");
        assert!(pl.is_empty());
    }

    #[test]
    fn doorbell_bad_entropy_opcode_is_unknown_opcode() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Entropy, 2, 7, &[], &mut buf).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&page).expect("a response frame is written");
        assert_eq!(
            hdr.status,
            Status::UnknownOpcode as u16,
            "a known service with a bad opcode → UnknownOpcode, not UnknownService"
        );
        assert_eq!(
            hdr.service,
            ServiceId::Entropy as u16,
            "echoes the Entropy service"
        );
        assert_eq!(hdr.opcode, 2, "echoes the bad opcode");
        assert_eq!(hdr.seq, 7);
    }

    #[test]
    fn doorbell_request_header_validation_matrix() {
        fn dispatch_header(mutate: impl FnOnce(&mut [u8])) -> hypercall_proto::FrameHeader {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
            let mut buf = [0u8; HC_PAGE];
            let n = hypercall_proto::encode_request(
                ServiceId::Event,
                1,
                5,
                &7u32.to_le_bytes(),
                &mut buf,
            )
            .unwrap();
            mutate(&mut buf);
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            decode(&page).expect("a response frame is always written").0
        }
        let ev = ServiceId::Event as u16;

        let h = dispatch_header(|_| {});
        assert_eq!(h.status, Status::Ok as u16, "valid request is serviced");
        assert_eq!((h.service, h.opcode, h.seq), (ev, 1, 5));

        let h = dispatch_header(|b| b[4..6].copy_from_slice(&2u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "response-typed rejected"
        );
        assert_eq!((h.service, h.seq), (ev, 5), "BadRequest echoes raw fields");

        let h = dispatch_header(|b| b[10..12].copy_from_slice(&1u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "non-zero-status request rejected (round-11 P2)"
        );
        assert_eq!(h.service, ev);

        let h = dispatch_header(|b| b[20..24].copy_from_slice(&1u32.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "non-zero reserved rejected"
        );

        let h = dispatch_header(|b| b[4..6].copy_from_slice(&3u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "unrecognized message kind rejected"
        );

        let h = dispatch_header(|b| b[6..8].copy_from_slice(&0xABCDu16.to_le_bytes()));
        assert_eq!(h.status, Status::UnknownService as u16, "unknown service");
        assert_eq!(h.service, 0xABCD, "echoes the raw service id");

        let h = dispatch_header(|b| b[8..10].copy_from_slice(&9u16.to_le_bytes()));
        assert_eq!(h.status, Status::UnknownOpcode as u16, "unknown opcode");
        assert_eq!(
            (h.service, h.opcode),
            (ev, 9),
            "echoes service + bad opcode"
        );
    }

    #[test]
    fn oversized_service_answer_preserves_channel_state() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        enable_oversized(&mut vmm, 7);
        let before = vmm
            .sdk
            .as_ref()
            .unwrap()
            .env
            .snapshot_state()
            .unwrap()
            .encode();
        let mut payload = 4u16.to_le_bytes().to_vec();
        payload.extend_from_slice(&77u64.to_le_bytes());
        let mut request = [0; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Sdk, 3, 1, &payload, &mut request).unwrap();
        let mut response = [0; HC_PAGE];
        let (length, stop) = vmm.dispatch_doorbell(19, &request[..n], &mut response);
        assert!(stop.is_none());
        assert_eq!(
            decode(&response[..length]).unwrap().0.status,
            Status::Internal as u16
        );
        assert_eq!(
            before,
            vmm.sdk
                .as_ref()
                .unwrap()
                .env
                .snapshot_state()
                .unwrap()
                .encode()
        );
    }

    #[test]
    fn sdk_snapshot_preserves_unconsumed_stops() {
        for stop in [
            SdkStop::Quiescent,
            SdkStop::Assertion {
                id: 5,
                data: vec![1],
            },
        ] {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            enable_nominal(&mut vmm, 7);
            vmm.sdk.as_mut().unwrap().pending_stop = Some(stop);
            assert!(vmm.save_vm_state().is_ok());
            let snapshot = vmm.sdk_snapshot().unwrap().unwrap();
            let pending = vmm.take_sdk_stop().unwrap();
            vmm.sdk_restore(&snapshot).unwrap();
            assert_eq!(vmm.take_sdk_stop(), Some(pending.clone()));
            vmm.sdk_restore_events(&snapshot);
            assert_eq!(vmm.take_sdk_stop(), Some(pending));
            assert!(vmm.save_vm_state().is_ok());
            assert!(vmm.sdk_snapshot().is_ok());
        }
    }

    #[test]
    fn sdk_restore_keeps_the_pending_response_sequence_and_future() {
        let build = || {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            enable_nominal(&mut vmm, 7);
            vmm
        };
        let mut source = build();
        let question = channel::Question::with_request_id(77, 19, b"choose".to_vec()).unwrap();
        let stop = SdkStop::Decision {
            moment: 31,
            seq: 42,
            question: question.clone(),
        };
        source.sdk.as_mut().unwrap().pending_stop = Some(stop.clone());
        let before = source.state_hash().unwrap();
        let snapshot = source.sdk_snapshot().unwrap().unwrap();
        assert_eq!(source.state_hash().unwrap(), before);
        assert_eq!(source.take_sdk_stop(), Some(stop.clone()));
        assert_eq!(source.take_sdk_stop(), Some(stop.clone()));
        let mut restored = build();
        assert_ne!(restored.state_hash().unwrap(), before);
        restored.sdk_restore(&snapshot).unwrap();
        assert_eq!(restored.state_hash().unwrap(), before);
        assert_eq!(restored.take_sdk_stop(), Some(stop.clone()));
        let mut branched = build();
        branched.sdk_restore_events(&snapshot);
        assert_eq!(branched.take_sdk_stop(), Some(stop));
        let answer = channel::Answer::Data(vec![4, 5, 6]);
        branched.resolve_service_answer(answer.clone()).unwrap();
        assert_eq!(
            source.resolve_service_answer(answer.clone()).unwrap(),
            (31, question.clone())
        );
        assert_eq!(
            restored.resolve_service_answer(answer).unwrap(),
            (31, question)
        );
        let source_response = source.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap();
        let restored_response = restored.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap();
        assert_eq!(restored_response, source_response);
        assert_eq!(
            branched.guest_slice(RESP_GPA as u64, HC_PAGE).unwrap(),
            source_response
        );
        assert_eq!(restored.state_hash().unwrap(), source.state_hash().unwrap());
        assert_eq!(restored.sdk_events(), source.sdk_events());
        assert!(
            restored
                .resolve_service_answer(channel::Answer::Nominal)
                .is_err()
        );
        let completed = source.sdk_snapshot().unwrap().unwrap();
        restored.sdk_restore(&snapshot).unwrap();
        restored.sdk_restore(&completed).unwrap();
        assert!(restored.pending_service_question().is_none());
        assert_eq!(restored.state_hash().unwrap(), source.state_hash().unwrap());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn sdk_snapshot_round_trips_the_pending_deferred_point_hash() {
        let mk = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            enable_nominal(&mut v, 7);
            v
        };
        let mut base = mk();
        let h_false = base.state_hash().unwrap();
        base.sdk.as_mut().unwrap().pending_snapshot = true;
        let h_true = base.state_hash().unwrap();
        assert_ne!(h_false, h_true);
        let snap = base.sdk_snapshot().unwrap().expect("SDK snapshot");
        assert!(snap.pending_snapshot);
        let mut fork = mk();
        fork.sdk_restore(&snap).unwrap();
        assert_eq!(fork.state_hash().unwrap(), h_true);
        let mut events_only = mk();
        events_only.sdk_restore_events(&snap);
        assert_eq!(events_only.state_hash().unwrap(), h_true);
        assert!(events_only.take_snapshot_point());
        assert!(!events_only.take_snapshot_point());
    }

    #[test]
    fn run_stops_on_an_sdk_assertion_not_the_later_terminal() {
        let viol_id: u32 = (1 << 24) | 20;
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]);
        let mut frame = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();

        let mut vmm = Vmm::new(
            configured_mock(vec![
                Exit::Arch(X86Exit::Io {
                    port: DOORBELL_PORT,
                    size: 4,
                    write: Some(n as u32),
                }),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);

        let r = vmm.run().expect("run");
        assert_eq!(
            r.reason,
            TerminalReason::SdkStop,
            "run stops at the assertion, not the HLT that follows"
        );
        assert_eq!(
            r.sdk_stop,
            Some(SdkStop::Assertion {
                id: 20,
                data: vec![]
            })
        );
    }

    #[test]
    fn run_does_not_cache_the_vcpu_on_a_resumable_sdk_stop() {
        let viol_id: u32 = (1 << 24) | 20;
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]);
        let mut frame = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();

        let mut stop_state = nonzero_state();
        stop_state.regs.rip = 0x1000;
        let mut resumed_state = nonzero_state();
        resumed_state.regs.rip = 0x2000;

        let mut mock = configured_mock(vec![Exit::Arch(X86Exit::Io {
            port: DOORBELL_PORT,
            size: 4,
            write: Some(n as u32),
        })]);
        mock.set_state(stop_state.clone());
        let mut vmm = Vmm::new(mock, GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);

        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::SdkStop);
        assert!(
            vmm.saved_state.is_none(),
            "a resumable SDK stop must NOT cache the vCPU (it would go stale on resume)"
        );

        vmm.backend.set_state(resumed_state.clone());
        assert_eq!(
            vmm.current_vcpu(),
            resumed_state,
            "state_blob reads the live resumed vCPU, not the stale stop snapshot"
        );
        assert_ne!(vmm.current_vcpu(), stop_state, "not the stop-time vCPU");
    }

    #[test]
    fn doorbell_is_total_on_edge_requests() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(nominal_env(1), &ServiceConfig::default());

        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, 0).unwrap(),
            Step::Continued
        );
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, HC_PAGE as u32 + 1)
                .unwrap(),
            Step::Continued
        );
        let resp = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&resp).expect("a valid response frame");
        assert_eq!(
            hdr.status,
            Status::BadRequest as u16,
            "an oversize req_len is rejected, not clamped"
        );
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, u32::MAX).unwrap(),
            Step::Continued
        );
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, HC_PAGE as u32).unwrap(),
            Step::Continued
        );

        for (i, b) in vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + 96]
            .iter_mut()
            .enumerate()
        {
            *b = (i as u8).wrapping_mul(37).wrapping_add(1);
        }
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, 96).unwrap(),
            Step::Continued
        );

        assert!(
            vmm.take_sdk_stop().is_none(),
            "no spurious stop from garbage"
        );
        assert!(
            vmm.sdk_events().is_empty(),
            "garbage never captures an event"
        );
    }

    #[test]
    fn doorbell_routes_entropy_deterministically() {
        let mk = || {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Common(CommonExit::Idle)]),
                GuestRam::new(TEST_RAM).unwrap(),
            );
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 99).unwrap());
            vmm.enable_sdk(nominal_env(99), &ServiceConfig::default());
            vmm
        };
        let entropy = |vmm: &mut Vmm<MockBackend>, n: u32| -> (u16, Vec<u8>) {
            let mut buf = [0u8; HC_PAGE];
            let len = hypercall_proto::encode_request(
                ServiceId::Entropy,
                1,
                1,
                &n.to_le_bytes(),
                &mut buf,
            )
            .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&buf[..len]);
            vmm.dispatch_out(DOORBELL_PORT, 4, len as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            (hdr.status, pl.to_vec())
        };
        let mut a = mk();
        let (status, bytes_a) = entropy(&mut a, 16);
        assert_eq!(status, Status::Ok as u16, "entropy is routed, not rejected");
        assert_eq!(bytes_a.len(), 16);
        let mut b = mk();
        assert_eq!(
            entropy(&mut b, 16).1,
            bytes_a,
            "entropy is deterministic per seed"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn state_hash_folds_the_sdk_stream_and_is_absent_when_unwired() {
        let unwired = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let unwired_hash = unwired.state_hash().unwrap();
        let mut wired = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        enable_nominal(&mut wired, 7);
        assert!(
            wired
                .state_blob()
                .unwrap()
                .windows(4)
                .any(|w| w == b"SDK\0")
        );
        assert_ne!(unwired_hash, wired.state_hash().unwrap());
    }

    #[test]
    fn rdtsc_completes_with_vtime_tsc_not_host() {
        let mut vmm = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            1,
        );
        assert!(vmm.vtime_wired(), "wire_vtime reports the path as wired");
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(vmm.backend.completions(), &[Completion::Read(2)]);
    }

    #[test]
    fn rdtscp_completes_with_vtime_tsc() {
        let mut vmm = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            1,
        );
        vmm.run().expect("run");
        assert_eq!(vmm.backend.completions(), &[Completion::Read(2)]);
    }

    #[test]
    fn save_vtime_is_none_when_unwired() {
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(v.save_vtime().unwrap().is_none());
        assert!(!v.vtime_wired());
    }

    #[test]
    fn save_vtime_anchors_vns_to_last_intercept_not_live_work() {
        let v = vtime_vmm(vec![], 1);
        let snap = v.save_vtime().expect("save").expect("wired");
        assert_eq!(
            snap.vns, 0,
            "vns must anchor to assigned_clock (0), not the live counter (777)"
        );
    }

    #[test]
    fn restore_vtime_rejects_bad_snapshot_atomically() {
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        let before = v.state_hash().unwrap();
        let bad = VtimeSnapshot {
            vns: 9_999,
            guest_clock_offset: 0,
            entropy: vec![0u8; 8],
        };
        assert!(matches!(
            v.restore_vtime(&bad),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(
            v.state_hash().unwrap(),
            before,
            "a rejected snapshot must leave the V-time/entropy state untouched"
        );
    }

    #[test]
    fn restore_vtime_does_not_touch_backend_save() {
        let mut v = Vmm::new(
            SaveFailBackend(configured_mock(vec![])),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        let snap0 = v.save_vtime().expect("clean save").expect("V-time wired");
        let snap = VtimeSnapshot {
            vns: snap0.vns + 4_096,
            guest_clock_offset: snap0.guest_clock_offset,
            entropy: snap0.entropy.clone(),
        };
        v.restore_vtime(&snap).expect("V-time-only restore");
        assert_eq!(v.effective_vns(), Some(snap.vns));
    }

    #[test]
    fn rdmsr_ia32_tsc_matches_rdtsc_instruction_and_is_deterministic() {
        let run_msr = || {
            let mut v = vtime_vmm(
                vec![
                    Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                    Exit::Common(CommonExit::Idle),
                ],
                1,
            );
            v.run().unwrap();
            v
        };
        let mut insn = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            1,
        );
        insn.run().unwrap();

        let msr = run_msr();
        assert_eq!(
            msr.backend.completions(),
            insn.backend.completions(),
            "RDMSR(IA32_TSC) must read the same V-time TSC as the RDTSC instruction"
        );
        assert_eq!(msr.backend.completions(), &[Completion::Read(2)]);
        assert_eq!(msr.state_hash().unwrap(), run_msr().state_hash().unwrap());
    }

    #[test]
    fn tsc_adjust_state_is_in_the_hash() {
        let with_adjust = |adjust: u64| {
            let mut v = vtime_vmm(
                vec![Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: adjust,
                })],
                1,
            );
            v.step().unwrap();
            v
        };
        assert_ne!(
            with_adjust(0).state_hash().unwrap(),
            with_adjust(12_345).state_hash().unwrap(),
            "a written IA32_TSC_ADJUST must change the VTIM hash"
        );
    }

    #[test]
    fn tsc_adjust_access_records_work_in_the_hash() {
        let at_vns = |vns: u64| {
            let mut v = vtime_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x3b })], 1);
            v.vtime.as_mut().unwrap().advance_virtual_time(vns);
            v.step().unwrap();
            v
        };
        assert_ne!(
            at_vns(100).state_hash().unwrap(),
            at_vns(200).state_hash().unwrap(),
            "a 0x3b access at different work ⇒ different effective V-time ⇒ different hash"
        );
    }

    #[test]
    fn vtime_snapshot_round_trips_tsc_adjust() {
        let mut v = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: 9,
                }),
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: 99,
                }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x3b }),
            ],
            1,
        );
        v.step().unwrap();
        let snap = v
            .save_vtime()
            .expect("save with non-zero adjust succeeds")
            .expect("wired");
        assert_eq!(
            snap.guest_clock_offset, 9,
            "snapshot must capture IA32_TSC_ADJUST"
        );
        v.step().unwrap();
        v.restore_vtime(&snap).expect("restore");
        v.step().unwrap();
        assert_eq!(
            v.backend.completions().last(),
            Some(&Completion::Read(9)),
            "restore must re-apply the snapshotted IA32_TSC_ADJUST"
        );
    }

    #[test]
    fn emulate_vtime_tsc_msr_unwired_fails_closed() {
        for idx in [0x10u32, 0x3b] {
            let mut rd = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Rdmsr { index: idx })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(matches!(rd.step(), Err(VmmError::ContractViolation(_))));
            let mut wr = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Wrmsr {
                    index: idx,
                    value: 0,
                })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(matches!(wr.step(), Err(VmmError::ContractViolation(_))));
        }
    }

    #[test]
    fn vtime_state_is_hashed_and_distinguishes_seed_and_vns_base() {
        fn contains_tag(blob: &[u8], tag: &[u8; 4]) -> bool {
            blob.windows(4).any(|w| w == tag)
        }
        fn wired(seed: u64, cfg: vtime::VClockConfig) -> Vmm<MockBackend> {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(cfg, seed).unwrap());
            v
        }

        let stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(
            !contains_tag(&stock.state_blob().unwrap(), b"VTIM"),
            "stock Vmm must not emit a VTIM chunk (M1/M2 hash unchanged)"
        );
        let stock2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(stock.state_hash().unwrap(), stock2.state_hash().unwrap());

        let a = wired(1, contract_vclock_config());
        assert!(contains_tag(&a.state_blob().unwrap(), b"VTIM"));
        assert_ne!(
            a.state_hash().unwrap(),
            stock.state_hash().unwrap(),
            "wiring vtime must change the hash"
        );

        let b = wired(2, contract_vclock_config());
        assert_ne!(
            a.state_hash().unwrap(),
            b.state_hash().unwrap(),
            "different seed ⇒ different state_hash"
        );

        let base = contract_vclock_config();
        let variants = [
            (
                "guest_hz",
                vtime::VClockConfig {
                    guest_hz: 3_000_000_000,
                    ..base
                },
            ),
            (
                "guest_base",
                vtime::VClockConfig {
                    guest_base: 5,
                    ..base
                },
            ),
            (
                "vns_base",
                vtime::VClockConfig {
                    vns_base: 12_345,
                    ..base
                },
            ),
        ];
        for (field, cfg) in variants {
            assert_ne!(
                a.state_hash().unwrap(),
                wired(1, cfg).state_hash().unwrap(),
                "different {field} ⇒ different state_hash"
            );
        }

        let a2 = wired(1, contract_vclock_config());
        assert_eq!(a.state_hash().unwrap(), a2.state_hash().unwrap());
    }

    #[test]
    fn vtime_hash_preimage_keeps_the_frozen_n1_prefix() {
        let wiring =
            VtimeWiring::new_virtual_time(contract_vclock_config(), 1).expect("valid wiring");
        let encoded = encode_vtime(&wiring);
        assert_eq!(encoded[0], 1, "historical assigned-clock marker");
        assert_eq!(&encoded[1..9], &1_u64.to_le_bytes(), "historical ratio");
    }

    fn report_out(value: u32) -> Exit<X86> {
        Exit::Arch(X86Exit::Io {
            port: REPORT_PORT,
            size: 4,
            write: Some(value),
        })
    }

    #[test]
    fn report_port_out_appends_values_in_order() {
        let mut vmm = Vmm::new(
            configured_mock(vec![
                report_out(0x1111_1111),
                report_out(0x0000_0000),
                report_out(0xDEAD_BEEF),
                report_out(0x0000_0001),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(
            vmm.report_stream(),
            [0x1111_1111, 0x0000_0000, 0xDEAD_BEEF, 0x0000_0001]
        );
        assert!(vmm.backend.completions().is_empty());
    }

    #[test]
    fn report_port_advances_the_paravirtual_exit_budget() {
        let mut vmm = vtime_vmm(vec![report_out(0xA5A5_5A5A)], 1);
        let before = vmm.effective_vns().unwrap();
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(
            vmm.effective_vns().unwrap() - before,
            crate::vendor::x86::contract::virtual_time_timing().paravirtual_device_mmio_vns
        );
        assert_eq!(vmm.report_stream(), [0xA5A5_5A5A]);
    }

    #[test]
    fn tick_port_advances_the_execution_tick_budget() {
        let mut vmm = vtime_vmm(
            vec![Exit::Arch(X86Exit::Io {
                port: VIRTUAL_TIME_TICK_PORT,
                size: 4,
                write: Some(1),
            })],
            1,
        );
        let before = vmm.effective_vns().unwrap();
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(
            vmm.effective_vns().unwrap() - before,
            crate::vendor::x86::contract::virtual_time_timing().execution_tick_vns
        );
        assert!(vmm.backend.completions().is_empty());
    }

    #[test]
    fn tick_port_rejects_non_protocol_accesses() {
        for (size, value) in [(1u8, 1u32), (2, 1), (4, 0), (4, 2)] {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Io {
                    port: VIRTUAL_TIME_TICK_PORT,
                    size,
                    write: Some(value),
                })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(
                matches!(vmm.step(), Err(VmmError::ContractViolation(_))),
                "tick access size {size} value {value} must fail closed"
            );
        }
    }

    #[test]
    fn off_protocol_tick_accesses_do_not_advance_virtual_time() {
        let writes = [(1u8, Some(1u32)), (2, Some(1)), (4, Some(0)), (4, Some(2))];
        for (size, write) in writes.into_iter().chain([(1, None), (4, None)]) {
            let mut vmm = vtime_vmm(
                vec![Exit::Arch(X86Exit::Io {
                    port: VIRTUAL_TIME_TICK_PORT,
                    size,
                    write,
                })],
                1,
            );
            let before = vmm.effective_vns().unwrap();
            assert!(
                matches!(vmm.step(), Err(VmmError::ContractViolation(_))),
                "tick access size {size} write {write:?} must fail closed"
            );
            assert_eq!(
                vmm.effective_vns().unwrap(),
                before,
                "rejected tick access size {size} write {write:?} must leave V-time unchanged"
            );
        }
    }

    #[test]
    fn report_port_non_dword_fails_closed() {
        for bad_size in [1u8, 2] {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Io {
                    port: REPORT_PORT,
                    size: bad_size,
                    write: Some(0xAB),
                })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(
                matches!(vmm.step(), Err(VmmError::ContractViolation(_))),
                "report write of size {bad_size} must fail closed"
            );
        }
    }

    #[test]
    fn observable_digest_tracks_report_stream_but_state_hash_does_not() {
        let mut a = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let b = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a.report_stream = vec![0xAA, 0xBB];
        assert_eq!(
            a.state_hash().unwrap(),
            b.state_hash().unwrap(),
            "report stream must NOT reach state_hash (M1/M2 hash unchanged)"
        );
        assert_ne!(
            a.observable_digest(),
            b.observable_digest(),
            "report stream MUST reach observable_digest"
        );
        let mut a2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a2.report_stream = vec![0xAA, 0xBB];
        assert_eq!(a.observable_digest(), a2.observable_digest());
        let mut a_rev = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a_rev.report_stream = vec![0xBB, 0xAA];
        assert_ne!(a.observable_digest(), a_rev.observable_digest());
    }

    #[test]
    fn observable_digest_also_covers_the_serial_banner() {
        let mut quiet = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let mut loud = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        for &byte in b"PAYLOAD x PASS\n" {
            loud.devices
                .uart
                .write(crate::vendor::x86::devices::UART_PORT_BASE, byte);
        }
        assert_ne!(quiet.observable_digest(), loud.observable_digest());
        quiet.report_stream = vec![u32::from_le_bytes(*b"PAYL")];
        assert_ne!(
            quiet.observable_digest(),
            loud.observable_digest(),
            "domain/length-prefixed digest separates the report stream from serial"
        );
    }

    #[test]
    fn state_components_breakdown_is_stable_and_covers_state() {
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let comps = v.state_components();
        assert_eq!(comps, v.state_components(), "pure: two calls agree");
        let labels: Vec<&str> = comps.iter().map(|(l, _)| *l).collect();
        for expect in [
            "RAM:0..64K",
            "regs",
            "segments",
            "control-regs",
            "msrs",
            "xsave-legacy",
            "xsave-header",
            "xsave-extended",
            "serial",
            "dev",
        ] {
            assert!(
                labels.contains(&expect),
                "missing component {expect}: {labels:?}"
            );
        }
        assert!(
            !labels.iter().any(|l| l.starts_with("vtim")),
            "no vtim components when unwired"
        );
        let mut w = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        w.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        let wlabels: Vec<&str> = w.state_components().iter().map(|(l, _)| *l).collect();
        for expect in ["vtim:cfg", "vtim:eff-vns", "vtim:entropy"] {
            assert!(wlabels.contains(&expect), "missing {expect}: {wlabels:?}");
        }
        let v2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(v.state_components(), v2.state_components());
        let mut v3 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        v3.report_stream = vec![0xDEAD_BEEF];
        assert_eq!(v.state_components(), v3.state_components());

        let mut raw_backend = configured_mock(vec![]);
        let raw_state = VcpuState {
            xsave_restore_bv: Some(3),
            ..Default::default()
        };
        raw_backend.set_state(raw_state);
        let raw = Vmm::new(raw_backend, GuestRam::new(0x1000).unwrap());
        assert!(
            raw.state_components()
                .iter()
                .any(|(label, _)| *label == "xsave-restore-bv")
        );
        let mut raw_two_backend = configured_mock(vec![]);
        raw_two_backend.set_state(VcpuState {
            xsave_restore_bv: Some(2),
            ..Default::default()
        });
        let raw_two = Vmm::new(raw_two_backend, GuestRam::new(0x1000).unwrap());
        let component = |vmm: &Vmm<MockBackend>, name| {
            vmm.state_components()
                .into_iter()
                .find(|(label, _)| *label == name)
                .unwrap_or_else(|| panic!("missing diagnostic component {name}"))
                .1
        };
        assert_eq!(
            component(&raw, "xsave-header"),
            component(&raw_two, "xsave-header"),
            "raw provenance stays out of the canonical XSAVE header component"
        );
        assert_ne!(
            component(&raw, "xsave-restore-bv"),
            component(&raw_two, "xsave-restore-bv"),
            "the diagnostic provenance component still distinguishes raw values"
        );
    }

    #[test]
    fn restored_and_fresh_at_same_effective_vtime_hash_identically() {
        const E: u64 = 4242;
        const SEED: u64 = 0x1234;

        let mut fresh = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]),
            GuestRam::new(0x1000).unwrap(),
        );
        let mut cfg = contract_vclock_config();
        cfg.vns_base = E - 1;
        fresh.wire_vtime(VtimeWiring::new_virtual_time(cfg, SEED).unwrap());
        fresh.step().unwrap();

        let mut restored = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        restored.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), SEED).unwrap());
        let snap = VtimeSnapshot {
            vns: E,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(SEED).save_state(),
        };
        restored.restore_vtime(&snap).unwrap();

        assert_eq!(
            fresh.state_hash().unwrap(),
            restored.state_hash().unwrap(),
            "a restored VM and a fresh VM at the same effective V-time must hash identically"
        );
    }

    fn linux_vmm(exits: Vec<Exit<X86>>) -> Vmm<MockBackend> {
        let mut v = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    #[test]
    fn x86_clockevent_trace_reports_only_an_armed_lapic_timer() {
        let mut vmm = linux_vmm(Vec::new());
        assert_eq!(<X86 as Vendor>::clockevent_trace_schedule(&vmm), None);

        let lapic = vmm.devices.lapic.as_mut().unwrap();
        lapic.mmio_write(lapic::APIC_SVR, 0x100, 0).unwrap();
        lapic.mmio_write(lapic::APIC_LVT_TIMER, 0x41, 0).unwrap();
        lapic.mmio_write(lapic::APIC_TMICT, 24_000, 0).unwrap();
        let expected = (lapic.next_timer_deadline().unwrap(), 0x41);
        assert_ne!(expected.0, 0);
        assert_eq!(
            <X86 as Vendor>::clockevent_trace_schedule(&vmm),
            Some(expected)
        );
    }

    #[test]
    fn apic_mmio_serviced_only_when_lapic_wired() {
        let mut v = linux_vmm(vec![
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            }),
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_00B0),
                size: 4,
                write: Some(0),
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        assert!(v.lapic_wired());
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(
            v.backend.completions(),
            &[Completion::Read(u64::from(lapic::APIC_VERSION_VALUE))]
        );

        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            })]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!stock.lapic_wired(), "stock Vmm has no xAPIC wired");
        assert!(matches!(stock.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn mmio_outside_apic_page_fails_closed_even_on_linux_path() {
        let mut v = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEB0_0000),
            size: 4,
            write: None,
        })]);
        assert!(matches!(v.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn legacy_io_serviced_only_when_wired() {
        let mut v = linux_vmm(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0x8000_0000),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x0CFC,
                size: 4,
                write: None,
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        v.run().expect("run");
        assert_eq!(v.backend.completions(), &[Completion::Read(0xFFFF_FFFF)]);

        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0),
            })]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(matches!(stock.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn linux_platform_state_in_hash_only_when_wired() {
        fn has(blob: &[u8], tag: &[u8; 4]) -> bool {
            blob.windows(4).any(|w| w == tag)
        }
        let stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let stock_blob = stock.state_blob().unwrap();
        assert!(!has(&stock_blob, b"LAPC"));
        assert!(!has(&stock_blob, b"LEGY"));

        let linux = linux_vmm(vec![]);
        let blob = linux.state_blob().unwrap();
        assert!(has(&blob, b"LAPC"));
        assert!(has(&blob, b"LEGY"));
        assert_ne!(stock.state_hash().unwrap(), linux.state_hash().unwrap());

        let with_pci = |addr: u32| {
            let mut v = linux_vmm(vec![Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(addr),
            })]);
            v.step().unwrap();
            v
        };
        assert_ne!(
            with_pci(0x1000).state_hash().unwrap(),
            with_pci(0x2000).state_hash().unwrap()
        );
    }

    #[test]
    fn serial_and_exit_counts_accessors_reflect_the_run() {
        let mut v = linux_vmm(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'i')),
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        v.run().expect("run");
        assert_eq!(v.serial(), b"Hi");
        assert!(v.exit_counts().io >= 2, "exit_counts reflects the IO exits");
    }

    #[test]
    fn mmio_just_past_apic_page_fails_closed() {
        let mut v = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_1000),
            size: 4,
            write: None,
        })]);
        assert!(matches!(v.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn lapic_timer_current_count_tracks_vtime() {
        let mut v = Vmm::new(
            configured_mock(vec![
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_00F0),
                    size: 4,
                    write: Some(0x1FF),
                }),
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0320),
                    size: 4,
                    write: Some(0x40),
                }),
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0380),
                    size: 4,
                    write: Some(0xFFFF_FFFF),
                }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0390),
                    size: 4,
                    write: None,
                }),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );

        v.run().expect("run");
        let tmcct = match v.backend.completions().last() {
            Some(Completion::Read(v)) => *v,
            other => panic!("expected a TMCCT read completion, got {other:?}"),
        };
        assert!(tmcct > 0, "timer is running (some count remains)");
        assert!(
            tmcct < 0xFFFF_FFFF,
            "TMCCT decreased from the armed initial count — lapic_now_vns advanced with V-time"
        );
    }

    #[test]
    fn lapic_register_state_is_in_the_hash() {
        let base = linux_vmm(vec![]);
        let mut modified = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0080),
            size: 4,
            write: Some(0x20),
        })]);
        modified.step().unwrap();
        assert_ne!(
            base.state_hash().unwrap(),
            modified.state_hash().unwrap(),
            "an xAPIC register write must change the LAPC hash chunk"
        );
    }

    fn configured_stock_mock(exits: Vec<Exit<X86>>) -> MockBackend {
        let mut m = MockBackend::with_capabilities(vmm_backend::Capabilities {
            name: "mock-stock",
            arch: X86Caps,
        });
        m.extend_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .expect("set_policy");
        m
    }

    fn arm_timer_exits(initial_count: u64) -> Vec<Exit<X86>> {
        let w = |off: u64, v: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(v),
            })
        };
        vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40),
            w(u64::from(lapic::APIC_TMICT), initial_count),
        ]
    }

    #[test]
    fn lapic_timer_delivers_off_intercept_anchor_on_deterministic_backend() {
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Arch(X86Exit::Rdmsr { index: 0x10 }));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits));

        v.run().expect("run");
        let reads = read_completions(&v);
        assert_eq!(
            reads.first().expect("ISR read A") & 1,
            0,
            "not delivered before the anchor advances (off the intercept anchor, not live work)"
        );
        assert_eq!(
            reads.last().expect("ISR read B") & 1,
            1,
            "delivered once the intercept anchor crosses the timer deadline"
        );
    }

    #[test]
    fn stale_vector_re_arbitrated_away_after_tpr_raise() {
        let tpr_write = Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TPR)),
            size: 4,
            write: Some(0xF0),
        });
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(tpr_write);
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);

        for _ in 0..5 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        v.backend.set_defer_accept(false);
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "the stale, now-masked vector was re-arbitrated out of the pending slot"
        );
        assert_eq!(
            v.backend.take_accepted_interrupt(),
            None,
            "the stale vector was never accepted (KVM_INTERRUPT not issued for it)"
        );
        let reads = read_completions(&v);
        assert_eq!(
            reads.last().expect("IRR read") & 1,
            1,
            "0x40 is retained in IRR (masked by TPR, not dropped)"
        );
    }

    #[test]
    fn no_injection_when_lapic_unwired() {
        let mut v = Vmm::new(
            configured_mock(vec![Exit::Common(CommonExit::Idle)]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!v.lapic_wired());
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert!(
            v.backend.injected().is_empty(),
            "an unwired LAPIC never drives an injection"
        );
    }

    const UNMASK_IRQ4: Exit<X86> = Exit::Arch(X86Exit::Io {
        port: 0x0021,
        size: 1,
        write: Some(0xEF),
    });
    const ENABLE_THRI: Exit<X86> = Exit::Arch(X86Exit::Io {
        port: 0x03F9,
        size: 1,
        write: Some(0x02),
    });

    #[test]
    fn serial_thre_interrupt_injects_com1_vector() {
        let mut mock = configured_mock(vec![
            UNMASK_IRQ4,
            ENABLE_THRI,
            Exit::Common(CommonExit::Idle),
        ]);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);

        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "no THRE interrupt before IER.THRI"
        );
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            Some(COM1_IRQ_VECTOR),
            "the THRE interrupt is injected on the legacy COM1 vector 0x34"
        );
        assert_eq!(COM1_IRQ_VECTOR, 0x34, "ISA_IRQ_VECTOR(4) = 0x30 + 4");
    }

    #[test]
    fn serial_irq_suppressed_while_8259_masks_it() {
        let mut mock = configured_mock(vec![ENABLE_THRI, Exit::Common(CommonExit::Idle)]);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "a masked COM1 line is not injected even with THRE asserted"
        );
    }

    #[test]
    fn lapic_vector_outranks_the_serial_line() {
        let mut exits = arm_timer_exits(1);
        exits.push(UNMASK_IRQ4);
        exits.push(ENABLE_THRI);
        exits.push(Exit::Arch(X86Exit::Rdmsr { index: 0x10 }));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);
        v.run().expect("run");
        assert_eq!(
            v.backend.pending_irq(),
            Some(0x40),
            "the LAPIC timer vector outranks the legacy serial ExtINT line"
        );
    }

    #[test]
    fn serial_acceptance_takes_no_lapic_isr_transition() {
        let mut exits = vec![UNMASK_IRQ4, ENABLE_THRI];
        exits.push(read_mmio(isr_gpa(COM1_IRQ_VECTOR)));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits));
        v.run().expect("run");
        let isr = *read_completions(&v).last().expect("ISR read");
        assert_eq!(
            isr & (1 << (u32::from(COM1_IRQ_VECTOR) % 32)),
            0,
            "the serial vector never enters the LAPIC ISR (EOI'd at the 8259)"
        );
    }

    fn isr_gpa(v: u8) -> Gpa {
        Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_ISR) + u64::from(v / 32) * 0x10)
    }
    fn irr_gpa(v: u8) -> Gpa {
        Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_IRR) + u64::from(v / 32) * 0x10)
    }
    fn read_mmio(gpa: Gpa) -> Exit<X86> {
        Exit::Common(CommonExit::Mmio {
            gpa,
            size: 4,
            write: None,
        })
    }

    fn read_completions(v: &Vmm<MockBackend>) -> Vec<u64> {
        v.backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect()
    }

    fn lapic_vmm(mock: MockBackend) -> Vmm<MockBackend> {
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    fn virtual_time_lapic_vmm(mock: MockBackend) -> Vmm<MockBackend> {
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    #[test]
    fn virtual_time_lapic_timer_records_schedule_and_delivery() {
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Shutdown));
        let mut v = virtual_time_lapic_vmm(configured_mock(exits));
        v.run().expect("run");

        let trace = v.virtual_time_trace().expect("virtual_time trace wired");
        let schedule = trace.schedule();
        assert_eq!(schedule.len(), 1, "the one-shot arm is one schedule record");
        assert_eq!(schedule[0].interrupt_id, 0x40);
        assert_eq!(schedule[0].canceled_at_event, None);
        let log = trace.normalized_log();
        crate::virtual_time::check_delivery_placement(schedule, log)
            .expect("the delivery sits at the first event whose V-time covers the deadline");
        let delivery_events: Vec<u64> = log
            .events
            .iter()
            .filter(|e| !e.interrupts.is_empty())
            .map(|e| e.event_index)
            .collect();
        assert_eq!(
            delivery_events,
            vec![3],
            "fired inside the crossing (ISR-read) event"
        );
    }

    #[test]
    fn virtual_time_lapic_timer_disarm_cancels_the_schedule() {
        let mut exits = arm_timer_exits(1_000_000);
        exits.push(Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TMICT)),
            size: 4,
            write: Some(0),
        }));
        exits.push(Exit::Common(CommonExit::Shutdown));
        let mut v = virtual_time_lapic_vmm(configured_mock(exits));
        v.run().expect("run");

        let trace = v.virtual_time_trace().expect("virtual_time trace wired");
        let schedule = trace.schedule();
        assert_eq!(schedule.len(), 1);
        assert_eq!(
            schedule[0].canceled_at_event,
            Some(3),
            "the TMICT=0 event canceled it"
        );
        crate::virtual_time::check_delivery_placement(schedule, trace.normalized_log())
            .expect("a canceled deadline needs no delivery");
    }

    #[test]
    fn injected_vector_stays_in_irr_until_accepted() {
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);

        v.run().expect("run");
        let reads: Vec<u64> = v
            .backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect();
        let isr = *reads.last().expect("ISR read");
        let irr = reads[reads.len() - 2];
        assert_eq!(irr & 1, 1, "vector 0x40 is pending in IRR while deferred");
        assert_eq!(
            isr & 1,
            0,
            "vector 0x40 is NOT in service before acceptance"
        );
    }

    #[test]
    fn accepted_vector_moves_irr_to_isr() {
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits));

        v.run().expect("run");
        let reads: Vec<u64> = v
            .backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect();
        let isr = *reads.last().expect("ISR read");
        let irr = reads[reads.len() - 2];
        assert_eq!(irr & 1, 0, "IRR bit cleared once the vector is accepted");
        assert_eq!(isr & 1, 1, "vector 0x40 is in service after acceptance");
    }

    fn if_set_state() -> VcpuState {
        VcpuState {
            regs: vmm_backend::VcpuRegs {
                rflags: RFLAGS_IF | 0x2,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn idle_hlt_without_if_is_terminal() {
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(
            v.idle_landings().is_empty(),
            "an IF==0 HLT is terminal, never resumed"
        );
    }

    #[test]
    fn hlt_without_armed_timer_is_terminal_even_with_if() {
        let mut mock = configured_mock(vec![Exit::Common(CommonExit::Idle)]);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock);
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(v.idle_landings().is_empty(), "no armed timer ⇒ terminal");
    }

    #[test]
    fn idle_hlt_on_stock_backend_is_terminal() {
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_stock_mock(exits);
        mock.set_state(if_set_state());
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(
            v.idle_landings().is_empty(),
            "a non-deterministic backend never idle-resumes"
        );
    }

    #[test]
    fn idle_hlt_with_undeliverable_timer_is_terminal() {
        let w = |off: u64, val: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(val),
            })
        };
        let undeliverable_timer_hlt_terminates = |setup: Vec<Exit<X86>>| {
            let mut exits = setup;
            exits.push(Exit::Common(CommonExit::Idle));
            let mut mock = configured_mock(exits);
            mock.set_state(if_set_state());
            let mut v = lapic_vmm(mock);
            let r = v.run().expect("run");
            assert_eq!(
                r.reason,
                TerminalReason::Idle,
                "an armed-but-undeliverable timer HLT is terminal"
            );
            assert!(
                v.idle_landings().is_empty(),
                "no idle resume / no V-time advance for an undeliverable timer"
            );
        };

        undeliverable_timer_hlt_terminates(vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x05),
            w(u64::from(lapic::APIC_TMICT), 1000),
        ]);
        undeliverable_timer_hlt_terminates(vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40),
            w(u64::from(lapic::APIC_TMICT), 1000),
            w(u64::from(lapic::APIC_TPR), 0xF0),
        ]);
    }

    #[test]
    fn idle_discriminator_save_error_fails_closed() {
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut inner = configured_mock(exits);
        inner.set_state(if_set_state());
        let mut v = Vmm::new(SaveFailBackend(inner), GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );

        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let err = v.step().unwrap_err();
        assert!(
            matches!(err, VmmError::Backend(_)),
            "a save error during the idle discriminator must fail closed, got {err:?}"
        );
    }

    fn full_vmm(
        state: VcpuState,
        exits: Vec<Exit<X86>>,
        _retired_initial_work: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut m = configured_mock(exits);
        m.set_state(state);
        let mut v = Vmm::new(m, GuestRam::new(0x2000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    fn nonzero_state() -> VcpuState {
        let mut msrs = std::collections::BTreeMap::new();
        msrs.insert(0xC000_0080u32, 0x501);
        VcpuState {
            regs: vmm_backend::VcpuRegs {
                rax: 0x1111,
                rbx: 0x2222,
                rip: 0x10_0000,
                rsp: 0x8000,
                rflags: 0x2,
                ..Default::default()
            },
            sregs: vmm_backend::VcpuSregs {
                cs: vmm_backend::Segment {
                    selector: 0x10,
                    limit: 0xFFFF_FFFF,
                    type_: 0xB,
                    present: 1,
                    s: 1,
                    l: 1,
                    g: 1,
                    ..Default::default()
                },
                cr0: 0x8000_0011,
                cr3: 0x1000,
                cr4: 0x20,
                efer: 0x500,
                apic_base: 0xFEE0_0900,
                ..Default::default()
            },
            xcr0: 0x7,
            msrs,
            xsave: (0u16..512).map(|i| i as u8).collect(),
            ..Default::default()
        }
    }

    fn mutate_exits() -> Vec<Exit<X86>> {
        vec![
            Exit::Arch(X86Exit::Wrmsr {
                index: 0x3b,
                value: 0x1234,
            }),
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x20),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x0021,
                size: 1,
                write: Some(0xEF),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x3f8,
                size: 1,
                write: Some(b'X'.into()),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
        ]
    }

    fn step_n(v: &mut Vmm<MockBackend>, n: usize) {
        for _ in 0..n {
            assert_eq!(v.step().unwrap(), Step::Continued);
        }
    }

    #[test]
    fn save_vm_state_round_trips_through_the_codec() {
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let s = a.save_vm_state().expect("clean synchronized boundary");
        let bytes = s.encode().expect("encodable (ratio_den == 1)");
        assert_eq!(vm_state::VmState::decode(&bytes).unwrap(), s);
        assert_eq!(s.regs.rax, 0x1111);
        assert_eq!(s.vtime.snapshot_vns, 40002);
        assert_eq!(
            s.contract_hash,
            crate::vendor::x86::contract::contract_hash()
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn restore_vm_state_reproduces_the_blob_byte_for_byte() {
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();

        let mut b = full_vmm(VcpuState::default(), vec![], 9999, 0x0000);
        b.restore_vm_state(&s).expect("restore");
        let s2 = b.save_vm_state().expect("re-save after restore");
        assert_eq!(s, s2, "restore-then-save must reproduce the snapshot blob");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn restore_vm_state_rejects_a_different_contract_atomically() {
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        let version_5_hash = [
            0x01, 0xb0, 0x21, 0x4b, 0x93, 0x87, 0xe2, 0x05, 0xe4, 0xc3, 0xdd, 0x41, 0x87, 0x80,
            0xbb, 0xe1, 0x5c, 0x77, 0xf1, 0x71, 0x25, 0x91, 0x20, 0xc2, 0xf7, 0x90, 0x2a, 0xe3,
            0x88, 0x58, 0xff, 0x63,
        ];
        for rejected_hash in [version_5_hash, [0xFFu8; 32]] {
            s.contract_hash = rejected_hash;

            let mut b = full_vmm(nonzero_state(), vec![], 100, 0xABCD);
            let before = b.state_hash().unwrap();
            assert!(matches!(
                b.restore_vm_state(&s),
                Err(VmmError::Snapshot(
                    crate::snapshot::SnapshotError::ContractMismatch
                ))
            ));
            assert_eq!(
                b.state_hash().unwrap(),
                before,
                "a rejected snapshot leaves the VM fully intact (atomic)"
            );
        }
    }

    #[test]
    fn restore_trace_failure_is_classified_after_commit() {
        let mut source = vtime_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 7);
        source.step().unwrap();
        let snapshot = source.save_vm_state().unwrap();

        let mut target = vtime_vmm(Vec::new(), 7);
        target
            .virtual_time_trace
            .as_mut()
            .unwrap()
            .begin(
                ExitReason::Rdmsr,
                "planted active event".to_owned(),
                NormalizedEventClass::TimeRead,
                Vec::new(),
            )
            .unwrap();
        assert!(matches!(
            target.restore_vm_state(&snapshot),
            Err(VmmError::Backend(vmm_backend::BackendError::Internal(message)))
                if message.contains("during an active event")
        ));
    }

    #[test]
    fn reseed_entropy_requires_a_wired_stream() {
        let mut stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock.reseed_entropy(7),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    fn platform_entropy_reseed_does_not_reset_service_or_scheduler_state() {
        use environment::channel::Question;

        const SERVICE_SEED: u64 = 0x5151;
        let scheduler = Question::scheduler(3).unwrap();
        let service = Question::with_request_id(7, 99, vec![0xA5]).unwrap();

        let mut expected = channel::RecordedEnv::new(
            SERVICE_SEED,
            Box::new(SeededService::default()) as Box<dyn channel::ServiceHandler>,
        );
        expected.set_moment(11);
        let mut actual = vtime_vmm(Vec::new(), 0xCAFE);
        enable_seeded_service(&mut actual, SERVICE_SEED);
        actual.sdk.as_mut().expect("SDK enabled").env.set_moment(11);

        assert_eq!(
            actual.sdk.as_mut().unwrap().env.decide(&scheduler).unwrap(),
            expected.decide(&scheduler).unwrap()
        );
        assert_eq!(
            actual.sdk.as_mut().unwrap().env.decide(&service).unwrap(),
            expected.decide(&service).unwrap()
        );
        let before = actual
            .sdk
            .as_ref()
            .unwrap()
            .env
            .snapshot_state()
            .unwrap()
            .encode();
        let expected_after_draw = expected.snapshot_state().unwrap().encode();
        assert_eq!(before, expected_after_draw);

        actual.reseed_entropy(0xDEAD_BEEF).unwrap();
        let after = actual
            .sdk
            .as_ref()
            .unwrap()
            .env
            .snapshot_state()
            .unwrap()
            .encode();
        assert_eq!(
            after, before,
            "platform entropy reseed must not reset the workload scheduler stream or handler state"
        );
    }

    #[test]
    fn branch_input_spec_materializes_handler_with_requested_seed() {
        use environment::input_spec::{InputSpec, ServiceFactory};
        use std::sync::Arc;

        const REQUESTED_SEED: u64 = 0xBEEF_CAFE;
        let config = SeededService::config();
        let mut spec = InputSpec::seeded(REQUESTED_SEED);
        spec.set_config(config.clone());
        let factory: ServiceFactory = Arc::new(move |requested| {
            if requested != &config {
                return Err(channel::ChannelError::Handler(
                    "unexpected service configuration".to_owned(),
                ));
            }
            Ok(Box::new(SeededService::default()))
        });

        let env = spec.materialize(&factory).unwrap();
        let mut expected_state = Vec::new();
        expected_state.extend_from_slice(&REQUESTED_SEED.to_le_bytes());
        expected_state.extend_from_slice(&0_u64.to_le_bytes());
        assert_eq!(
            env.handler().snapshot_state().unwrap(),
            expected_state,
            "branch InputSpec seed initializes workload handler state"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn restore_vm_state_rejects_a_clock_rate_mismatch() {
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();
        let reject = |bad: &vm_state::VmState, name: &str| {
            let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
            assert!(
                matches!(b.restore_vm_state(bad), Err(VmmError::ContractViolation(_))),
                "a {name} clock-rate mismatch must be rejected"
            );
        };
        let mut bad = s.clone();
        bad.vtime.guest_hz += 1;
        reject(&bad, "guest_hz");
        let mut bad = s.clone();
        bad.vtime.guest_base += 1;
        reject(&bad, "guest_base");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn restore_into_unwired_vm_rejects_a_vtime_bearing_blob() {
        let mut a = vtime_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 1);
        a.step().unwrap();
        let s = a.save_vm_state().unwrap();
        assert!(
            s.vtime.guest_hz != 0,
            "source blob carries a live V-time block"
        );

        let mut only_hz = s.clone();
        only_hz.vtime.snapshot_vns = 0;
        let mut stock1 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock1.restore_vm_state(&only_hz),
            Err(VmmError::ContractViolation(_))
        ));

        let mut only_vns = s.clone();
        only_vns.vtime.guest_hz = 0;
        only_vns.vtime.snapshot_vns = 7;
        let mut stock2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock2.restore_vm_state(&only_vns),
            Err(VmmError::ContractViolation(_))
        ));
    }

    struct SaveFailBackend(MockBackend);
    impl Backend for SaveFailBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region
            // (no dereference); this adds no obligation beyond the trait contract.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            self.0.run()
        }
        fn inject(&mut self, e: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.0.inject(e)
        }
        fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
            self.0.set_pending_irq(v)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.0.take_accepted_interrupt()
        }
        fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
            self.0.complete_read(v)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.0.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.0.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<VcpuState> {
            Err(vmm_backend::BackendError::Memory("induced save failure"))
        }
        fn restore(&mut self, s: &VcpuState) -> vmm_backend::Result<()> {
            self.0.restore(s)
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.0.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.0.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.0.capabilities()
        }
    }

    struct ContinuationBackend {
        inner: MockBackend,
        ordinary: VecDeque<Exit<X86>>,
        continuation_scripts: VecDeque<VecDeque<Exit<X86>>>,
        current_continuations: VecDeque<Exit<X86>>,
        ordinary_runs: usize,
        finish_runs: usize,
        mapped: Vec<(Gpa, usize)>,
    }

    impl ContinuationBackend {
        fn new(ordinary: Vec<Exit<X86>>, continuations: Vec<Vec<Exit<X86>>>) -> Self {
            assert_eq!(
                ordinary.len(),
                continuations.len(),
                "each ordinary entry needs one continuation script"
            );
            Self {
                inner: configured_mock(Vec::new()),
                ordinary: ordinary.into(),
                continuation_scripts: continuations.into_iter().map(VecDeque::from).collect(),
                current_continuations: VecDeque::new(),
                ordinary_runs: 0,
                finish_runs: 0,
                mapped: Vec::new(),
            }
        }
    }

    impl Backend for ContinuationBackend {
        type A = X86;

        fn set_policy(&mut self, policy: &X86Policy) -> vmm_backend::Result<()> {
            self.inner.set_policy(policy)
        }

        unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> vmm_backend::Result<()> {
            self.mapped.push((gpa, host.len()));
            Ok(())
        }

        fn run(&mut self) -> vmm_backend::Result<Exit<X86>> {
            if !self.current_continuations.is_empty() {
                return Err(vmm_backend::BackendError::PendingCompletion);
            }
            let exit = self
                .ordinary
                .pop_front()
                .ok_or(vmm_backend::BackendError::Internal(
                    "continuation test ordinary run queue empty",
                ))?;
            let continuations = self.continuation_scripts.pop_front().ok_or(
                vmm_backend::BackendError::Internal("continuation test script queue empty"),
            )?;
            self.current_continuations = continuations;
            self.ordinary_runs += 1;
            self.inner.push_exit(exit);
            self.inner.run()
        }

        fn inject(&mut self, event: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.inner.inject(event)
        }

        fn set_pending_irq(&mut self, id: Option<u8>) -> vmm_backend::Result<()> {
            self.inner.set_pending_irq(id)
        }

        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.inner.take_accepted_interrupt()
        }

        fn complete_read(&mut self, value: u64) -> vmm_backend::Result<()> {
            self.inner.complete_read(value)
        }

        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_fault()
        }

        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_ok()
        }

        fn complete_hypercall(&mut self, ret: u64) -> vmm_backend::Result<()> {
            self.inner.complete_hypercall(ret)
        }

        fn complete_arch(
            &mut self,
            completion: vmm_backend::X86Completion,
        ) -> vmm_backend::Result<()> {
            self.inner.complete_arch(completion)
        }

        fn finish_exit(&mut self) -> vmm_backend::Result<Option<Exit<X86>>> {
            let Some(exit) = self.current_continuations.pop_front() else {
                return Ok(None);
            };
            self.finish_runs += 1;
            self.inner.push_exit(exit);
            Ok(Some(self.inner.run()?))
        }

        fn retire_pending_completion(&mut self) -> vmm_backend::Result<()> {
            if !self.current_continuations.is_empty() {
                return Err(vmm_backend::BackendError::PendingCompletion);
            }
            self.inner.retire_pending_completion()
        }

        fn save(&self) -> vmm_backend::Result<VcpuState> {
            if !self.current_continuations.is_empty() || self.inner.has_pending() {
                return Err(vmm_backend::BackendError::PendingCompletion);
            }
            self.inner.save()
        }

        fn restore(&mut self, state: &VcpuState) -> vmm_backend::Result<()> {
            if !self.current_continuations.is_empty() || self.inner.has_pending() {
                return Err(vmm_backend::BackendError::PendingCompletion);
            }
            self.inner.restore(state)
        }

        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.inner.exit_counts()
        }

        fn reset_exit_counts(&mut self) {
            self.inner.reset_exit_counts()
        }

        fn capabilities(&self) -> vmm_backend::Capabilities<X86Caps> {
            self.inner.capabilities()
        }
    }

    fn continuation_vmm(
        ordinary: Vec<Exit<X86>>,
        continuations: Vec<Vec<Exit<X86>>>,
        seed: u64,
    ) -> Vmm<ContinuationBackend> {
        let mut vmm = Vmm::new(
            ContinuationBackend::new(ordinary, continuations),
            GuestRam::new(0x2000).unwrap(),
        );
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        vmm
    }

    fn boxed_continuation_vmm(
        ordinary: Vec<Exit<X86>>,
        continuations: Vec<Vec<Exit<X86>>>,
        seed: u64,
    ) -> Vmm<Box<ContinuationBackend>> {
        let mut vmm = Vmm::new(
            Box::new(ContinuationBackend::new(ordinary, continuations)),
            GuestRam::new(0x2000).unwrap(),
        );
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        vmm
    }

    fn apic_read(offset: u32) -> Exit<X86> {
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(offset)),
            size: 4,
            write: None,
        })
    }

    fn apic_write(offset: u32, value: u64) -> Exit<X86> {
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(offset)),
            size: 4,
            write: Some(value),
        })
    }

    #[test]
    fn fragmented_lapic_mmio_is_drained_without_an_extra_outer_run() {
        let mut source = boxed_continuation_vmm(
            vec![apic_read(lapic::APIC_VERSION)],
            vec![vec![apic_write(lapic::APIC_TPR, 0x20)]],
            7,
        );
        source.wire_snapshot_hashing();

        assert_eq!(source.step().unwrap(), Step::Continued);
        assert_eq!(source.backend.as_ref().ordinary_runs, 1);
        assert_eq!(source.backend.as_ref().finish_runs, 1);
        assert!(source.backend.as_ref().ordinary.is_empty());
        assert_eq!(source.exit_counts().mmio, 2);
        assert_eq!(
            source.backend.as_ref().inner.completions(),
            &[Completion::Read(u64::from(lapic::APIC_VERSION_VALUE))],
            "the read fragment receives the APIC version response"
        );
        assert_eq!(
            source.devices.lapic.as_ref().unwrap().snapshot().tpr,
            0x20,
            "the write fragment updates the LAPIC"
        );

        let per_mmio =
            crate::vendor::x86::contract::virtual_time_timing().interrupt_controller_mmio_vns;
        assert_eq!(source.effective_vns(), Some(per_mmio * 2));
        let counts = source.exit_counts();
        let vns = source.effective_vns();
        let outer_runs = source.backend.as_ref().ordinary_runs;
        let finish_runs = source.backend.as_ref().finish_runs;
        let hash = source.state_hash().unwrap();
        let bytes = source.save_vm_state().unwrap().encode().unwrap();
        assert_eq!(source.state_hash().unwrap(), hash);
        assert_eq!(source.save_vm_state().unwrap().encode().unwrap(), bytes);
        assert_eq!(source.exit_counts(), counts);
        assert_eq!(source.effective_vns(), vns);
        assert_eq!(source.backend.as_ref().ordinary_runs, outer_runs);
        assert_eq!(source.backend.as_ref().finish_runs, finish_runs);

        let decoded = vm_state::VmState::decode(&bytes).unwrap();
        let memory = source.guest_memory().to_vec();
        let mut cold = boxed_continuation_vmm(vec![], vec![], 7);
        cold.wire_snapshot_hashing();
        cold.restore_snapshot(&memory, &decoded).unwrap();
        assert_eq!(cold.state_hash().unwrap(), hash);
        assert_eq!(cold.save_vm_state().unwrap().encode().unwrap(), bytes);
        assert_eq!(cold.backend.as_ref().ordinary_runs, 0);
        assert_eq!(cold.backend.as_ref().finish_runs, 0);
        assert_eq!(cold.exit_counts(), vmm_backend::ExitCounts::default());
        assert_eq!(cold.effective_vns(), vns);
        assert_eq!(
            cold.devices.lapic.as_ref().unwrap().snapshot(),
            source.devices.lapic.as_ref().unwrap().snapshot()
        );
    }

    #[test]
    fn snapshot_fails_while_a_continuation_remains_queued() {
        let mut vmm = continuation_vmm(
            vec![apic_read(lapic::APIC_VERSION)],
            vec![vec![apic_write(lapic::APIC_TPR, 0x20)]],
            7,
        );
        vmm.backend.run().unwrap();
        assert!(matches!(
            vmm.save_vm_state(),
            Err(VmmError::Backend(
                vmm_backend::BackendError::PendingCompletion
            ))
        ));
        assert!(matches!(
            vmm.state_hash(),
            Err(VmmError::Backend(
                vmm_backend::BackendError::PendingCompletion
            ))
        ));
    }

    fn fragmented_checkpoint_script() -> (Vec<Exit<X86>>, Vec<Vec<Exit<X86>>>) {
        let mut ordinary = Vec::with_capacity(256);
        let mut continuations = Vec::with_capacity(256);
        for value in 0..255u32 {
            ordinary.push(Exit::Arch(X86Exit::Io {
                port: REPORT_PORT,
                size: 4,
                write: Some(value),
            }));
            continuations.push(Vec::new());
        }
        ordinary.push(apic_read(lapic::APIC_VERSION));
        continuations.push(vec![apic_write(lapic::APIC_TPR, 0x20)]);
        (ordinary, continuations)
    }

    #[test]
    fn deferred_checkpoint_lands_on_the_final_fragment_and_resets_with_trace_segments() {
        let (ordinary, continuations) = fragmented_checkpoint_script();
        let mut synchronous = boxed_continuation_vmm(ordinary.clone(), continuations.clone(), 7);
        synchronous.wire_snapshot_hashing();
        for _ in 0..255 {
            assert_eq!(synchronous.step().unwrap(), Step::Continued);
        }
        assert_eq!(synchronous.step().unwrap(), Step::Continued);
        let synchronous_trace = synchronous.virtual_time_trace().unwrap();
        assert_eq!(synchronous_trace.normalized_log().events.len(), 257);
        assert_eq!(
            synchronous_trace.normalized_log().events[255].state_hash,
            None,
            "the first fragment is not a complete checkpoint boundary"
        );
        let expected = synchronous
            .virtual_time_trace()
            .unwrap()
            .normalized_log()
            .events[256]
            .state_hash
            .expect("the final fragment owns the synchronous checkpoint");

        let mut deferred = boxed_continuation_vmm(ordinary, continuations, 7);
        deferred.wire_snapshot_hashing();
        deferred.defer_virtual_time_checkpoint_hashes().unwrap();
        for _ in 0..255 {
            assert_eq!(deferred.step().unwrap(), Step::Continued);
        }
        assert_eq!(deferred.step().unwrap(), Step::Continued);
        let trace = deferred.virtual_time_trace().unwrap();
        assert_eq!(trace.normalized_log().events.len(), 257);
        assert_eq!(trace.normalized_log().events[255].state_hash, None);
        assert_eq!(trace.normalized_log().events[256].state_hash, None);
        assert!(!deferred.virtual_time_checkpoint_due(255));
        assert!(deferred.virtual_time_checkpoint_due(256));
        assert_eq!(deferred.state_hash().unwrap(), expected);
        deferred
            .checkpoint_virtual_time_trace_at(256, expected)
            .unwrap();
        assert_eq!(
            deferred
                .virtual_time_trace()
                .unwrap()
                .normalized_log()
                .events[256]
                .state_hash,
            Some(expected)
        );
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(255, expected)
                .is_err()
        );

        let completed_segment = deferred.take_virtual_time_trace().unwrap();
        assert_eq!(completed_segment.normalized_log().events.len(), 257);
        assert!(!deferred.virtual_time_checkpoint_due(256));
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(256, expected)
                .is_err()
        );
        assert!(
            deferred
                .virtual_time_trace()
                .unwrap()
                .normalized_log()
                .events
                .is_empty()
        );
    }

    #[test]
    fn save_vm_state_fails_closed_on_backend_save_error() {
        let v = Vmm::new(
            SaveFailBackend(configured_mock(vec![])),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(
            matches!(v.save_vm_state(), Err(VmmError::Backend(_))),
            "a failing Backend::save must make save_vm_state fail closed"
        );
        assert!(
            matches!(v.state_hash(), Err(VmmError::Backend(_))),
            "a failing register read must not produce a hash of default CPU state"
        );
    }

    #[test]
    fn terminal_snapshots_preserve_the_stop_without_guest_reentry() {
        for (exit, reason) in [
            (
                Exit::Arch(X86Exit::Io {
                    port: 0xf4,
                    size: 1,
                    write: Some(7),
                }),
                TerminalReason::DebugExit { code: 7 },
            ),
            (Exit::Common(CommonExit::Idle), TerminalReason::Idle),
            (Exit::Common(CommonExit::Shutdown), TerminalReason::Shutdown),
        ] {
            let mut cpu = nonzero_state();
            cpu.regs.rflags = 2;
            let mut source = full_vmm(cpu.clone(), vec![exit], 0, 7);
            source.wire_snapshot_hashing();
            assert_eq!(source.step().unwrap(), Step::Terminal(reason));
            let hash = source.state_hash().unwrap();
            let at = source.effective_vns();
            let counts = source.exit_counts();
            let memory = source.guest_memory().to_vec();
            let encoded = source.save_vm_state().unwrap().encode().unwrap();
            assert_eq!(source.state_hash().unwrap(), hash);
            assert_eq!(source.effective_vns(), at);
            assert_eq!(source.exit_counts(), counts);
            assert_eq!(source.save_vm_state().unwrap().encode().unwrap(), encoded);
            let snapshot = vm_state::VmState::decode(&encoded).unwrap();

            let next_exit = Exit::Arch(X86Exit::Io {
                port: 0x3f8,
                size: 1,
                write: Some(b'X'.into()),
            });
            let mut cold = full_vmm(cpu, vec![next_exit], 0, 7);
            cold.wire_snapshot_hashing();
            let runnable = cold.save_vm_state().unwrap();
            cold.restore_snapshot(&memory, &snapshot).unwrap();
            assert_eq!(cold.state_hash().unwrap(), hash);
            let cold_counts = cold.exit_counts();
            for _ in 0..3 {
                assert_eq!(source.step().unwrap(), Step::Terminal(reason));
                assert_eq!(cold.step().unwrap(), Step::Terminal(reason));
                assert_eq!(cold.exit_counts(), cold_counts);
                assert_eq!(cold.effective_vns(), at);
                assert_eq!(cold.state_hash().unwrap(), hash);
                assert_eq!(source.state_hash().unwrap(), hash);
            }
            cold.restore_vm_state(&runnable).unwrap();
            assert_eq!(cold.step().unwrap(), Step::Continued);
            assert_eq!(cold.serial_output(), b"X");
        }
    }

    #[test]
    fn lifecycle_restore_rejects_malformed_state_before_mutation() {
        let source = full_vmm(nonzero_state(), vec![], 0, 7);
        let mut snapshot = source.save_vm_state().unwrap();
        snapshot.set_engine_state(vec![b'V', b'M', b'E', 1, 4, 0, 0]);
        let mut destination = full_vmm(VcpuState::default(), vec![], 0, 9);
        let before = destination.state_hash().unwrap();
        assert!(matches!(
            destination.restore_vm_state(&snapshot),
            Err(VmmError::Snapshot(
                crate::snapshot::SnapshotError::EngineState(_)
            ))
        ));
        assert_eq!(destination.state_hash().unwrap(), before);
    }

    #[test]
    fn deferred_sdk_reentry_is_part_of_snapshot_and_hash_identity() {
        let build = || {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Io {
                    port: 0x3f8,
                    size: 1,
                    write: Some(b'X'.into()),
                })]),
                GuestRam::new(TEST_RAM).unwrap(),
            );
            enable_nominal(&mut vmm, 7);
            vmm
        };
        let mut source = build();
        source.sdk.as_mut().unwrap().pending_snapshot = true;
        let ready_hash = source.state_hash().unwrap();
        source.sdk_snapshot_reentry_required = true;
        let deferred_hash = source.state_hash().unwrap();
        assert_ne!(ready_hash, deferred_hash);
        let sdk = source.sdk_snapshot().unwrap().unwrap();
        let snapshot = source.save_vm_state().unwrap();
        let snapshot = vm_state::VmState::decode(&snapshot.encode().unwrap()).unwrap();
        let mut cold = build();
        cold.restore_snapshot(source.guest_memory(), &snapshot)
            .unwrap();
        cold.sdk_restore(&sdk).unwrap();
        assert_eq!(cold.state_hash().unwrap(), deferred_hash);
        assert!(!source.take_snapshot_point());
        assert!(!cold.take_snapshot_point());
        assert_eq!(source.step().unwrap(), Step::Continued);
        assert_eq!(cold.step().unwrap(), Step::Continued);
        assert_eq!(cold.state_hash().unwrap(), source.state_hash().unwrap());
        assert!(source.take_snapshot_point());
        assert!(cold.take_snapshot_point());
        assert!(!cold.take_snapshot_point());
        assert_eq!(cold.state_hash().unwrap(), source.state_hash().unwrap());
    }

    #[test]
    fn report_stream_round_trips_through_save_restore() {
        let mut a = full_vmm(VcpuState::default(), vec![], 0, 1);
        a.report_stream = vec![0xAA, 0x0000_0000, 0xDEAD_BEEF];
        let s = a.save_vm_state().unwrap();

        let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
        assert!(b.report_stream().is_empty(), "B starts with no reports");
        b.restore_vm_state(&s).unwrap();
        assert_eq!(
            b.report_stream(),
            &[0xAA, 0x0000_0000, 0xDEAD_BEEF],
            "the report stream is restored in execution order"
        );
        assert_eq!(
            b.observable_digest(),
            a.observable_digest(),
            "the restored VM's O2 observable_digest matches the snapshot source"
        );
    }

    #[test]
    fn restore_vm_state_rejects_a_legacy_wiring_mismatch() {
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        assert!(
            dev.legacy.is_some() && dev.lapic.is_some(),
            "the full-VM blob carries both LAPIC and legacy state"
        );
        dev.legacy = None;
        s.devices = snapshot::encode_device_blob(&dev);

        let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
        assert!(
            matches!(b.restore_vm_state(&s), Err(VmmError::ContractViolation(_))),
            "a dropped legacy subrecord must be rejected, not silently skipped"
        );
    }

    #[test]
    fn restore_vm_state_rejects_a_staged_non_rng_completion() {
        let mut src = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut src, 6);
        let snap = src.save_vm_state().unwrap();

        let mut tgt = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })],
            10,
            1,
        );
        tgt.step().unwrap();
        assert!(matches!(
            tgt.restore_vm_state(&snap),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    fn output_completion_must_be_retired_before_restoring_registers() {
        let src = full_vmm(VcpuState::default(), vec![], 500, 1);
        let snap = src.save_vm_state().unwrap();
        let mut tgt = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Io {
                port: 0x3f8,
                size: 1,
                write: Some(b'x' as u32),
            })],
            10,
            1,
        );
        tgt.step().unwrap();
        assert!(tgt.completion_staged);
        assert!(matches!(
            tgt.restore_vm_state(&snap),
            Err(VmmError::ContractViolation(_))
        ));
        tgt.retire_pending_completion().unwrap();
        tgt.restore_vm_state(&snap).unwrap();
    }

    #[test]
    fn restore_vm_state_rejects_a_non_empty_timer_queue() {
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        s.timers.entries.push(vm_state::TimerEntry {
            deadline_vns: 1000,
            seq: 0,
            token: 7,
            period_vns: 0,
        });
        s.timers.next_seq = 1;
        let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
        assert!(matches!(
            b.restore_vm_state(&s),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    fn cpu_snapshot_preserves_pae_and_debug_flags_without_guest_execution() {
        for fields in 1..8 {
            let mut cpu = nonzero_state();
            if fields & 1 != 0 {
                cpu.sregs.flags = 1;
            }
            if fields & 2 != 0 {
                cpu.sregs.pdptrs = [0x1001, 0x2001, 0x3001, 0x4001];
            }
            if fields & 4 != 0 {
                cpu.debugregs.flags = 1;
            }
            let exits = vec![
                Exit::Arch(X86Exit::Io {
                    port: 0x3f8,
                    size: 1,
                    write: Some(0x58),
                }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            ];
            let mut source = full_vmm(cpu.clone(), exits.clone(), 0, 7);
            source.wire_snapshot_hashing();
            let before_hash = source.state_hash().unwrap();
            let before_counts = source.exit_counts();
            let saved = source.save_vm_state().unwrap();
            let bytes = saved.encode().unwrap();
            let decoded = vm_state::VmState::decode(&bytes).unwrap();
            assert_eq!(source.state_hash().unwrap(), before_hash);
            assert_eq!(source.exit_counts(), before_counts);
            assert_eq!(source.effective_vns(), Some(0));
            assert_eq!(decoded.sregs.flags, cpu.sregs.flags);
            assert_eq!(decoded.sregs.pdptrs, cpu.sregs.pdptrs);
            assert_eq!(decoded.debugregs.flags, cpu.debugregs.flags);

            let mut cold = full_vmm(VcpuState::default(), exits, 0, 7);
            cold.wire_snapshot_hashing();
            cold.restore_snapshot(source.guest_memory(), &decoded)
                .unwrap();
            assert_eq!(cold.state_hash().unwrap(), before_hash);
            assert_eq!(cold.save_vm_state().unwrap().encode().unwrap(), bytes);
            for _ in 0..2 {
                assert_eq!(source.step().unwrap(), cold.step().unwrap());
                assert_eq!(source.state_hash().unwrap(), cold.state_hash().unwrap());
                assert_eq!(source.effective_vns(), cold.effective_vns());
            }
            assert_eq!(source.serial_output(), cold.serial_output());
            assert_eq!(
                source.save_vm_state().unwrap().encode().unwrap(),
                cold.save_vm_state().unwrap().encode().unwrap()
            );
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn save_vm_state_captures_in_flight_events_at_a_non_quiescent_point() {
        let in_flight = |events: vmm_backend::VcpuEvents, name: &str| {
            let mut st = nonzero_state();
            st.events = events;
            let a = full_vmm(st, vec![], 0, 1);
            let s = a
                .save_vm_state()
                .unwrap_or_else(|e| panic!("{name}: an in-flight point must snapshot, got {e:?}"));
            let want = snapshot::canonical_events(&events);
            let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
            assert_eq!(
                dev.events, want,
                "{name}: canonical kvm_vcpu_events captured"
            );
            let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
            b.restore_vm_state(&s)
                .expect("restore the in-flight snapshot");
            assert_eq!(
                b.backend.save().unwrap().events,
                snapshot::events_for_restore(&events),
                "{name}: restore re-establishes the in-flight events (restore form) on the backend"
            );
        };
        in_flight(
            vmm_backend::VcpuEvents {
                nmi_masked: 1,
                ..Default::default()
            },
            "nmi_masked",
        );
        in_flight(
            vmm_backend::VcpuEvents {
                interrupt_injected: 1,
                interrupt_nr: 0x34,
                ..Default::default()
            },
            "interrupt_injected",
        );
        in_flight(
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_nr: 14,
                exception_has_error_code: 1,
                exception_error_code: 0xCAFE,
                ..Default::default()
            },
            "exception_error_code",
        );
        let rejects = |events: vmm_backend::VcpuEvents, needle: &str| {
            let mut st = nonzero_state();
            st.events = events;
            let v = full_vmm(st, vec![], 0, 1);
            match v.save_vm_state() {
                Err(VmmError::ContractViolation(msg)) => assert!(
                    msg.contains(needle),
                    "reject reason should name {needle:?}, got: {msg}"
                ),
                other => {
                    panic!("a triple-fault event field must fail closed at save, got {other:?}")
                }
            }
        };
        rejects(
            vmm_backend::VcpuEvents {
                triple_fault_pending: 1,
                ..Default::default()
            },
            "triple_fault_pending",
        );
        let v_ok = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            v_ok.save_vm_state().is_ok(),
            "a quiescent point still snapshots"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn save_vm_state_preserves_pending_exception_payload_through_fresh_restore() {
        let events = vmm_backend::VcpuEvents {
            exception_pending: 1,
            exception_nr: 14,
            exception_has_error_code: 1,
            exception_error_code: 0xCAFE,
            exception_has_payload: 1,
            exception_payload: 0x1234_5678_9ABC_DEF0,
            ..Default::default()
        };
        let mut source_state = nonzero_state();
        source_state.events = events;
        let mut source = full_vmm(source_state, vec![], 0, 1);
        source.wire_snapshot_hashing();

        let before_counts = source.exit_counts();
        let before_vns = source.effective_vns();
        let before_hash = source.state_hash().unwrap();
        let saved = source
            .save_vm_state()
            .expect("a payload-bearing pending exception is restorable");
        let bytes = <vm_state::VmState as SnapshotRecords>::encode(&saved)
            .expect("the payload snapshot encodes");
        let decoded = <vm_state::VmState as SnapshotRecords>::decode(&bytes)
            .expect("the payload snapshot decodes");
        assert_eq!(decoded, saved, "trait codec preserves the full snapshot");
        let dev = snapshot::decode_device_blob(&decoded.devices.0)
            .expect("the decoded device blob carries events");
        assert_eq!(
            dev.events,
            snapshot::canonical_events(&events),
            "the pending exception vector/error code/payload are serialized canonically"
        );

        assert_eq!(source.exit_counts(), before_counts);
        assert_eq!(source.effective_vns(), before_vns);
        assert_eq!(source.state_hash().unwrap(), before_hash);
        assert_eq!(
            <vm_state::VmState as SnapshotRecords>::encode(&source.save_vm_state().unwrap())
                .unwrap(),
            bytes,
            "repeated capture has stable VM bytes"
        );
        assert_eq!(source.state_hash().unwrap(), before_hash);

        let mut cold = full_vmm(VcpuState::default(), vec![], 9999, 1);
        cold.wire_snapshot_hashing();
        cold.restore_snapshot(source.guest_memory(), &decoded)
            .expect("fresh restore accepts the payload-bearing exception");
        assert_eq!(
            cold.backend.save().unwrap().events,
            snapshot::events_for_restore(&events),
            "fresh restore re-establishes the pending exception payload"
        );
        assert_eq!(cold.state_hash().unwrap(), before_hash);
        assert_eq!(
            <vm_state::VmState as SnapshotRecords>::encode(&cold.save_vm_state().unwrap()).unwrap(),
            bytes,
            "fresh restore re-encodes to the saved VM bytes"
        );
        assert_eq!(cold.exit_counts(), before_counts);
        assert_eq!(cold.effective_vns(), before_vns);
    }

    #[test]
    fn public_snapshot_path_accepts_last_exception_vector() {
        let cases = [
            (
                "pending",
                vmm_backend::VcpuEvents {
                    exception_pending: 1,
                    exception_nr: 31,
                    exception_has_error_code: 1,
                    exception_error_code: 0xCAFE,
                    ..Default::default()
                },
            ),
            (
                "injected",
                vmm_backend::VcpuEvents {
                    exception_injected: 1,
                    exception_nr: 31,
                    exception_has_error_code: 1,
                    exception_error_code: 0xBEEF,
                    ..Default::default()
                },
            ),
        ];

        for (name, events) in cases {
            let mut state = nonzero_state();
            state.events = events;
            let source = full_vmm(state, vec![], 0, 1);
            let saved = source
                .save_vm_state()
                .unwrap_or_else(|error| panic!("{name} vector 31 must save: {error:?}"));
            let bytes = <vm_state::VmState as SnapshotRecords>::encode(&saved)
                .unwrap_or_else(|error| panic!("{name} vector 31 must encode: {error:?}"));
            let decoded = <vm_state::VmState as SnapshotRecords>::decode(&bytes)
                .unwrap_or_else(|error| panic!("{name} vector 31 must decode: {error:?}"));
            let device = snapshot::decode_device_blob(&decoded.devices.0)
                .unwrap_or_else(|error| panic!("{name} device blob must decode: {error:?}"));
            assert_eq!(
                device.events.exception_nr, 31,
                "{name} vector survives codec"
            );
            assert_eq!(
                device.events.exception_pending, events.exception_pending,
                "{name} pending shape survives codec"
            );
            assert_eq!(
                device.events.exception_injected, events.exception_injected,
                "{name} injected shape survives codec"
            );

            let mut cold = full_vmm(VcpuState::default(), vec![], 0, 1);
            cold.restore_snapshot(source.guest_memory(), &decoded)
                .unwrap_or_else(|error| panic!("{name} vector 31 must restore: {error:?}"));
            assert_eq!(
                cold.backend.save().unwrap().events,
                snapshot::events_for_restore(&events),
                "{name} vector 31 survives fresh import"
            );
            assert_eq!(
                cold.save_vm_state().unwrap().encode().unwrap(),
                bytes,
                "{name} vector 31 re-encodes identically"
            );
        }
    }

    #[test]
    fn save_vm_state_rejects_invalid_active_exception_shapes() {
        let invalid = [
            (
                vmm_backend::VcpuEvents {
                    exception_injected: 1,
                    exception_pending: 1,
                    exception_nr: 14,
                    ..Default::default()
                },
                "exception_injected",
            ),
            (
                vmm_backend::VcpuEvents {
                    exception_pending: 1,
                    exception_nr: 2,
                    ..Default::default()
                },
                "vector 2",
            ),
            (
                vmm_backend::VcpuEvents {
                    exception_pending: 1,
                    exception_nr: 32,
                    ..Default::default()
                },
                "above 31",
            ),
            (
                vmm_backend::VcpuEvents {
                    exception_injected: 1,
                    exception_nr: 13,
                    exception_has_payload: 1,
                    exception_payload: 0xCAFE,
                    ..Default::default()
                },
                "exception_has_payload",
            ),
        ];
        for (events, needle) in invalid {
            let mut state = nonzero_state();
            state.events = events;
            let vmm = full_vmm(state, vec![], 0, 1);
            match vmm.save_vm_state() {
                Err(VmmError::ContractViolation(message)) => assert!(
                    message.contains(needle),
                    "save rejection should identify {needle:?}, got: {message}"
                ),
                other => panic!(
                    "save must reject invalid active exception shape {events:?}, got {other:?}"
                ),
            }
        }
    }

    #[test]
    fn restore_canonicalizes_raw_events_from_an_external_blob() {
        let a = full_vmm(nonzero_state(), vec![], 0, 1);
        let mut s = a.save_vm_state().expect("quiescent save");
        let raw = vmm_backend::VcpuEvents {
            interrupt_nr: 0x34,
            exception_nr: 13,
            exception_has_error_code: 1,
            flags: 0x0D,
            ..Default::default()
        };
        let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        dev.events = raw;
        s.devices = snapshot::encode_device_blob(&dev);
        let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
        b.restore_vm_state(&s).expect("restore the external blob");
        let restored = b.backend.save().unwrap().events;
        assert_eq!(
            restored,
            snapshot::events_for_restore(&raw),
            "restore strips the residuals and forces the clear-on-restore validity bits"
        );
        assert_eq!(restored.interrupt_nr, 0, "stale interrupt.nr stripped");
        assert_eq!(restored.exception_nr, 0, "stale exception.nr stripped");
        assert_eq!(
            restored.exception_has_error_code, 0,
            "stale has_error_code stripped"
        );
        assert_ne!(
            restored, raw,
            "the raw residuals were NOT forwarded verbatim"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn restore_vm_state_rejects_invalid_event_blobs_before_mutation() {
        let reject = |bad: vmm_backend::VcpuEvents, needle: &str| {
            let mut marked = nonzero_state();
            marked.events.interrupt_injected = 1;
            marked.events.interrupt_nr = 0x99;
            let mut b = full_vmm(marked, vec![], 0, 1);
            let before = b.backend.save().unwrap();
            let a = full_vmm(nonzero_state(), vec![], 0, 1);
            let mut s = a.save_vm_state().unwrap();
            let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
            dev.events = bad;
            s.devices = snapshot::encode_device_blob(&dev);
            match b.restore_vm_state(&s) {
                Err(VmmError::ContractViolation(msg)) => assert!(
                    msg.contains(needle),
                    "reject reason should name {needle:?}, got: {msg}"
                ),
                other => panic!("restore must reject an invalid event blob, got {other:?}"),
            }
            assert_eq!(
                b.backend.save().unwrap(),
                before,
                "restore must not mutate the target vCPU when it rejects the blob"
            );
        };
        reject(
            vmm_backend::VcpuEvents {
                triple_fault_pending: 1,
                ..Default::default()
            },
            "triple_fault_pending",
        );
        reject(
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_pending: 1,
                exception_nr: 14,
                ..Default::default()
            },
            "exception_injected",
        );
        reject(
            vmm_backend::VcpuEvents {
                exception_pending: 1,
                exception_nr: 2,
                ..Default::default()
            },
            "vector 2",
        );
        reject(
            vmm_backend::VcpuEvents {
                exception_pending: 1,
                exception_nr: 32,
                ..Default::default()
            },
            "above 31",
        );
        reject(
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_nr: 13,
                exception_has_payload: 1,
                exception_payload: 0xCAFE,
                ..Default::default()
            },
            "exception_has_payload",
        );
    }

    #[test]
    fn restore_vm_state_rejects_invalid_xsave_provenance_before_mutation() {
        let source = full_vmm(nonzero_state(), vec![], 0, 1);
        let mut snapshot = source.save_vm_state().unwrap();
        snapshot.xsave_restore_bv = Some(u64::MAX);

        let mut target_state = nonzero_state();
        target_state.regs.rax = 0xDEAD;
        target_state.regs.rbx = 0xBEEF;
        let mut target = full_vmm(target_state, vec![], 0, 9);
        let target_memory = vec![0x5A; 0x2000];
        target.restore_guest_memory(&target_memory).unwrap();
        let before = target.backend.save().unwrap();
        let before_blob = target.save_vm_state().unwrap().encode().unwrap();
        let before_vns = target.effective_vns();
        let source_memory = vec![0xA5; 0x2000];
        match target.restore_snapshot(&source_memory, &snapshot) {
            Err(VmmError::ContractViolation(message)) => {
                assert!(message.contains("XSAVE restore provenance"), "{message}")
            }
            other => panic!("malformed XSAVE provenance must be rejected, got {other:?}"),
        }
        assert_eq!(
            target.backend.save().unwrap(),
            before,
            "rejecting malformed XSAVE provenance must leave the target vCPU untouched"
        );
        assert!(
            target.guest_memory() == target_memory,
            "rejecting malformed XSAVE provenance must leave target RAM untouched"
        );
        assert_eq!(target.effective_vns(), before_vns);
        assert_eq!(
            target.save_vm_state().unwrap().encode().unwrap(),
            before_blob,
            "rejecting malformed XSAVE provenance must leave devices and V-time untouched"
        );
    }

    #[test]
    fn has_inflight_event_injection_reflects_the_live_vcpu() {
        let quiescent = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            !quiescent.has_inflight_event_injection(),
            "a quiescent vCPU is not a non-quiescent point"
        );
        let mut st = nonzero_state();
        st.events.interrupt_injected = 1;
        st.events.interrupt_nr = 0x34;
        let in_flight = full_vmm(st, vec![], 0, 1);
        assert!(
            in_flight.has_inflight_event_injection(),
            "an injected-but-undelivered interrupt is a non-quiescent point"
        );
    }

    #[test]
    fn has_active_event_injection_reflects_the_live_vcpu() {
        let quiescent = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            !quiescent.has_active_event_injection(),
            "a quiescent vCPU carries no active event"
        );
        let mut residual = nonzero_state();
        residual.events.interrupt_nr = 0x34;
        let residual_vmm = full_vmm(residual, vec![], 0, 1);
        assert!(
            residual_vmm.has_inflight_event_injection(),
            "an inert residual is still a would-reject point"
        );
        assert!(
            !residual_vmm.has_active_event_injection(),
            "but an inert residual is NOT a genuine active injection — never seal here"
        );
        let mut st = nonzero_state();
        st.events.interrupt_injected = 1;
        st.events.interrupt_nr = 0x34;
        let in_flight = full_vmm(st, vec![], 0, 1);
        assert!(
            in_flight.has_active_event_injection(),
            "an injected-but-undelivered interrupt is a genuine active injection"
        );
    }

    #[test]
    fn has_pending_guest_interrupt_reflects_a_pending_lapic_vector() {
        let mut q = lapic_vmm(configured_mock(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]));
        q.step().unwrap();
        assert!(
            !q.has_pending_guest_interrupt().unwrap(),
            "a quiescent LAPIC has no pending guest interrupt"
        );
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(Exit::Arch(X86Exit::Rdmsr { index: 0x10 }));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut a = lapic_vmm(mock);
        step_n(&mut a, 5);
        assert_eq!(
            a.backend.pending_irq(),
            Some(0x40),
            "0x40 is in flight in the IRR (routed to the seam, not yet accepted)"
        );
        assert!(
            a.has_pending_guest_interrupt().unwrap(),
            "a vector pending in the LAPIC IRR is a genuine in-flight guest interrupt"
        );
    }

    #[test]
    fn snapshot_restore_re_derives_the_in_flight_lapic_irq() {
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(Exit::Arch(X86Exit::Rdmsr { index: 0x10 }));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut a = lapic_vmm(mock);
        step_n(&mut a, 5);
        assert_eq!(
            a.backend.pending_irq(),
            Some(0x40),
            "the timer vector is in flight (routed to the seam, not yet accepted)"
        );

        let s = a
            .save_vm_state()
            .expect("a point with an in-flight LAPIC vector is snapshottable");
        let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        let irr = dev.lapic.expect("lapic captured").irr;
        assert_eq!(irr[2] & 1, 1, "vector 0x40 is pending in the captured IRR");

        let mut bmock = configured_mock(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        bmock.set_defer_accept(true);
        let mut b = lapic_vmm(bmock);
        b.restore_vm_state(&s)
            .expect("restore the in-flight LAPIC snapshot");
        b.step().unwrap();
        assert_eq!(
            b.backend.pending_irq(),
            Some(0x40),
            "the restored VM re-derives the in-flight vector from the LAPIC IRR (seam re-armed)"
        );
    }

    #[test]
    fn save_vm_state_captures_the_uart_dlm() {
        let mut v = full_vmm(
            VcpuState::default(),
            vec![
                Exit::Arch(X86Exit::Io {
                    port: 0x3FB,
                    size: 1,
                    write: Some(0x80),
                }),
                Exit::Arch(X86Exit::Io {
                    port: 0x3F9,
                    size: 1,
                    write: Some(0x07),
                }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            ],
            0,
            1,
        );
        step_n(&mut v, 3);
        let s = v.save_vm_state().unwrap();
        let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        assert_eq!(dev.uart.dlm, 7, "save_vm_state must capture the UART DLM");
        assert!(dev.uart.dlab, "and the latched DLAB window state");
    }

    #[test]
    fn restore_guest_memory_overwrites_the_backing_and_checks_length() {
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x2000).unwrap());
        let image = vec![0xABu8; 0x2000];
        v.restore_guest_memory(&image).unwrap();
        assert_eq!(v.guest_memory(), &image[..]);
        assert!(matches!(
            v.restore_guest_memory(&[0u8; 0x1000]),
            Err(VmmError::ContractViolation(_))
        ));
    }

    fn has_tag(blob: &[u8], tag: &[u8; 4]) -> bool {
        blob.windows(4).any(|w| w == tag)
    }

    #[test]
    fn snapshot_hashing_is_gated_off_by_default() {
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(!v.snapshot_hashing_wired());
        assert!(!has_tag(&v.state_blob().unwrap(), b"VMST"));
        let v2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(v.state_hash().unwrap(), v2.state_hash().unwrap());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings"
    )]
    fn wiring_snapshot_hashing_folds_the_canonical_blob_into_the_hash() {
        let base = full_vmm(VcpuState::default(), vec![], 0, 1);
        let base_hash_unwired = base.state_hash().unwrap();

        let mut on = full_vmm(VcpuState::default(), vec![], 0, 1);
        on.wire_snapshot_hashing();
        assert!(on.snapshot_hashing_wired());
        assert!(has_tag(&on.state_blob().unwrap(), b"VMST"));
        assert_ne!(
            on.state_hash().unwrap(),
            base_hash_unwired,
            "folding the canonical blob changes the hash"
        );

        let mut a = full_vmm(VcpuState::default(), vec![], 0, 1);
        a.wire_snapshot_hashing();
        let mut b = full_vmm(
            VcpuState::default(),
            vec![Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x30),
            })],
            0,
            1,
        );
        b.wire_snapshot_hashing();
        b.step().unwrap();
        assert_ne!(
            a.state_hash().unwrap(),
            b.state_hash().unwrap(),
            "a vm_state difference reaches state_hash when snapshot-hashing is wired"
        );
    }

    use vtime::pvclock::PVCLOCK_PAGE_LEN;

    fn pvclock_vmm(exits: Vec<Exit<X86>>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(TEST_RAM).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm.enable_pvclock();
        vmm
    }

    fn ring_pvclock_register(vmm: &mut Vmm<MockBackend>, gpa: u64) -> (u16, Vec<u8>) {
        let mut frame = [0_u8; 64];
        let len = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            1,
            1,
            &gpa.to_le_bytes(),
            &mut frame,
        )
        .expect("encode register request");
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        assert_eq!(
            vmm.service_doorbell(len as u32).expect("doorbell serviced"),
            Step::Continued
        );
        let resp = &vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE];
        let (header, payload) = decode(resp).expect("well-formed response frame");
        (header.status, payload.to_vec())
    }

    const PV_GPA: u64 = 0x4000;

    #[test]
    fn pvclock_gpa_helpers_resolve_a_high_ram_base() {
        let mut vmm = pvclock_vmm(vec![], 7);
        vmm.ram_base_gpa = 0x4000_0000;

        let high = 0x4000_0000 + PV_GPA;
        assert!(
            vmm.pvclock_validate_gpa(high).is_ok(),
            "a high arm64 pvclock GPA must validate: {:?}",
            vmm.pvclock_validate_gpa(high)
        );
        assert_eq!(
            vmm.pvclock_validate_gpa(0x1000),
            Err("below the guest RAM base")
        );

        assert_eq!(vmm.pvclock_register(high).0 as u16, Status::Ok as u16);
        assert_eq!(vmm.pvclock_registration(), Some(high));
        assert!(
            vmm.pvclock_page().is_some(),
            "pvclock_page must resolve the high GPA"
        );

        let x86 = pvclock_vmm(vec![], 7);
        assert!(x86.pvclock_validate_gpa(PV_GPA).is_ok());
    }

    #[test]
    fn pvclock_registration_rejects_bad_gpas() {
        for bad in [
            PV_GPA + 1,
            TEST_RAM as u64,
            u64::MAX - 4095,
            REQ_GPA as u64,
            RESP_GPA as u64,
        ] {
            let mut vmm = pvclock_vmm(vec![], 7);
            let (status, payload) = ring_pvclock_register(&mut vmm, bad);
            assert_eq!(status, Status::OutOfRange as u16, "gpa {bad:#x}");
            assert!(payload.is_empty());
            assert_eq!(vmm.pvclock_registration(), None, "gpa {bad:#x} recorded");
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "4 GiB guest RAM image — lazy mmap, but not for the interpreter"
    )]
    fn pvclock_registration_rejects_the_lapic_mmio_hole() {
        const LAPIC_HOLE: u64 = 0xFEE0_0000;
        let ram_len = (LAPIC_HOLE + 0x2000) as usize;
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(ram_len).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        vmm.enable_pvclock();
        let (status, _) = ring_pvclock_register(&mut vmm, LAPIC_HOLE);
        assert_eq!(
            status,
            Status::OutOfRange as u16,
            "the xAPIC MMIO page was accepted as a clock page — the host would stamp backing \
             the guest cannot read, and the guest would read the LAPIC instead of its clock"
        );
        assert_eq!(vmm.pvclock_registration(), None);
        let (status, _) = ring_pvclock_register(&mut vmm, LAPIC_HOLE + 0x1000);
        assert_eq!(status, Status::Ok as u16);
        assert_eq!(vmm.pvclock_registration(), Some(LAPIC_HOLE + 0x1000));
    }

    #[test]
    fn pvclock_unavailable_answers_unknown_service_before_classifying() {
        let mut m = MockBackend::new();
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_pvclock();
        assert!(!vmm.pvclock_available());

        let ring_raw = |vmm: &mut Vmm<MockBackend>, opcode: u32, payload: &[u8]| -> u16 {
            let mut frame = [0_u8; 64];
            let len =
                hypercall_proto::encode_request(ServiceId::Pvclock, 1, opcode, payload, &mut frame)
                    .expect("encode");
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
            vmm.service_doorbell(len as u32).expect("doorbell");
            let resp = &vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE];
            decode(resp).expect("well-formed response frame").0.status
        };
        assert_eq!(
            ring_raw(&mut vmm, 1, &PV_GPA.to_le_bytes()),
            Status::UnknownService as u16
        );
        assert_eq!(
            ring_raw(&mut vmm, 1, &[0u8; 3]),
            Status::UnknownService as u16,
            "a malformed payload was graded BadRequest on a service that is not offered"
        );
        assert_eq!(
            ring_raw(&mut vmm, 9, &[]),
            Status::UnknownService as u16,
            "a bad opcode was graded UnknownOpcode on a service that is not offered"
        );
        assert_eq!(vmm.pvclock_registration(), None);
    }

    #[test]
    fn pvclock_channel_configuration_reaches_state_identity() {
        let build = || {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
            vmm.enable_pvclock();
            vmm
        };
        let base = build().state_blob().unwrap();
        assert_eq!(
            base,
            build().state_blob().unwrap(),
            "same configuration must hash identically"
        );
        let mut registered = build();
        let (status, _) = ring_pvclock_register(&mut registered, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
        let pending = registered.state_blob().unwrap();
        assert_ne!(
            base, pending,
            "a registration is a different future — must reach the hash"
        );
        registered.pvclock.as_mut().unwrap().armed = true;
        assert_ne!(
            pending,
            registered.state_blob().unwrap(),
            "pending vs armed is a different future — the handshake bit must reach the hash"
        );
    }

    #[test]
    fn pvclock_snapshot_and_restore_preserve_pending_registration_state() {
        let mut source = pvclock_vmm(vec![], 7);
        assert_eq!(
            ring_pvclock_register(&mut source, PV_GPA).0,
            Status::Ok as u16
        );
        let expected = PvclockSnapshot {
            gpa: Some(PV_GPA),
            registrable: true,
            armed: false,
        };
        assert_eq!(source.pvclock_snapshot(), Some(expected));

        let state = <X86 as crate::vendor::Vendor>::build_vm_state(&source, &VcpuState::default());
        let decoded = snapshot::decode_device_blob(&state.devices.0).unwrap();
        assert_eq!(decoded.pvclock, Some(expected));

        let mut target = pvclock_vmm(vec![], 7);
        target
            .pvclock_validate_restore(decoded.pvclock.as_ref())
            .unwrap();
        target.pvclock_commit_restore(decoded.pvclock.as_ref());
        assert_eq!(target.pvclock_snapshot(), Some(expected));
    }

    #[test]
    fn pvclock_decode_rejects_registered_but_non_registrable() {
        use crate::vendor::x86::records::{DeviceState, encode_device_blob};
        let good = DeviceState {
            pvclock: Some(crate::vmm::PvclockSnapshot {
                gpa: Some(PV_GPA),
                registrable: true,
                armed: false,
            }),
            ..DeviceState::default()
        };
        let mut blob = encode_device_blob(&good).0;
        assert_eq!(
            blob[blob.len() - 2],
            1,
            "the registrable byte precedes armed"
        );
        let registrable_index = blob.len() - 2;
        blob[registrable_index] = 0;
        assert!(
            crate::vendor::x86::records::decode_device_blob(&blob).is_err(),
            "a registered-but-non-registrable v5 record must be rejected at the wire"
        );
    }

    #[test]
    fn pvclock_natural_exits_refresh_with_the_anchor_value() {
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Io {
                    port: 0x3F8,
                    size: 1,
                    write: Some(u32::from(b'x')),
                }),
            ],
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap();
        let stamped = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        let off = PV_GPA as usize + vtime::pvclock::VNS_OFF;
        vmm.ram.as_mut_bytes()[off] ^= 0xA5;
        assert!(vmm.pvclock_check_oracle().is_err(), "scribble visible");
        vmm.step().unwrap();
        let repaired = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert!(repaired.vns > stamped.vns);
        vmm.pvclock_check_oracle()
            .expect("the natural-exit refresh restored oracle equality");
    }

    #[test]
    fn pvclock_seal_never_touches_the_live_page() {
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
            ],
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap();
        vmm.step().unwrap();
        vmm.host_dirty.clear();
        let page_before = vmm.pvclock_page().unwrap().to_vec();
        assert_ne!(
            vtime::pvclock::read(&page_before).unwrap().seq,
            0,
            "precondition: the live page is non-canonical"
        );
        let refreshes_before = vmm.pvclock_refreshes().to_vec();
        vmm.retire_pending_completion().unwrap();
        let mut bad = vmm.backend.save().unwrap();
        bad.events.triple_fault_pending = 1;
        vmm.backend.restore(&bad).unwrap();
        vmm.saved_state = None;
        assert!(
            matches!(vmm.save_vm_state(), Err(VmmError::ContractViolation(_))),
            "the unsealable vCPU must reject the seal"
        );
        assert_eq!(
            vmm.pvclock_page().unwrap(),
            page_before.as_slice(),
            "a rejected seal canonicalized the page (reject-before-mutation broken)"
        );
        assert_eq!(vmm.pvclock_refreshes(), refreshes_before.as_slice());
        assert!(
            vmm.host_dirty.is_empty(),
            "a rejected seal marked host-dirty state"
        );
        bad.events.triple_fault_pending = 0;
        vmm.backend.restore(&bad).unwrap();
        vmm.save_vm_state().unwrap();
        assert_eq!(
            vmm.pvclock_page().unwrap(),
            page_before.as_slice(),
            "a successful seal rewrote the live page — the ABA the r4 P1 rules out"
        );
        assert!(
            vmm.host_dirty.is_empty(),
            "a seal marked host-dirty state — it wrote to guest RAM"
        );
    }

    #[test]
    fn pvclock_refresh_tracks_the_trap_oracle_through_intercepts() {
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            ],
            7,
        );
        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);

        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!((f.vns, f.guest_clock), (1, 2));
        let trap_value = match vmm.backend.completions().last().unwrap() {
            Completion::Read(v) => *v,
            other => panic!("RDTSC completes as a read, got {other:?}"),
        };
        assert_eq!(f.guest_clock, trap_value, "page == what the trap returned");
        vmm.pvclock_check_oracle().unwrap();

        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.guest_clock, 9, "guest_clock = ticks(2) + adjust 5");
        vmm.pvclock_check_oracle().unwrap();

        let seq_before = f.seq;
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.guest_clock, 11);
        assert_ne!(f.seq, seq_before);

        assert_eq!(vmm.pvclock_refreshes(), &[(2, 9), (3, 11)]);
        vmm.pvclock_clear_refreshes();
        assert!(vmm.pvclock_refreshes().is_empty());
    }

    #[test]
    fn pvclock_oracle_check_fails_on_a_corrupted_page() {
        let mut vmm = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 7);
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap();
        vmm.pvclock_check_oracle().expect("clean page passes");
        let off = PV_GPA as usize + vtime::pvclock::GUEST_CLOCK_OFF;
        vmm.ram.as_mut_bytes()[off] ^= 0xFF;
        assert!(
            matches!(
                vmm.pvclock_check_oracle(),
                Err(VmmError::ContractViolation(_))
            ),
            "a diverged page must fail the G2 check"
        );
        vmm.ram.as_mut_bytes()[off] ^= 0xFF;
        vmm.pvclock_check_oracle()
            .expect("repaired page passes again");
        vmm.vtime.as_mut().unwrap().advance_virtual_time(999);
        assert!(
            matches!(
                vmm.pvclock_check_oracle(),
                Err(VmmError::ContractViolation(_))
            ),
            "a frozen page must fail once the clock has moved on"
        );
    }

    #[test]
    fn restore_vtime_restamps_the_armed_page_to_the_restored_timeline() {
        const SEED: u64 = 7;
        let mut a = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], SEED);
        a.vtime.as_mut().unwrap().advance_virtual_time(999);
        ring_pvclock_register(&mut a, PV_GPA);
        a.step().unwrap();
        let ahead = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_eq!(
            (ahead.seq, ahead.vns),
            (0, 1000),
            "armed at the large anchor"
        );

        let snap = VtimeSnapshot {
            vns: 42,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(SEED).save_state(),
        };
        a.restore_vtime(&snap).unwrap();

        let after = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_eq!(
            after.vns, 42,
            "restore_vtime must re-stamp the armed page to the restored anchor"
        );
        assert!(
            after.vns < ahead.vns,
            "the page moved BACK to the restored time"
        );
        assert_ne!(
            after.seq, ahead.seq,
            "the seqlock epoch must advance across a live-page restore (ABA-safety)"
        );
        assert_ne!(
            after.seq, 0,
            "a LIVE re-stamp must use the epoch-advancing refresh, not canonical seq=0"
        );
        a.pvclock_check_oracle()
            .expect("re-stamped page matches the restored-clock oracle");
    }

    #[test]
    fn restore_vtime_leaves_a_pending_registration_unstamped() {
        let mut v = pvclock_vmm(vec![], 7);
        ring_pvclock_register(&mut v, PV_GPA);
        assert!(
            vtime::pvclock::read(v.pvclock_page().unwrap()).is_none(),
            "pending registration is un-stamped before restore"
        );
        let snap = VtimeSnapshot {
            vns: 42,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(7).save_state(),
        };
        v.restore_vtime(&snap).unwrap();
        assert!(
            vtime::pvclock::read(v.pvclock_page().unwrap()).is_none(),
            "restore_vtime must not stamp a pending (un-armed) registration"
        );
    }

    #[test]
    fn save_vm_state_preserves_pending_pvclock_for_the_next_handshake() {
        const SEED: u64 = 7;
        let expected = PvclockSnapshot {
            gpa: Some(PV_GPA),
            registrable: true,
            armed: false,
        };
        let pending = || {
            let mut v = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], SEED);
            assert_eq!(ring_pvclock_register(&mut v, PV_GPA).0, Status::Ok as u16);
            v.restore_vtime(&VtimeSnapshot {
                vns: 42,
                guest_clock_offset: 0,
                entropy: SeededEntropy::new(SEED).save_state(),
            })
            .unwrap();
            assert_eq!(v.effective_vns(), Some(42));
            assert_eq!(v.pvclock_snapshot(), Some(expected));
            assert!(
                vtime::pvclock::read(v.pvclock_page().unwrap()).is_none(),
                "the pending page is still unstamped"
            );
            v
        };

        let mut uninterrupted = pending();
        let mut save_continue = pending();
        let before_counts = save_continue.exit_counts();
        let before_time = save_continue.effective_vns();
        let before_page = save_continue.pvclock_page().unwrap().to_vec();
        let before_memory = save_continue.guest_memory().to_vec();
        let before_refreshes = save_continue.pvclock_refreshes().to_vec();

        let saved = save_continue
            .save_vm_state()
            .expect("a pending pvclock registration is representable");
        let saved = vm_state::VmState::decode(&saved.encode().unwrap()).unwrap();
        assert_eq!(save_continue.exit_counts(), before_counts);
        assert_eq!(save_continue.effective_vns(), before_time);
        assert_eq!(
            save_continue.pvclock_page().unwrap(),
            before_page.as_slice()
        );
        assert_eq!(save_continue.guest_memory(), before_memory.as_slice());
        assert_eq!(
            save_continue.pvclock_refreshes(),
            before_refreshes.as_slice()
        );
        assert_eq!(save_continue.pvclock_snapshot(), Some(expected));

        let decoded = snapshot::decode_device_blob(&saved.devices.0).unwrap();
        assert_eq!(decoded.pvclock, Some(expected));

        let mut cold = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], SEED);
        cold.restore_snapshot(&before_memory, &saved)
            .expect("cold restore preserves the pending registration");
        assert_eq!(cold.effective_vns(), before_time);
        assert_eq!(cold.pvclock_snapshot(), Some(expected));
        assert_eq!(cold.pvclock_page().unwrap(), before_page.as_slice());
        assert!(
            vtime::pvclock::read(cold.pvclock_page().unwrap()).is_none(),
            "restore must not stamp a pending page"
        );

        let handshake = |v: &mut Vmm<MockBackend>| {
            assert_eq!(v.step().unwrap(), Step::Continued);
            assert_eq!(
                v.pvclock_snapshot().map(|snapshot| snapshot.armed),
                Some(true)
            );
            let page = v.pvclock_page().unwrap().to_vec();
            assert!(
                vtime::pvclock::read(&page).is_some(),
                "the post-doorbell time read must arm and stamp the page"
            );
            let completions = v.backend.completions().to_vec();
            assert!(
                matches!(completions.as_slice(), [Completion::Read(_)]),
                "the handshake must service one read completion"
            );
            (
                v.effective_vns(),
                v.exit_counts(),
                completions,
                v.pvclock_snapshot(),
                page,
                v.guest_memory().to_vec(),
                v.save_vm_state().unwrap().encode().unwrap(),
                v.state_hash().unwrap(),
            )
        };

        let expected_after = handshake(&mut uninterrupted);
        assert_eq!(handshake(&mut save_continue), expected_after);
        assert_eq!(handshake(&mut cold), expected_after);
    }

    #[test]
    fn pvclock_seal_is_verbatim_and_restore_carries_the_registration() {
        let mut a = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
            ],
            7,
        );
        ring_pvclock_register(&mut a, PV_GPA);
        a.step().unwrap();
        a.step().unwrap();
        let live = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_ne!(live.seq, 0, "a mid-run refresh bumped the epoch");
        let page_before_seal = a.pvclock_page().unwrap().to_vec();
        let vm_state = a.save_vm_state().unwrap();
        let image = a.guest_memory().to_vec();
        assert_eq!(
            a.pvclock_page().unwrap(),
            page_before_seal.as_slice(),
            "the seal rewrote the live page"
        );
        assert_eq!(
            &image[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
            page_before_seal.as_slice(),
            "the sealed image must reproduce live guest memory, page included"
        );
        let sealed = vtime::pvclock::read(&image[PV_GPA as usize..]).unwrap();
        assert_eq!(
            (sealed.seq, sealed.vns, sealed.guest_clock),
            (live.seq, live.vns, live.guest_clock),
            "the sealed page carries the live epoch and values verbatim"
        );

        let mut b = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 7);
        ring_pvclock_register(&mut b, PV_GPA + 0x1000);
        b.restore_snapshot(&image, &vm_state).unwrap();
        assert_eq!(
            b.pvclock_registration(),
            Some(PV_GPA),
            "the blob's sealed registration is authoritative after a direct restore"
        );
        assert_eq!(
            &b.guest_memory()[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
            &image[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
        );
        b.pvclock_check_oracle().unwrap();
    }

    #[test]
    fn pvclock_restore_mismatch_fails_loud() {
        let seal = |register: bool| {
            let mut src = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 7);
            if register {
                ring_pvclock_register(&mut src, PV_GPA);
            }
            src.step().unwrap();
            src.save_vm_state().unwrap()
        };
        let registered_state = seal(true);
        let offered_unregistered_state = seal(false);
        let unoffered_state = {
            let mut src = vtime_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 7);
            let _ = &mut src;
            let mut src = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]),
                GuestRam::new(TEST_RAM).unwrap(),
            );
            src.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
            src.step().unwrap();
            src.save_vm_state().unwrap()
        };
        let reject = |vmm: &mut Vmm<MockBackend>, s: &vm_state::VmState, why: &str| {
            assert!(
                matches!(vmm.restore_vm_state(s), Err(VmmError::ContractViolation(_))),
                "expected loud rejection: {why}"
            );
        };

        let mut unoffered = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        unoffered.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        reject(&mut unoffered, &registered_state, "registered -> unoffered");
        reject(
            &mut unoffered,
            &offered_unregistered_state,
            "offered-unregistered -> unoffered",
        );
        unoffered.restore_vm_state(&unoffered_state).unwrap();

        let mut offered = pvclock_vmm(vec![], 7);
        reject(&mut offered, &unoffered_state, "unoffered -> offered");

        let mut small = Vmm::new(configured_mock(vec![]), GuestRam::new(0x2000).unwrap());
        small.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        small.enable_pvclock();
        reject(&mut small, &registered_state, "GPA past the target's RAM");
        assert_eq!(small.pvclock_registration(), None, "rejection mutated");
    }

    #[test]
    fn pvclock_doorbell_rejects_bad_frames() {
        let mut vmm = pvclock_vmm(vec![], 7);
        let mut frame = [0_u8; 64];
        let len = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            2,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        vmm.service_doorbell(len as u32).unwrap();
        let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
        assert_eq!(header.status, Status::UnknownOpcode as u16);

        let len =
            hypercall_proto::encode_request(ServiceId::Pvclock, 1, 2, &[0; 7], &mut frame).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        vmm.service_doorbell(len as u32).unwrap();
        let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
        assert_eq!(header.status, Status::BadRequest as u16);
        assert_eq!(vmm.pvclock_registration(), None);
    }

    #[test]
    fn pvclock_only_composition_rejects_unoffered_doorbell_services() {
        let mut vmm = pvclock_vmm(vec![], 7);
        let entropy_before = vmm.entropy_state();

        let ring = |vmm: &mut Vmm<MockBackend>, frame: &[u8]| -> (Step, u16) {
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + frame.len()].copy_from_slice(frame);
            let step = vmm.service_doorbell(frame.len() as u32).unwrap();
            let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
            (step, header.status)
        };

        let mut frame = [0_u8; 128];
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x0001_0001u32.to_le_bytes());
        payload.extend_from_slice(b"assert detail bytes");
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Event must be unoffered"
        );
        assert_eq!(step, Step::Continued, "no SdkStop without an SDK channel");

        let n =
            hypercall_proto::encode_request(ServiceId::Sdk, 1, 2, &7u32.to_le_bytes(), &mut frame)
                .unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Sdk must be unoffered"
        );
        assert_eq!(step, Step::Continued);

        let n = hypercall_proto::encode_request(
            ServiceId::Entropy,
            1,
            3,
            &8u32.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Entropy must be unoffered"
        );
        assert_eq!(step, Step::Continued);
        assert_eq!(
            vmm.entropy_state(),
            entropy_before,
            "an unoffered entropy ask advanced the shared seeded stream"
        );

        for (svc, name) in [
            (ServiceId::Event, "Event"),
            (ServiceId::Sdk, "Sdk"),
            (ServiceId::Entropy, "Entropy"),
        ] {
            let n = hypercall_proto::encode_request(svc, 1, 7, &[], &mut frame).unwrap();
            let (step, status) = ring(&mut vmm, &frame[..n]);
            assert_eq!(
                status,
                Status::UnknownService as u16,
                "{name} opcode 7 on a pvclock-only VM must be UnknownService (unoffered), not \
                 UnknownOpcode — that would advertise the service"
            );
            assert_eq!(step, Step::Continued);
        }

        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
    }
}
