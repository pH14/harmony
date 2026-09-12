// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use control_proto::{
    Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, EventRef, HashScope, Moment,
    READ_CAP, RegsView, Reply, Reproducer, Request, SnapId, StopReason, decode_request,
    encode_reply,
};
use environment::{
    channel::{Effect, NominalHandler, RecordedEnv, ServiceHandler},
    input_spec::{InputSpec as EnvSpec, ServiceConfig, ServiceFactory, nominal_factory},
};
use snapshot_store::SnapshotId;
use vm_state::SnapshotRecords;
use vmm_backend::Backend;

use crate::vendor::{InterruptReject, Vendor};

use crate::control_state::{ControlState, ScheduleFailure};
use crate::exec::ExecSession;
use crate::portable_snapshot::{
    PortableSnapshot, PortableSnapshotError, PortableSnapshotRef, SparsePortableSidecarRef,
    SparsePortableSnapshot, SparsePortableSnapshotReceipt, decode_sparse_sidecar,
    encode_sparse_sidecar,
};
use crate::session_trace::{SessionTraceSegment, SessionTraceStart, SessionVirtualTimeTrace};
use crate::snapshot::{SnapshotEngine, SnapshotError};
use crate::vmm::{SdkSnapshot, SdkStop, Step, TerminalReason, Vmm, VmmError};

#[cfg(all(target_os = "linux", any(test, not(miri))))]
fn host_minor_faults_with(getrusage: impl FnOnce(*mut libc::rusage) -> i32) -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if getrusage(usage.as_mut_ptr()) != 0 {
        return None;
    }
    // SAFETY: a zero return from the supplied getrusage-compatible function
    // means it initialized the caller-owned `rusage` buffer completely.
    let minor_faults = unsafe { usage.assume_init() }.ru_minflt;
    u64::try_from(minor_faults).ok()
}

#[cfg(all(target_os = "linux", not(miri)))]
pub fn host_minor_faults() -> Option<u64> {
    host_minor_faults_with(|usage| {
        // SAFETY: `usage` points to the live, writable `MaybeUninit<rusage>`
        // above, and libc writes it only for the duration of this call.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage) }
    })
}

#[cfg(any(not(target_os = "linux"), miri))]
pub fn host_minor_faults() -> Option<u64> {
    None
}

pub type VmmFactory<B> = Box<dyn FnMut() -> Result<Vmm<B>, VmmError>>;

pub type RemapVmmFactory<B> = Box<dyn FnMut(snapshot_store::Mapping) -> Result<Vmm<B>, VmmError>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestoreMode {
    InPlace,
    Remap,
    Memcpy,
}

fn restore_defers_materialization(mode: RestoreMode) -> bool {
    mode == RestoreMode::InPlace
}

fn restore_error_is_precommit(error: &VmmError) -> bool {
    matches!(
        error,
        VmmError::ContractViolation(_) | VmmError::Snapshot(_) | VmmError::Vtime(_)
    )
}

fn map_restore_preflight_error(error: VmmError) -> PortableSnapshotError {
    match error {
        VmmError::Snapshot(error) => PortableSnapshotError::Snapshot(error),
        _ => PortableSnapshotError::Malformed("portable VM state restore validation"),
    }
}

fn sparse_page_order_error(previous: Option<u64>, current: u64) -> Option<SnapshotError> {
    let previous = previous?;
    if current < previous {
        return Some(SnapshotError::SparsePagesNotSorted { previous, current });
    }
    if current == previous {
        return Some(SnapshotError::SparsePageDuplicate { gfn: current });
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("control transport I/O error")]
    Io(#[from] std::io::Error),
    #[error("control transport framing error: {0}")]
    Protocol(#[from] control_proto::ProtocolError),
    #[error("substrate failure: {0}")]
    Vmm(#[from] VmmError),
    #[error("snapshot store failure")]
    Snapshot(#[from] SnapshotError),
    #[error("service state failure: {0}")]
    Service(#[from] environment::channel::ChannelError),
    #[error("server poisoned by a prior fatal error")]
    Poisoned,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PortableSnapshotReceipt {
    pub id: SnapId,
    pub at: Moment,
    pub sdk_events: u64,
    pub trace_events: u64,
    pub trace_schedules: u64,
    pub tainted: bool,
    pub state_hash: [u8; 32],
}

pub fn server_caps() -> Caps {
    Caps {
        protocol_version: control_proto::APP_PROTOCOL_VERSION,
        env_version_min: EnvSpec::BLOB_VERSION,
        env_version_max: EnvSpec::BLOB_VERSION,
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: control_proto::CapFlags::GUEST_HAS_SDK,
    }
}

pub struct ControlServer<B: Backend<A: Vendor>> {
    vmm: Option<Vmm<B>>,
    factory: VmmFactory<B>,
    service_factory: ServiceFactory,
    remap_factory: Option<RemapVmmFactory<B>>,
    restore_mode: RestoreMode,
    engine: SnapshotEngine,
    derive_parent: Option<SnapshotId>,
    current_image: Option<SnapshotId>,
    in_place_fallbacks: u64,
    last_restore_bytes_written: u64,
    snaps: BTreeMap<u64, SnapshotId>,
    next_snap: u64,
    hello_done: bool,
    schedule: BTreeMap<u64, Effect>,
    reseed_schedule: BTreeMap<u64, u64>,
    recorded: EnvSpec,
    schedule_poisoned: Option<ScheduleFailure>,
    sdk_snaps: BTreeMap<u64, SdkSnap>,
    timeline_tainted: bool,
    tainted_snaps: BTreeSet<u64>,
    snapshot_meta: BTreeMap<u64, SnapshotMeta>,
    exec_nonce: u64,
    session_trace: Vec<SessionTraceSegment>,
    session_trace_start: SessionTraceStart,
    last_seal_dirty_gfns: Option<Vec<u64>>,
}

#[derive(Clone)]
struct SdkSnap {
    channel: SdkSnapshot,
    policy: ServiceConfig,
}

#[derive(Clone)]
struct SnapshotMeta {
    at: u64,
    sdk_events: u64,
    trace_events: u64,
    trace_schedules: u64,
    tainted: bool,
    state_hash: Option<[u8; 32]>,
    state_blob_suffix: Vec<u8>,
    policy: ServiceConfig,
    control_state: Vec<u8>,
}

fn validate_control_hash_suffix(
    mut suffix: &[u8],
    control: &[u8],
) -> Result<(), PortableSnapshotError> {
    let malformed = || PortableSnapshotError::Malformed("sparse control hash preimage");
    while !suffix.is_empty() {
        let header = suffix.get(..12).ok_or_else(malformed)?;
        let length = u64::from_le_bytes(header[4..12].try_into().map_err(|_| malformed())?);
        let length = usize::try_from(length).map_err(|_| malformed())?;
        let end = 12_usize.checked_add(length).ok_or_else(malformed)?;
        let payload = suffix.get(12..end).ok_or_else(malformed)?;
        let tail = &suffix[end..];
        if &header[..4] == b"CPLN" {
            return if !control.is_empty() && payload == control && tail.is_empty() {
                Ok(())
            } else {
                Err(malformed())
            };
        }
        suffix = tail;
    }
    if control.is_empty() {
        Ok(())
    } else {
        Err(malformed())
    }
}

fn hash_state_blob_parts(memory: &[u8], suffix: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"MEM\0");
    hasher.update((memory.len() as u64).to_le_bytes());
    hasher.update(memory);
    hasher.update(suffix);
    hasher.finalize().into()
}

fn reseed_marker_requires_arrival(marker: u64, restored_floor: u64) -> bool {
    marker > restored_floor
}

impl<B: Backend<A: Vendor>> ControlServer<B> {
    fn preflight_imported_vm_state(
        &self,
        vm_state: &<B::A as Vendor>::Snapshot,
    ) -> Result<(), PortableSnapshotError> {
        let Some(vmm) = self.vmm.as_ref() else {
            return Err(PortableSnapshotError::Malformed(
                "portable VM state restore validation unavailable",
            ));
        };
        vmm.preflight_restore_vm_state(vm_state)
            .map_err(map_restore_preflight_error)
    }

    pub fn new(mut vmm: Vmm<B>, factory: VmmFactory<B>) -> Self {
        let engine = SnapshotEngine::new(vmm.guest_memory().len());
        let seed = vmm.entropy_state().unwrap_or(0);
        let recorded = EnvSpec::seeded(seed);
        vmm.enable_sdk(
            RecordedEnv::new(seed, Box::new(NominalHandler) as Box<dyn ServiceHandler>),
            recorded.config(),
        );
        ControlServer {
            vmm: Some(vmm),
            factory,
            service_factory: nominal_factory(),
            remap_factory: None,
            restore_mode: RestoreMode::Memcpy,
            engine,
            derive_parent: None,
            current_image: None,
            in_place_fallbacks: 0,
            last_restore_bytes_written: 0,
            snaps: BTreeMap::new(),
            next_snap: 1,
            hello_done: false,
            schedule: BTreeMap::new(),
            reseed_schedule: BTreeMap::new(),
            recorded: EnvSpec::seeded(seed),
            schedule_poisoned: None,
            sdk_snaps: BTreeMap::new(),
            timeline_tainted: false,
            tainted_snaps: BTreeSet::new(),
            snapshot_meta: BTreeMap::new(),
            exec_nonce: 0,
            session_trace: Vec::new(),
            session_trace_start: SessionTraceStart::InitialBoot,
            last_seal_dirty_gfns: None,
        }
    }

    pub fn set_service_factory(&mut self, factory: ServiceFactory) {
        self.service_factory = factory;
    }

    pub fn recorded_env(&self) -> &EnvSpec {
        &self.recorded
    }

    pub fn vmm(&self) -> Option<&Vmm<B>> {
        self.vmm.as_ref()
    }

    pub fn vmm_mut(&mut self) -> Option<&mut Vmm<B>> {
        self.vmm.as_mut()
    }

    pub fn session_virtual_time_trace(&self) -> Option<SessionVirtualTimeTrace> {
        let mut segments = self.session_trace.clone();
        if let Some(trace) = self.vmm.as_ref().and_then(Vmm::virtual_time_trace) {
            segments.push(SessionTraceSegment::capture(
                self.session_trace_start,
                trace,
            ));
        }
        (!segments.is_empty()).then(|| SessionVirtualTimeTrace::from_segments(segments))
    }

    pub fn take_session_virtual_time_trace(&mut self) -> Option<SessionVirtualTimeTrace> {
        let mut segments = std::mem::take(&mut self.session_trace);
        if let Some(trace) = self.vmm.as_ref().and_then(Vmm::virtual_time_trace) {
            segments.push(SessionTraceSegment::capture(
                self.session_trace_start,
                trace,
            ));
        }
        (!segments.is_empty()).then(|| SessionVirtualTimeTrace::from_segments(segments))
    }

    fn finish_session_trace_segment(&mut self) {
        if let Some(trace) = self.vmm.as_mut().and_then(Vmm::take_virtual_time_trace) {
            self.session_trace.push(SessionTraceSegment::capture(
                self.session_trace_start,
                &trace,
            ));
        }
    }

    pub fn set_remap_factory(&mut self, factory: RemapVmmFactory<B>) {
        self.remap_factory = Some(factory);
        self.restore_mode = RestoreMode::Remap;
    }

    pub fn set_restore_mode(&mut self, mode: RestoreMode) {
        self.restore_mode = mode;
    }

    pub fn restore_mode(&self) -> RestoreMode {
        self.restore_mode
    }

    pub fn in_place_fallbacks(&self) -> u64 {
        self.in_place_fallbacks
    }

    pub fn last_restore_bytes_written(&self) -> u64 {
        self.last_restore_bytes_written
    }

    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.engine.set_max_chain_len(max_chain_len);
    }

    pub fn snapshot_chain_len(&self, snap: SnapId) -> Option<u32> {
        let id = self.snaps.get(&snap.0)?;
        self.engine.stats(*id).ok().map(|s| s.chain_len)
    }

    pub fn snapshot_store_stats(&self) -> snapshot_store::StoreStats {
        self.engine.store_stats()
    }

    pub fn snapshot_stats(&self, snap: SnapId) -> Option<snapshot_store::SnapStats> {
        self.snaps
            .get(&snap.0)
            .and_then(|id| self.engine.stats(*id).ok())
    }

    pub fn last_seal_dirty_gfns(&self) -> Option<&[u64]> {
        self.last_seal_dirty_gfns.as_deref()
    }

    pub fn latest_snapshot(&self) -> Option<SnapId> {
        self.snaps.last_key_value().map(|(&id, _)| SnapId(id))
    }

    pub fn export_sparse_snapshot(
        &self,
        base: SnapId,
        target: SnapId,
    ) -> Result<SparsePortableSnapshot, PortableSnapshotError> {
        let base_id = *self
            .snaps
            .get(&base.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(base.0))?;
        let target_id = *self
            .snaps
            .get(&target.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(target.0))?;
        let meta = self
            .snapshot_meta
            .get(&target.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(target.0))?;
        if meta.state_blob_suffix.is_empty() {
            return Err(PortableSnapshotError::Malformed(
                "sparse export target lacks a canonical state-blob suffix",
            ));
        }
        let candidates = self.engine.diff_pages(Some(base_id), target_id)?;
        let mut pages = Vec::with_capacity(candidates.len());
        for (gfn, page) in candidates {
            if self.engine.read_page(base_id, gfn)? != page {
                pages.push((gfn, Arc::new(page)));
            }
        }
        let vm_state = self.engine.vm_state_bytes(target_id)?;
        let sidecar = encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state,
            sdk: self.sdk_snaps.get(&target.0).map(|s| &s.channel),
            policy: &meta.policy,
            control_state: &meta.control_state,
            at: meta.at,
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_blob_suffix: &meta.state_blob_suffix,
        })?;
        Ok(SparsePortableSnapshot { pages, sidecar })
    }

    pub fn import_sparse_snapshot(
        &mut self,
        base: SnapId,
        portable: SparsePortableSnapshot,
    ) -> Result<SparsePortableSnapshotReceipt, PortableSnapshotError> {
        self.import_sparse_snapshot_parts(base, &portable.pages, &portable.sidecar)
    }

    pub fn import_sparse_snapshot_parts(
        &mut self,
        base: SnapId,
        pages: &[(u64, Arc<[u8; 4096]>)],
        sidecar: &[u8],
    ) -> Result<SparsePortableSnapshotReceipt, PortableSnapshotError> {
        let base_id = *self
            .snaps
            .get(&base.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(base.0))?;
        let mut previous = None;
        for (gfn, _) in pages {
            if let Some(error) = sparse_page_order_error(previous, *gfn) {
                return Err(PortableSnapshotError::Snapshot(error));
            }
            if *gfn >= self.engine.mem_pages() {
                return Err(PortableSnapshotError::Snapshot(
                    SnapshotError::SparsePageOutOfRange {
                        gfn: *gfn,
                        pages: self.engine.mem_pages(),
                    },
                ));
            }
            previous = Some(*gfn);
        }

        let portable = decode_sparse_sidecar(sidecar)?;
        let decoded = <<B::A as Vendor>::Snapshot as SnapshotRecords>::decode(&portable.vm_state)
            .map_err(SnapshotError::from)?;
        self.preflight_imported_vm_state(&decoded)?;
        if decoded.vtime().snapshot_vns != portable.at {
            return Err(PortableSnapshotError::Malformed(
                "sparse sidecar V-time does not match vendor VM state",
            ));
        }
        if let Some(sdk) = &portable.sdk {
            if usize::try_from(portable.sdk_events).ok() != Some(sdk.events.len()) {
                return Err(PortableSnapshotError::Malformed(
                    "sparse sidecar SDK event count",
                ));
            }
        } else if portable.sdk_events != 0 {
            return Err(PortableSnapshotError::Malformed(
                "sparse sidecar SDK event count",
            ));
        }
        ControlState::decode_for_policy(&portable.control_state, &portable.policy)
            .map_err(PortableSnapshotError::Malformed)?;
        validate_control_hash_suffix(&portable.state_blob_suffix, &portable.control_state)?;
        let id = self.next_snap;
        let next_snap = self
            .next_snap
            .checked_add(1)
            .ok_or(PortableSnapshotError::Malformed("snapshot handle overflow"))?;
        let mut owned_pages = Vec::new();
        owned_pages
            .try_reserve_exact(pages.len())
            .map_err(|_| PortableSnapshotError::Malformed("sparse page allocation"))?;
        for (gfn, page) in pages {
            owned_pages.push((*gfn, **page));
        }
        let store_id =
            self.engine
                .snapshot_sparse_derive(base_id, &owned_pages, &portable.vm_state)?;

        self.next_snap = next_snap;
        self.snaps.insert(id, store_id);
        if let Some(channel) = portable.sdk {
            self.sdk_snaps.insert(
                id,
                SdkSnap {
                    channel,
                    policy: portable.policy.clone(),
                },
            );
        }
        if portable.tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at: portable.at,
                sdk_events: portable.sdk_events,
                trace_events: portable.trace_events,
                trace_schedules: portable.trace_schedules,
                tainted: portable.tainted,
                state_hash: None,
                state_blob_suffix: portable.state_blob_suffix,
                policy: portable.policy,
                control_state: portable.control_state,
            },
        );
        Ok(SparsePortableSnapshotReceipt {
            id: SnapId(id),
            at: Moment(decoded.vtime().snapshot_vns),
            sdk_events: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.sdk_events),
            trace_events: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.trace_events),
            trace_schedules: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.trace_schedules),
            tainted: self.tainted_snaps.contains(&id),
        })
    }

    pub fn export_portable_snapshot<W: Write>(
        &self,
        snap: SnapId,
        writer: W,
    ) -> Result<PortableSnapshotReceipt, PortableSnapshotError> {
        let store_id = *self
            .snaps
            .get(&snap.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(snap.0))?;
        let meta = self
            .snapshot_meta
            .get(&snap.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(snap.0))?;
        let memory = self.engine.materialize(store_id)?;
        let vm_state = self.engine.vm_state_bytes(store_id)?;
        let state_hash = meta
            .state_hash
            .unwrap_or_else(|| hash_state_blob_parts(memory.as_slice(), &meta.state_blob_suffix));
        PortableSnapshotRef {
            memory: memory.as_slice(),
            vm_state,
            sdk: self.sdk_snaps.get(&snap.0).map(|s| &s.channel),
            policy: &meta.policy,
            control_state: &meta.control_state,
            at: meta.at,
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_hash,
        }
        .write_to(writer)?;
        Ok(PortableSnapshotReceipt {
            id: snap,
            at: Moment(meta.at),
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_hash,
        })
    }

    pub fn import_portable_snapshot<R: Read>(
        &mut self,
        reader: R,
    ) -> Result<PortableSnapshotReceipt, PortableSnapshotError> {
        let expected_memory_len = usize::try_from(
            self.engine
                .mem_pages()
                .checked_mul(snapshot_store::PAGE_SIZE as u64)
                .ok_or(PortableSnapshotError::Malformed(
                    "configured memory length overflow",
                ))?,
        )
        .map_err(|_| PortableSnapshotError::Malformed("configured memory length"))?;
        let portable = PortableSnapshot::read_from(reader, expected_memory_len)?;
        let decoded = <<B::A as Vendor>::Snapshot as SnapshotRecords>::decode(&portable.vm_state)
            .map_err(SnapshotError::from)?;
        self.preflight_imported_vm_state(&decoded)?;
        if decoded.vtime().snapshot_vns != portable.at {
            return Err(PortableSnapshotError::Malformed(
                "portable V-time does not match vendor VM state",
            ));
        }
        if portable.sdk.as_ref().map_or(0, |sdk| sdk.events.len()) as u64 != portable.sdk_events {
            return Err(PortableSnapshotError::Malformed("portable SDK event count"));
        }
        ControlState::decode_for_policy(&portable.control_state, &portable.policy)
            .map_err(PortableSnapshotError::Malformed)?;
        let id = self.next_snap;
        let next_snap = self
            .next_snap
            .checked_add(1)
            .ok_or(PortableSnapshotError::Malformed("snapshot handle overflow"))?;
        let store_id = self
            .engine
            .snapshot_base(&portable.memory, &portable.vm_state)?;
        self.next_snap = next_snap;
        self.snaps.insert(id, store_id);
        if let Some(channel) = portable.sdk {
            self.sdk_snaps.insert(
                id,
                SdkSnap {
                    channel,
                    policy: portable.policy.clone(),
                },
            );
        }
        if portable.tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at: portable.at,
                sdk_events: portable.sdk_events,
                trace_events: portable.trace_events,
                trace_schedules: portable.trace_schedules,
                tainted: portable.tainted,
                state_hash: Some(portable.state_hash),
                state_blob_suffix: Vec::new(),
                policy: portable.policy,
                control_state: portable.control_state,
            },
        );
        Ok(PortableSnapshotReceipt {
            id: SnapId(id),
            at: Moment(portable.at),
            sdk_events: portable.sdk_events,
            trace_events: portable.trace_events,
            trace_schedules: portable.trace_schedules,
            tainted: portable.tainted,
            state_hash: portable.state_hash,
        })
    }

    pub fn serve<S: Read + Write>(&mut self, mut stream: S) -> Result<(), ServeError> {
        let mut inbuf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut outbuf: Vec<u8> = Vec::new();
        loop {
            while let Some((seq, req, consumed)) = decode_request(&inbuf)? {
                inbuf.drain(..consumed);
                let reply = self.handle(&req)?;
                outbuf.clear();
                encode_reply(seq, &reply, &mut outbuf)?;
                stream.write_all(&outbuf)?;
                stream.flush()?;
            }
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return if inbuf.is_empty() {
                    Ok(())
                } else {
                    Err(control_proto::ProtocolError::ShortFrame.into())
                };
            }
            inbuf.extend_from_slice(&chunk[..n]);
        }
    }

    #[allow(clippy::result_large_err)]
    pub fn handle(&mut self, req: &Request) -> Result<Result<Reply, ControlError>, ServeError> {
        if !self.hello_done && !matches!(req, Request::Hello(_)) {
            return Ok(Err(ControlError::Unsupported));
        }
        match req {
            Request::Hello(client_caps) => {
                if client_caps.protocol_version != control_proto::APP_PROTOCOL_VERSION {
                    return Ok(Err(ControlError::Unsupported));
                }
                self.hello_done = true;
                Ok(Ok(Reply::Hello(server_caps())))
            }
            Request::Snapshot => self.snapshot(),
            Request::Drop(snap) => Ok(self.drop_snap(*snap)),
            Request::Branch { snap, env } => self.restore(*snap, Some(env)),
            Request::Replay(snap) => self.restore(*snap, None),
            Request::Run { until, resolve } => {
                if let Some(resolution) = resolve {
                    let Some((at, question)) = self
                        .vmm
                        .as_ref()
                        .ok_or(ServeError::Poisoned)?
                        .pending_service_question()
                        .map(|(at, question)| (at, question.clone()))
                    else {
                        return Ok(Err(ControlError::ResolveWithoutDecision));
                    };
                    if resolution.vtime.0 != at
                        || resolution.service != question.service()
                        || resolution.id.0 != question.request_id()
                    {
                        return Ok(Err(ControlError::ResolveWithoutDecision));
                    }
                    if resolution.answer.0.len() > hypercall_proto::MAX_PAYLOAD {
                        return Ok(Err(ControlError::MalformedEnvironment));
                    }
                    let response = match environment::channel::Answer::decode(&resolution.answer.0)
                    {
                        Ok(response) => response,
                        Err(_) => return Ok(Err(ControlError::MalformedEnvironment)),
                    };
                    let mut recorded = self.recorded.clone();
                    recorded.record_answer(
                        at,
                        question.service(),
                        question.request_id(),
                        response.clone(),
                    )?;
                    if recorded.try_encode().is_err() {
                        return Ok(Err(ControlError::MalformedEnvironment));
                    }
                    if let Err(error) = self
                        .vmm
                        .as_mut()
                        .ok_or(ServeError::Poisoned)?
                        .resolve_service_answer(response)
                    {
                        self.vmm = None;
                        return Err(error.into());
                    }
                    self.recorded = recorded;
                }
                self.run(until)
            }
            Request::Hash { scope } => match scope {
                HashScope::Whole => {
                    let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                    let mut suffix = vmm.state_blob_suffix()?;
                    self.capture_control_state().append_hash(&mut suffix);
                    Ok(Ok(Reply::Hash(hash_state_blob_parts(
                        vmm.guest_memory(),
                        &suffix,
                    ))))
                }
                HashScope::Disk | HashScope::Region { .. } => Ok(Err(ControlError::Unsupported)),
            },
            Request::Perturb { fault, at } => Ok(self.perturb(fault, *at)),
            Request::SdkEvents { offset } => {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                Ok(Ok(Reply::SdkEvents(page_sdk_events(
                    vmm.sdk_events(),
                    *offset as usize,
                ))))
            }
            Request::Console { offset } => {
                let serial = self.vmm.as_ref().map(|v| v.serial()).unwrap_or(&[]);
                let (total, chunk) = page_console(serial, *offset as usize);
                Ok(Ok(Reply::Console { total, chunk }))
            }
            Request::Read { gpa, len } => self.read(*gpa, *len),
            Request::Regs => {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                Ok(Ok(Reply::Regs(regs_view(vmm))))
            }
            Request::Exec { cmd, deadline } => self.exec(cmd, *deadline),
            Request::RecordedEnv => Ok(self.recorded_env_reply()),
        }
    }

    #[allow(clippy::result_large_err)]
    fn read(&self, gpa: u64, len: u32) -> Result<Result<Reply, ControlError>, ServeError> {
        if len > READ_CAP {
            return Ok(Err(ControlError::ReadTooLarge { len, cap: READ_CAP }));
        }
        let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
        match vmm.guest_slice(gpa, len as usize) {
            Some(bytes) => Ok(Ok(Reply::Bytes(bytes.to_vec()))),
            None => Ok(Err(ControlError::ReadOutOfRange {
                gpa,
                len,
                ram_len: vmm.guest_memory().len() as u64,
            })),
        }
    }

    fn perturb(
        &mut self,
        fault: &control_proto::HostFault,
        at: control_proto::Moment,
    ) -> Result<Reply, ControlError> {
        if let Some(err) = &self.schedule_poisoned {
            return Err(err.reply());
        }
        let decoded = Effect::decode(&fault.0).map_err(|_| ControlError::MalformedEnvironment)?;
        if !self.vmm.as_ref().is_some_and(|v| v.vtime_wired()) {
            return Err(ControlError::Unsupported);
        }
        let floor = self
            .vmm
            .as_ref()
            .and_then(|v| v.effective_vns())
            .unwrap_or(0);
        self.validate_host_fault(&decoded, at.0, floor)?;
        self.schedule.insert(at.0, decoded);
        Ok(Reply::Unit)
    }

    fn validate_host_fault(&self, fault: &Effect, at: u64, floor: u64) -> Result<(), ControlError> {
        if self.schedule.contains_key(&at) || self.recorded.effects().contains_key(&at) {
            return Err(ControlError::PerturbMomentTaken { at });
        }
        self.check_fault_admissible(fault, at, floor)
    }

    fn check_fault_admissible(
        &self,
        fault: &Effect,
        at: u64,
        floor: u64,
    ) -> Result<(), ControlError> {
        let vmm = self.vmm.as_ref().ok_or(ControlError::Unsupported)?;
        if !vmm.vtime_wired() {
            return Err(ControlError::Unsupported);
        }
        if at < floor {
            return Err(ControlError::PerturbPastMoment { at, floor });
        }
        match fault {
            Effect::WriteMemory { gpa, bytes } | Effect::XorMemory { gpa, bytes } => {
                if vmm.guest_slice(*gpa, bytes.len()).is_none() {
                    return Err(ControlError::PerturbOutOfRange {
                        gpa: *gpa,
                        ram_len: vmm.guest_memory().len() as u64,
                    });
                }
            }
            Effect::InjectInterrupt { vector } => {
                if let Err(reject) = <B::A as Vendor>::check_wire_interrupt(vmm, *vector) {
                    return Err(match reject {
                        InterruptReject::NoFabric | InterruptReject::OutOfRange => {
                            ControlError::Unsupported
                        }
                        InterruptReject::Reserved { vector } => {
                            ControlError::PerturbReservedVector { vector }
                        }
                    });
                }
            }
        }
        Ok(())
    }

    fn seal_into_store(
        engine: &mut SnapshotEngine,
        vmm: &mut Vmm<B>,
        parent: Option<SnapshotId>,
        blob: &[u8],
    ) -> Result<(SnapshotId, bool, Option<Vec<u64>>), SnapshotError> {
        if let Some(parent) = parent {
            let chain_ok = engine
                .stats(parent)
                .is_ok_and(|s| s.chain_len < engine.max_chain_len());
            if chain_ok && let Some(gfns) = vmm.drain_dirty_pages() {
                return match engine.snapshot_derive(parent, vmm.guest_memory(), Some(&gfns), blob) {
                    Ok(id) => Ok((id, true, Some(gfns))),
                    Err(_) => engine
                        .snapshot_base(vmm.guest_memory(), blob)
                        .map(|id| (id, true, Some(gfns))),
                };
            }
            if !chain_ok
                && engine.stats(parent).is_ok()
                && let Some(gfns) = vmm.drain_dirty_pages()
            {
                return match engine.snapshot_flatten(parent, vmm.guest_memory(), &gfns, blob) {
                    Ok(id) => Ok((id, true, Some(gfns))),
                    Err(_) => engine
                        .snapshot_base(vmm.guest_memory(), blob)
                        .map(|id| (id, true, Some(gfns))),
                };
            }
        }
        engine
            .snapshot_base(vmm.guest_memory(), blob)
            .map(|id| (id, false, None))
    }

    fn snapshot(&mut self) -> Result<Result<Reply, ControlError>, ServeError> {
        self.last_seal_dirty_gfns = None;
        let control = self.capture_control_state();
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        let vm_state = match vmm.save_vm_state() {
            Ok(s) => s,
            Err(VmmError::ContractViolation(reason)) => {
                return Ok(Err(ControlError::SnapshotRefused { reason }));
            }
            Err(e) => return Err(e.into()),
        };
        let sdk_channel = match vmm.sdk_snapshot() {
            Ok(state) => state,
            Err(error) => {
                self.vmm = None;
                return Err(ServeError::Service(error));
            }
        };
        let mut state_blob_suffix = vmm.state_blob_suffix()?;
        control.append_hash(&mut state_blob_suffix);
        let blob = vm_state.encode().map_err(SnapshotError::from)?;
        let at = vm_state.vtime().snapshot_vns;
        let sdk_events = vmm.sdk_events().len() as u64;
        let (trace_events, trace_schedules) = vmm.virtual_time_trace().map_or((0, 0), |trace| {
            (
                trace.normalized_log().events.len() as u64,
                trace.schedule().len() as u64,
            )
        });
        let policy = self.recorded.config().clone();
        let parent = self.derive_parent.take();
        let (store_id, window_consumed, dirty_gfns) =
            Self::seal_into_store(&mut self.engine, vmm, parent, &blob)?;
        self.last_seal_dirty_gfns = dirty_gfns;
        let tracked = window_consumed || vmm.reset_dirty_tracking();
        self.derive_parent = tracked.then_some(store_id);
        self.current_image = tracked.then_some(store_id);

        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, store_id);
        if let Some(channel) = sdk_channel {
            let policy = self.recorded.config().clone();
            self.sdk_snaps.insert(id, SdkSnap { channel, policy });
        }
        if self.timeline_tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at,
                sdk_events,
                trace_events,
                trace_schedules,
                tainted: self.timeline_tainted,
                state_hash: None,
                state_blob_suffix,
                policy,
                control_state: control.encode(),
            },
        );
        Ok(Ok(Reply::Snapshot {
            id: SnapId(id),
            at: Moment(at),
            sdk_events,
            tainted: self.timeline_tainted,
        }))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<Reply, ControlError> {
        let Some(store_id) = self.snaps.remove(&snap.0) else {
            return Err(ControlError::UnknownSnapshot(snap));
        };
        self.sdk_snaps.remove(&snap.0);
        self.tainted_snaps.remove(&snap.0);
        self.snapshot_meta.remove(&snap.0);
        if self.current_image == Some(store_id) {
            self.current_image = None;
            self.derive_parent = None;
        }
        if self.engine.release(store_id).is_err() {
            return Err(ControlError::UnknownSnapshot(snap));
        }
        self.engine.gc();
        Ok(Reply::Unit)
    }

    fn restore_in_place(
        &mut self,
        store_id: SnapshotId,
        vm_state: &<B::A as Vendor>::Snapshot,
    ) -> Result<u64, ()> {
        let from = self.current_image;
        let mut pages: BTreeMap<u64, [u8; 4096]> = self
            .engine
            .diff_pages(from, store_id)
            .map_err(|_| ())?
            .into_iter()
            .collect();

        let dirty = {
            let vmm = self.vmm.as_mut().ok_or(())?;
            vmm.retire_pending_completion().map_err(|_| ())?;
            vmm.drain_dirty_pages()
        };
        match dirty {
            Some(gfns) => {
                for gfn in gfns {
                    if let std::collections::btree_map::Entry::Vacant(entry) = pages.entry(gfn) {
                        entry.insert(self.engine.read_page(store_id, gfn).map_err(|_| ())?);
                    }
                }
            }
            None if from.is_none() => {}
            None => return Err(()),
        }

        let pages: Vec<(u64, [u8; 4096])> = pages.into_iter().collect();
        let bytes = u64::try_from(pages.len())
            .ok()
            .and_then(|count| count.checked_mul(4096))
            .ok_or(())?;
        let vmm = self.vmm.as_mut().ok_or(())?;
        vmm.write_guest_pages(&pages).map_err(|_| ())?;
        vmm.restore_vm_state(vm_state).map_err(|_| ())?;
        Ok(bytes)
    }

    fn install_recovery_boot(&mut self, fresh: Vmm<B>) {
        self.vmm = Some(fresh);
        self.derive_parent = None;
        self.current_image = None;
        self.last_restore_bytes_written = 0;
        self.reset_schedule_to_fresh_vm();
        self.timeline_tainted = false;
        self.session_trace_start = SessionTraceStart::RecoveryBoot;
        let sdk_env = RecordedEnv::new(
            self.recorded.seed(),
            Box::new(NominalHandler) as Box<dyn ServiceHandler>,
        );
        let sdk_policy = self.recorded.config().clone();
        if let Some(vmm) = self.vmm.as_mut() {
            vmm.enable_sdk(sdk_env, &sdk_policy);
        }
    }

    fn restore(
        &mut self,
        snap: SnapId,
        env: Option<&control_proto::Reproducer>,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        let mut host: Vec<(u64, Effect)> = Vec::new();
        let mut reseeds: BTreeMap<u64, u64> = BTreeMap::new();
        let mut payloads: Option<Vec<Vec<u8>>> = None;
        let mut answers = BTreeMap::new();
        let mut env_policy: Option<ServiceConfig> = None;
        let seed = match env {
            None => None,
            Some(env) => {
                if env.blob_version != EnvSpec::BLOB_VERSION {
                    return Ok(Err(ControlError::BadEnvVersion(env.blob_version)));
                }
                let spec = match EnvSpec::decode(&env.bytes) {
                    Ok(spec) => spec,
                    Err(_) => return Ok(Err(ControlError::MalformedEnvironment)),
                };
                host = spec
                    .effects()
                    .iter()
                    .map(|(&at, effect)| (at, effect.clone()))
                    .collect();
                reseeds = spec.reseeds().clone();
                payloads = spec.payloads().map(<[Vec<u8>]>::to_vec);
                env_policy = Some(spec.config().clone());
                answers = spec.answers().clone();
                Some(spec.seed())
            }
        };
        let restore_config = env_policy
            .clone()
            .or_else(|| self.sdk_snaps.get(&snap.0).map(|s| s.policy.clone()))
            .unwrap_or_default();
        let prepared_handler = match (self.service_factory)(&restore_config) {
            Ok(handler)
                if handler.identity() == restore_config.identity
                    && handler.configuration() == restore_config.configuration =>
            {
                handler
            }
            _ => return Ok(Err(ControlError::Unsupported)),
        };
        let mut prepared_env = RecordedEnv::new(seed.unwrap_or(0), prepared_handler);
        if let Some(snapshot) = self.sdk_snaps.get(&snap.0).filter(|_| seed.is_none())
            && snapshot
                .channel
                .recorded
                .restore_into(&mut prepared_env)
                .is_err()
        {
            return Ok(Err(ControlError::RestoreFailed));
        }
        if seed.is_some() && prepared_env.set_payloads(payloads.clone()).is_err() {
            return Ok(Err(ControlError::MalformedEnvironment));
        }
        for (&(at, service, request), answer) in &answers {
            prepared_env.record_service_request(at, service, request, answer.clone());
        }
        let Some(&store_id) = self.snaps.get(&snap.0) else {
            return Ok(Err(ControlError::UnknownSnapshot(snap)));
        };
        let Ok(vm_state) = self.engine.vm_state::<<B::A as Vendor>::Snapshot>(store_id) else {
            return Ok(Err(ControlError::RestoreFailed));
        };
        let source_control = match self.snapshot_meta.get(&snap.0) {
            Some(meta) => {
                match ControlState::decode_for_policy(&meta.control_state, &meta.policy) {
                    Ok(state) => state,
                    Err(_) => return Ok(Err(ControlError::RestoreFailed)),
                }
            }
            None => return Ok(Err(ControlError::RestoreFailed)),
        };
        let mut mapping = if restore_defers_materialization(self.restore_mode) {
            None
        } else {
            let Ok(mapping) = self.engine.materialize(store_id) else {
                return Ok(Err(ControlError::RestoreFailed));
            };
            Some(mapping)
        };
        let restored_floor = vm_state.vtime().snapshot_vns;
        let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for (m, fault) in &host {
            if let Err(e) = self.check_fault_admissible(fault, *m, restored_floor) {
                return Ok(Err(e));
            }
            if !seen.insert(*m) {
                return Ok(Err(ControlError::PerturbMomentTaken { at: *m }));
            }
        }
        for &m in reseeds.keys() {
            if m < restored_floor {
                return Ok(Err(ControlError::PerturbPastMoment {
                    at: m,
                    floor: restored_floor,
                }));
            }
            if reseed_marker_requires_arrival(m, restored_floor)
                && !self.vmm.as_ref().is_some_and(|v| v.vtime_wired())
            {
                return Ok(Err(ControlError::Unsupported));
            }
        }
        self.finish_session_trace_segment();
        let in_place = self.restore_mode == RestoreMode::InPlace;
        let in_place_result = in_place.then(|| self.restore_in_place(store_id, &vm_state));
        let used_in_place = matches!(in_place_result, Some(Ok(_)));
        if let Some(Ok(bytes)) = in_place_result {
            self.last_restore_bytes_written = bytes;
        } else if in_place {
            self.in_place_fallbacks = self.in_place_fallbacks.saturating_add(1);
            self.last_restore_bytes_written = 0;
            self.current_image = None;
        }

        let use_remap = self.restore_mode != RestoreMode::Memcpy && self.remap_factory.is_some();
        let (mut fresh, restore_result) = if used_in_place {
            let fresh = self.vmm.take().ok_or(ServeError::Poisoned)?;
            (fresh, Ok(()))
        } else {
            if mapping.is_none() {
                mapping = match self.engine.materialize(store_id) {
                    Ok(mapping) => Some(mapping),
                    Err(_) => {
                        self.vmm = None;
                        let recovery = (self.factory)()?;
                        self.install_recovery_boot(recovery);
                        return Ok(Err(ControlError::RestoreFailed));
                    }
                };
            }
            self.vmm = None;
            self.current_image = None;
            self.last_restore_bytes_written = 0;
            let mapping = mapping.expect("fresh restore materialized the target image");
            if use_remap {
                let factory = self
                    .remap_factory
                    .as_mut()
                    .expect("use_remap checked is_some");
                let mut fresh = factory(mapping)?;
                let result = fresh.restore_vm_state(&vm_state);
                (fresh, result)
            } else {
                let mut fresh = (self.factory)()?;
                let result = fresh.restore_snapshot(mapping.as_slice(), &vm_state);
                (fresh, result)
            }
        };
        match restore_result {
            Ok(()) => {}
            Err(error) if restore_error_is_precommit(&error) => {
                if use_remap {
                    drop(fresh);
                    fresh = (self.factory)()?;
                }
                self.install_recovery_boot(fresh);
                return Ok(Err(ControlError::RestoreFailed));
            }
            Err(error) => return Err(error.into()),
        }
        if let Some(seed) = seed {
            if reseeds.is_empty() {
                fresh.reseed_entropy(seed)?;
            } else if let Some(&s0) = reseeds.get(&restored_floor) {
                fresh.reseed_entropy(s0)?;
            }
        }
        self.vmm = Some(fresh);
        self.session_trace_start = if seed.is_some() {
            SessionTraceStart::Branch { snapshot: snap.0 }
        } else {
            SessionTraceStart::Replay { snapshot: snap.0 }
        };
        self.timeline_tainted = self.tainted_snaps.contains(&snap.0);
        self.reset_schedule_to_fresh_vm();
        let sdk_snap = self.sdk_snaps.get(&snap.0).cloned();
        let restore_policy = env_policy.or_else(|| sdk_snap.as_ref().map(|s| s.policy.clone()));
        if let Some(policy) = restore_policy {
            self.set_recorded_policy(policy);
        }
        let active_payloads = if seed.is_some() {
            payloads
        } else {
            sdk_snap
                .as_ref()
                .and_then(|snap| snap.channel.remaining_payloads())
        };
        if seed.is_none()
            && let Some(snapshot) = &sdk_snap
        {
            answers = snapshot
                .channel
                .recorded
                .answers()
                .map(|(key, value)| (key, value.clone()))
                .collect();
        }
        for ((at, service, request), answer) in answers {
            self.recorded
                .record_answer(at, service, request, answer)
                .map_err(ServeError::Service)?;
        }
        self.recorded.set_payloads(active_payloads);
        if seed.is_some() {
            prepared_env.reseed(self.recorded.seed());
        }
        let sdk_env = prepared_env;
        let sdk_policy = self.recorded.config().clone();
        self.vmm
            .as_mut()
            .ok_or(ServeError::Poisoned)?
            .enable_sdk(sdk_env, &sdk_policy);
        if let Some(s) = sdk_snap {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            if seed.is_some() {
                vmm.sdk_restore_events(&s.channel);
            } else {
                if let Err(error) = vmm.sdk_restore(&s.channel) {
                    self.vmm = None;
                    return Err(ServeError::Service(error));
                }
            }
        }
        for (m, fault) in host {
            self.schedule.insert(m, fault);
        }
        if !reseeds.is_empty() {
            use std::ops::Bound;
            for (&m, &s) in reseeds.range((Bound::Excluded(restored_floor), Bound::Unbounded)) {
                self.reseed_schedule.insert(m, s);
            }
            let stream = self
                .vmm
                .as_ref()
                .and_then(|v| v.entropy_state())
                .unwrap_or(0);
            self.recorded.record_reseed(restored_floor, stream);
        }
        if let Some(control) = source_control {
            self.exec_nonce = control.exec_nonce;
            if seed.is_none() {
                self.schedule = control.pending.effects().clone();
                self.reseed_schedule = control.pending.reseeds().clone();
                self.schedule_poisoned = control.poisoned;
                self.recorded = control.recorded;
            }
        }
        {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            let tracked = vmm.reset_dirty_tracking();
            self.derive_parent = tracked.then_some(store_id);
            self.current_image = tracked.then_some(store_id);
        }
        Ok(Ok(Reply::Unit))
    }

    fn set_recorded_policy(&mut self, policy: ServiceConfig) {
        self.recorded.set_config(policy);
    }

    fn capture_control_state(&self) -> ControlState {
        let mut pending = EnvSpec::seeded(0);
        for (&at, effect) in &self.schedule {
            pending.record_effect(at, effect.clone());
        }
        for (&at, &seed) in &self.reseed_schedule {
            pending.record_reseed(at, seed);
        }
        ControlState {
            recorded: self.recorded.clone(),
            pending,
            poisoned: self.schedule_poisoned,
            exec_nonce: self.exec_nonce,
        }
    }

    fn reset_schedule_to_fresh_vm(&mut self) {
        self.exec_nonce = 0;
        self.schedule.clear();
        self.reseed_schedule.clear();
        self.schedule_poisoned = None;
        let seed = self
            .vmm
            .as_ref()
            .and_then(|v| v.entropy_state())
            .unwrap_or(0);
        self.recorded = EnvSpec::seeded(seed);
    }

    fn run(
        &mut self,
        until: &control_proto::StopConditions,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        if let Some(err) = &self.schedule_poisoned {
            return Ok(Err(err.reply()));
        }
        loop {
            if let Some((moment, question)) = self
                .vmm
                .as_ref()
                .ok_or(ServeError::Poisoned)?
                .pending_service_question()
            {
                let mut ctx = question.service().to_le_bytes().to_vec();
                ctx.extend(question.payload());
                return Ok(Ok(Reply::Stop(StopReason::Decision {
                    vtime: control_proto::Moment(moment),
                    id: control_proto::DecisionId(question.request_id()),
                    ctx,
                })));
            }
            let vns = {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                vmm.effective_vns().unwrap_or(0)
            };

            loop {
                let next_reseed = self.reseed_schedule.range(..=vns).next().map(|(&m, _)| m);
                let next_fault = self.schedule.range(..=vns).next().map(|(&m, _)| m);
                let (m, is_reseed) = match (next_reseed, next_fault) {
                    (Some(r), Some(f)) if r <= f => (r, true),
                    (Some(_) | None, Some(f)) => (f, false),
                    (Some(r), None) => (r, true),
                    (None, None) => break,
                };
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                if is_reseed {
                    let seed = self.reseed_schedule.remove(&m).expect("range key exists");
                    vmm.reseed_entropy(seed).map_err(ServeError::Vmm)?;
                    self.recorded.record_reseed(m, seed);
                } else {
                    let fault = self.schedule.remove(&m).expect("range key exists");
                    vmm.apply_effect(&fault).map_err(ServeError::Vmm)?;
                    self.recorded.record_effect(m, fault);
                }
            }

            if until.on.armed(control_proto::class_bit::SNAPSHOT_POINT) {
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                if vmm.take_snapshot_point() {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    return Ok(Ok(Reply::Stop(StopReason::SnapshotPoint {
                        vtime: Moment(vns),
                    })));
                }
            }

            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            let vns = vmm.effective_vns().unwrap_or(0);
            if let Some(deadline) = until.deadline
                && vns >= deadline.0
            {
                return Ok(Ok(Reply::Stop(StopReason::Deadline { vtime: Moment(vns) })));
            }

            let next_host_event = [
                self.schedule.keys().next().copied(),
                self.reseed_schedule.keys().next().copied(),
            ]
            .into_iter()
            .flatten()
            .min();
            vmm.set_idle_wake_vns(next_host_event);

            match vmm.step()? {
                Step::Continued => {}
                Step::SdkStop => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    let stop = vmm.take_sdk_stop();
                    if matches!(stop, Some(SdkStop::Decision { .. })) {
                        return Ok(Ok(Reply::Stop(sdk_stop_to_reason(
                            stop.expect("decision is present"),
                            vns,
                        ))));
                    }
                    let staged = [
                        self.schedule.keys().next().copied(),
                        self.reseed_schedule.keys().next().copied(),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    if let Some(m) = staged {
                        let failure = ScheduleFailure {
                            moment: m,
                            vtime: vns,
                        };
                        self.schedule_poisoned = Some(failure);
                        return Ok(Err(failure.reply()));
                    }
                    let reason = match stop {
                        Some(sdk_stop) => sdk_stop_to_reason(sdk_stop, vns),
                        None => StopReason::Quiescent { vtime: Moment(vns) },
                    };
                    if matches!(reason, StopReason::Assertion { .. })
                        && !until.on.armed(control_proto::class_bit::ASSERTION)
                    {
                        continue;
                    }
                    return Ok(Ok(Reply::Stop(reason)));
                }
                Step::Terminal(reason) => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    let staged = [
                        self.schedule.keys().next().copied(),
                        self.reseed_schedule.keys().next().copied(),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    if let Some(m) = staged {
                        let failure = ScheduleFailure {
                            moment: m,
                            vtime: vns,
                        };
                        self.schedule_poisoned = Some(failure);
                        return Ok(Err(failure.reply()));
                    }
                    return Ok(Ok(Reply::Stop(map_terminal(reason, vns))));
                }
            }
        }
    }

    fn exec(
        &mut self,
        cmd: &str,
        deadline: Moment,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        self.timeline_tainted = true;
        let nonce = self.exec_nonce;
        self.exec_nonce = self.exec_nonce.wrapping_add(1);

        let mut session = ExecSession::new(cmd, nonce);
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        vmm.inject_serial_input(session.input());
        let mut cursor = vmm.serial_output().len();

        loop {
            let out = vmm.serial_output();
            if out.len() > cursor {
                session.feed(&out[cursor..]);
                cursor = out.len();
            }
            if session.is_done() {
                break;
            }
            let vns = vmm.effective_vns().unwrap_or(0);
            if vns >= deadline.0 {
                session.finish_timeout();
                break;
            }
            match vmm.step()? {
                Step::Continued => {}
                Step::SdkStop => {
                    if vmm.pending_service_question().is_some() {
                        return Ok(Err(ControlError::Unsupported));
                    }
                    let _ = vmm.take_sdk_stop();
                }
                Step::Terminal(_) => {
                    let out = vmm.serial_output();
                    if out.len() > cursor {
                        session.feed(&out[cursor..]);
                    }
                    session.finish_timeout();
                    break;
                }
            }
        }
        let outcome = session.into_outcome();
        Ok(Ok(Reply::ExecResult {
            output: outcome.output,
            ok: outcome.ok,
        }))
    }

    fn recorded_env_reply(&self) -> Result<Reply, ControlError> {
        if self.timeline_tainted {
            return Err(ControlError::Tainted);
        }
        let mut recorded = self.recorded.clone();
        recorded.set_payloads(self.vmm.as_ref().and_then(Vmm::sdk_remaining_payloads));
        Ok(Reply::Recorded(Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: recorded
                .try_encode()
                .map_err(|_| ControlError::MalformedEnvironment)?,
        }))
    }
}

fn page_sdk_events(all: &[(u64, u32, Vec<u8>)], offset: usize) -> Vec<(u64, u32, Vec<u8>)> {
    const REPLY_OVERHEAD: usize = 6;
    let start = offset.min(all.len());
    let mut page = Vec::new();
    let mut body = REPLY_OVERHEAD;
    for ev in &all[start..] {
        let ev_size = 8 + 4 + 4 + ev.2.len();
        if !page.is_empty() && body + ev_size > control_proto::MAX_FRAME_LEN {
            break;
        }
        body += ev_size;
        page.push(ev.clone());
    }
    page
}

fn page_console(serial: &[u8], offset: usize) -> (u32, Vec<u8>) {
    const REPLY_OVERHEAD: usize = 2 + 4 + 4;
    let total = serial.len().min(u32::MAX as usize) as u32;
    let start = offset.min(serial.len());
    let cap = control_proto::MAX_FRAME_LEN.saturating_sub(REPLY_OVERHEAD);
    let end = (start.saturating_add(cap)).min(serial.len());
    (total, serial[start..end].to_vec())
}

fn regs_view<B: Backend<A: Vendor>>(vmm: &Vmm<B>) -> RegsView {
    let vns = vmm.effective_vns().unwrap_or(0);
    let mut view = <B::A as Vendor>::regs_view(&vmm.inspect_vcpu());
    view.moment = Moment(vns);
    view.vtime = vns;
    view
}

fn sdk_stop_to_reason(stop: SdkStop, vns: u64) -> StopReason {
    let vtime = Moment(vns);
    match stop {
        SdkStop::Quiescent => StopReason::Quiescent { vtime },
        SdkStop::Decision {
            moment, question, ..
        } => {
            let mut ctx = question.service().to_le_bytes().to_vec();
            ctx.extend(question.payload());
            StopReason::Decision {
                vtime: Moment(moment),
                id: control_proto::DecisionId(question.request_id()),
                ctx,
            }
        }
        SdkStop::Assertion { id, data } => StopReason::Assertion {
            vtime,
            ev: EventRef { id, data },
        },
    }
}

fn map_terminal(reason: TerminalReason, vns: u64) -> StopReason {
    let vtime = Moment(vns);
    match reason {
        TerminalReason::Idle | TerminalReason::DebugExit { code: 0 } => {
            StopReason::Quiescent { vtime }
        }
        TerminalReason::DebugExit { code } => StopReason::Crash {
            vtime,
            info: CrashInfo {
                kind: CrashKind::Panic,
                detail: vec![code],
            },
        },
        TerminalReason::Shutdown => StopReason::Crash {
            vtime,
            info: CrashInfo {
                kind: CrashKind::Shutdown,
                detail: b"backend shutdown exit (triple fault or guest-initiated shutdown)"
                    .to_vec(),
            },
        },
        TerminalReason::SdkStop => {
            unreachable!(
                "SdkStop is surfaced by the run loop's Step::SdkStop arm, not map_terminal"
            )
        }
    }
}

#[cfg(test)]
mod tests {

    use std::collections::BTreeMap;

    use control_proto::{
        Answer, CapFlags, ControlError, CrashKind, HashScope, HostFault, Moment, READ_CAP, Reply,
        Reproducer, Request, Resolution, SnapId, StopConditions, StopMask, StopReason,
    };
    use environment::{
        channel::{Effect as EnvHostEffect, RecordedEnv},
        input_spec::{InputSpec as EnvSpec, ServiceConfig, nominal_factory},
    };

    #[derive(Clone)]
    struct TestService {
        response: Vec<u8>,
        calls: u64,
    }
    impl environment::channel::ServiceHandler for TestService {
        fn identity(&self) -> &[u8] {
            b"test-service-v1"
        }
        fn configuration(&self) -> &[u8] {
            &self.response
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            self.calls += 1;
            Ok(environment::channel::ServiceResponse::Answered(
                environment::channel::Answer::Data(self.response.clone()),
            ))
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Ok(self.calls.to_le_bytes().to_vec())
        }
        fn restore_state(
            &mut self,
            state: &[u8],
        ) -> Result<(), environment::channel::ChannelError> {
            self.calls = u64::from_le_bytes(
                state
                    .try_into()
                    .map_err(|_| environment::channel::ChannelError::Malformed)?,
            );
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }
    fn with_test_service<B: Backend<A: Vendor>>(mut server: ControlServer<B>) -> ControlServer<B> {
        let nominal = nominal_factory();
        server.set_service_factory(std::sync::Arc::new(move |config| {
            if config.identity == b"test-service-v1" {
                Ok(Box::new(TestService {
                    response: config.configuration.clone(),
                    calls: 0,
                }))
            } else {
                nominal(config)
            }
        }));
        server
    }
    use vm_state::VmState;
    use vmm_backend::{
        Arm64, Arm64Policy, Backend, CommonExit, Exit, MockArm64Backend, MockBackend, X86, X86Exit,
        X86Policy,
    };

    use proptest::prelude::*;

    #[cfg(all(target_os = "linux", not(miri)))]
    use super::host_minor_faults;
    #[cfg(target_os = "linux")]
    use super::host_minor_faults_with;
    use super::{
        ControlServer, ServeError, page_console, page_sdk_events, reseed_marker_requires_arrival,
        restore_defers_materialization, restore_error_is_precommit, server_caps,
        sparse_page_order_error,
    };
    use crate::control_state::ScheduleFailure;
    use crate::portable_snapshot::PortableSnapshotError;
    use crate::vendor::Vendor;
    use crate::vendor::x86::contract_vclock_config;
    use crate::vmm::{GuestRam, Vmm, VmmError, VtimeWiring};

    #[cfg(target_os = "linux")]
    #[test]
    fn host_minor_fault_counter_initializes_through_the_miri_seam() {
        let faults = host_minor_faults_with(|usage| {
            // SAFETY: every field in Linux's integer/timeval-only `rusage`
            // representation admits zero, producing a valid initialized value.
            let mut fixture: libc::rusage = unsafe { std::mem::zeroed() };
            fixture.ru_minflt = 77;
            // SAFETY: the seam supplies one live, correctly aligned, writable
            // `rusage` pointer, and this closure initializes it exactly once.
            unsafe { usage.write(fixture) };
            0
        });
        assert_eq!(faults, Some(77));
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn host_minor_fault_counter_reports_the_live_process_counter() {
        assert!(host_minor_faults().is_some_and(|faults| faults > 1));
    }

    const BIG_RAM: usize = if cfg!(miri) { 0x1_0000 } else { 0x2_0000 };

    const RAM: usize = 0x4000;

    fn vmm_at_sync(exits: Vec<Exit<X86>>, work: u64, seed: u64) -> Vmm<MockBackend> {
        vmm_at_sync_from(MockBackend::new(), exits, work, seed)
    }

    fn vmm_at_sync_from(
        mut m: MockBackend,
        exits: Vec<Exit<X86>>,
        work: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut exits_with_sync = vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })];
        exits_with_sync.extend(exits);
        m.extend_exits(exits_with_sync);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        let mut cfg = contract_vclock_config();
        cfg.vns_base = work.saturating_sub(1);
        v.wire_vtime(VtimeWiring::new_virtual_time(cfg, seed).unwrap());
        v.wire_snapshot_hashing();
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        let mut image = vec![0u8; RAM];
        image[..12].copy_from_slice(b"SERVER_BOOT\n");
        v.restore_guest_memory(&image).unwrap();
        assert_eq!(v.step().unwrap(), crate::vmm::Step::Continued);
        v
    }

    fn server(fork_exits: Vec<Exit<X86>>) -> ControlServer<MockBackend> {
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(fork_exits.clone());
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            v.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    #[derive(Clone)]
    struct BrokenCapture;
    impl environment::channel::ServiceHandler for BrokenCapture {
        fn identity(&self) -> &[u8] {
            b"test-broken-capture"
        }
        fn configuration(&self) -> &[u8] {
            &[]
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            Ok(environment::channel::ServiceResponse::Answered(
                environment::channel::Answer::Nominal,
            ))
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Err(environment::channel::ChannelError::Handler(
                "capture failed".into(),
            ))
        }
        fn restore_state(&mut self, _: &[u8]) -> Result<(), environment::channel::ChannelError> {
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }
    #[test]
    fn extension_capture_failure_cannot_create_a_snapshot_or_hash() {
        let mut s = server(vec![]);
        let config = ServiceConfig {
            identity: b"test-broken-capture".to_vec(),
            configuration: vec![],
        };
        s.vmm.as_mut().unwrap().enable_sdk(
            environment::channel::RecordedEnv::new(0, Box::new(BrokenCapture)),
            &config,
        );
        assert!(s.vmm.as_ref().unwrap().state_hash().is_err());
        assert!(matches!(s.snapshot(), Err(ServeError::Service(_))));
        assert!(s.vmm.is_none());
        assert!(s.sdk_snaps.is_empty());
    }

    #[test]
    fn vmm_mut_reaches_the_live_vm() {
        let mut s = server(Vec::new());
        let shared = s.vmm().expect("live VM") as *const Vmm<MockBackend>;
        let exclusive = s.vmm_mut().expect("live VM") as *mut Vmm<MockBackend> as *const _;
        assert!(std::ptr::eq(shared, exclusive));
    }

    fn server_tracked() -> ControlServer<MockBackend> {
        let mut m = MockBackend::new();
        m.enable_dirty_tracking();
        let live = vmm_at_sync_from(m, vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            v.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    fn server_with_remap(
        fork_exits: Vec<Exit<X86>>,
        sabotage_lapic: bool,
    ) -> ControlServer<MockBackend> {
        let mut s = server(fork_exits.clone());
        let remap: super::RemapVmmFactory<MockBackend> = Box::new(move |mapping| {
            let m = MockBackend::with_exits(fork_exits.clone());
            let mut v =
                crate::vendor::x86::bringup::compose_restore_target(m, mapping, !sabotage_lapic)?;
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        s.set_remap_factory(remap);
        s
    }

    fn hello(server: &mut ControlServer<MockBackend>) {
        let reply = server.handle(&Request::Hello(server_caps())).unwrap();
        assert_eq!(reply, Ok(Reply::Hello(server_caps())));
    }

    fn snap(server: &mut ControlServer<MockBackend>) -> SnapId {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    fn snap_cut(server: &mut ControlServer<MockBackend>) -> (SnapId, u64, u64, bool) {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot {
                id,
                at,
                sdk_events,
                tainted,
            }) => (id, at.0, sdk_events, tainted),
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    fn seeded_env(seed: u64) -> Reproducer {
        let spec = EnvSpec::seeded(seed);
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    fn payload_env(seed: u64, payloads: Vec<Vec<u8>>) -> Reproducer {
        let mut spec = EnvSpec::seeded(seed);
        spec.set_payloads(Some(payloads));
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    fn payload_server() -> ControlServer<MockBackend> {
        let mut backend = MockBackend::with_exits([Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
        backend
            .set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
        let mut live = Vmm::new(backend, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0xBA5E).unwrap());
        live.wire_snapshot_hashing();
        live.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        live.restore_guest_memory(&vec![0_u8; BIG_RAM]).unwrap();
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        let factory = Box::new(|| {
            let mut backend = MockBackend::new();
            backend
                .set_policy(&X86Policy {
                    cpuid: vmm_backend::CpuidModel::default(),
                    msr_filter: vmm_backend::MsrFilter::default(),
                })
                .unwrap();
            let mut vmm = Vmm::new(backend, GuestRam::new(BIG_RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            vmm.wire_snapshot_hashing();
            vmm.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(vmm)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    #[derive(Clone)]
    struct ExternalService(u64);
    impl environment::channel::ServiceHandler for ExternalService {
        fn identity(&self) -> &[u8] {
            b"test-external-v1"
        }
        fn configuration(&self) -> &[u8] {
            &[]
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            self.0 += 1;
            Ok(environment::channel::ServiceResponse::External)
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Ok(self.0.to_le_bytes().to_vec())
        }
        fn restore_state(
            &mut self,
            bytes: &[u8],
        ) -> Result<(), environment::channel::ChannelError> {
            self.0 = u64::from_le_bytes(
                bytes
                    .try_into()
                    .map_err(|_| environment::channel::ChannelError::Malformed)?,
            );
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "restores the complete VM through snapshot-store mmap; pure service codec and state transitions run under Miri"
    )]
    fn host_service_response_is_stopped_recorded_and_replayed() {
        use environment::channel::{Answer as ServiceAnswer, RecordedEnv};
        let mut s = payload_server();
        hello(&mut s);
        let config = ServiceConfig {
            identity: b"test-external-v1".to_vec(),
            configuration: vec![],
        };
        let expected_config = config.clone();
        s.set_service_factory(std::sync::Arc::new(move |config| {
            if config != &expected_config {
                return Err(environment::channel::ChannelError::Malformed);
            }
            Ok(Box::new(ExternalService(0)))
        }));
        s.recorded.set_config(config.clone());
        s.vmm.as_mut().unwrap().enable_sdk(
            RecordedEnv::new(s.recorded.seed(), Box::new(ExternalService(0))),
            &config,
        );
        let origin = snap(&mut s);
        let mut payload = 19_u16.to_le_bytes().to_vec();
        payload.extend(77_u64.to_le_bytes());
        payload.extend(b"choose");
        let mut frame = [0; 4096];
        let len = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Sdk,
            3,
            12,
            &payload,
            &mut frame,
        )
        .unwrap();
        let ring = |s: &mut ControlServer<MockBackend>| {
            let v = s.vmm.as_mut().unwrap();
            v.guest_slice_mut(0xe000, len)
                .unwrap()
                .copy_from_slice(&frame[..len]);
            v.service_doorbell(len as u32).unwrap()
        };
        assert_eq!(ring(&mut s), crate::vmm::Step::SdkStop);
        let at = s.vmm().unwrap().effective_vns().unwrap();
        let until = StopConditions {
            deadline: Some(Moment(at)),
            on: StopMask::NONE,
        };
        let request = Request::Run {
            until,
            resolve: None,
        };
        let stopped_hash = hash(&mut s);
        let expected = Reply::Stop(StopReason::Decision {
            vtime: Moment(at),
            id: control_proto::DecisionId(77),
            ctx: [19_u16.to_le_bytes().as_slice(), b"choose"].concat(),
        });
        assert_eq!(s.handle(&request).unwrap(), Ok(expected.clone()));
        assert_eq!(s.handle(&request).unwrap(), Ok(expected.clone()));
        assert_eq!(hash(&mut s), stopped_hash);
        let counts = s.vmm().unwrap().exit_counts();
        let pending = snap(&mut s);
        let mut artifact = Vec::new();
        let receipt = s.export_portable_snapshot(pending, &mut artifact).unwrap();
        assert_eq!(receipt.state_hash, stopped_hash);
        assert_eq!(receipt.at, Moment(at));
        assert_eq!(s.vmm().unwrap().exit_counts(), counts);
        assert_eq!(s.vmm().unwrap().effective_vns(), Some(at));
        assert_eq!(hash(&mut s), stopped_hash);
        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(vec![9]),
                }),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        assert!(s.vmm().unwrap().pending_service_question().is_some());
        for (vtime, service, id) in [(at + 1, 19, 77), (at, 20, 77), (at, 19, 78)] {
            assert_eq!(
                s.handle(&Request::Run {
                    until,
                    resolve: Some(Resolution {
                        vtime: Moment(vtime),
                        service,
                        id: control_proto::DecisionId(id),
                        answer: Answer(ServiceAnswer::Nominal.encode()),
                    }),
                })
                .unwrap(),
                Err(ControlError::ResolveWithoutDecision)
            );
            assert_eq!(hash(&mut s), stopped_hash);
        }
        let oversized = ServiceAnswer::Data(vec![0; hypercall_proto::MAX_PAYLOAD]).encode();
        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(oversized),
                }),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        assert_eq!(hash(&mut s), stopped_hash);
        let answer = ServiceAnswer::Data(vec![0x5a; hypercall_proto::MAX_PAYLOAD - 1]);
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(answer.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert!(s.vmm().unwrap().pending_service_question().is_none());
        let expected_hash = hash(&mut s);
        let response = s.vmm().unwrap().guest_slice(0xf000, 4096).unwrap().to_vec();
        let (_, bytes) = hypercall_proto::decode(&response).unwrap();
        assert_eq!(ServiceAnswer::decode(bytes).unwrap(), answer);
        let recorded = s.recorded_env().clone();
        assert_eq!(recorded.answers().get(&(at, 19, 77)), Some(&answer));

        let mut cold = payload_server();
        cold.set_service_factory(std::sync::Arc::clone(&s.service_factory));
        hello(&mut cold);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        cold.restore(imported.id, None).unwrap().unwrap();
        assert_eq!(hash(&mut cold), stopped_hash);
        assert_eq!(cold.handle(&request).unwrap(), Ok(expected));
        assert_eq!(cold.vmm().unwrap().effective_vns(), Some(at));
        assert!(matches!(
            cold.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(answer.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert_eq!(
            cold.vmm().unwrap().guest_slice(0xf000, 4096).unwrap(),
            response
        );
        assert_eq!(hash(&mut cold), expected_hash);
        assert_eq!(
            cold.vmm().unwrap().sdk_events(),
            s.vmm().unwrap().sdk_events()
        );
        assert_eq!(
            cold.recorded_env().answers().get(&(at, 19, 77)),
            Some(&answer)
        );
        assert!(cold.vmm().unwrap().pending_service_question().is_none());
        s.restore(
            origin,
            Some(&Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: recorded.encode(),
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(ring(&mut s), crate::vmm::Step::Continued);
        assert_eq!(
            s.vmm().unwrap().guest_slice(0xf000, 4096).unwrap(),
            response
        );
        assert_eq!(hash(&mut s), expected_hash);
    }

    #[test]
    fn restore_rejects_factory_identity_or_configuration_mismatch_without_mutation() {
        let mut s = payload_server();
        hello(&mut s);
        let origin = snap(&mut s);
        let before = s.vmm().unwrap().state_hash().unwrap();
        for config in [
            ServiceConfig {
                identity: b"different-service".to_vec(),
                configuration: vec![7],
            },
            ServiceConfig {
                identity: b"test-service-v1".to_vec(),
                configuration: vec![8],
            },
        ] {
            s.set_service_factory(std::sync::Arc::new(|_| {
                Ok(Box::new(TestService {
                    response: vec![7],
                    calls: 0,
                }))
            }));
            let mut spec = EnvSpec::seeded(1);
            spec.set_config(config);
            assert_eq!(
                s.handle(&Request::Branch {
                    snap: origin,
                    env: Reproducer {
                        blob_version: EnvSpec::BLOB_VERSION,
                        bytes: spec.encode()
                    },
                })
                .unwrap(),
                Err(ControlError::Unsupported)
            );
            assert_eq!(s.vmm().unwrap().state_hash().unwrap(), before);
        }
    }

    #[test]
    fn stale_service_resolution_cannot_answer_a_later_decision() {
        use environment::channel::RecordedEnv;

        let mut s = payload_server();
        hello(&mut s);
        let config = ServiceConfig {
            identity: b"test-external-v1".to_vec(),
            configuration: vec![],
        };
        let expected_config = config.clone();
        s.set_service_factory(std::sync::Arc::new(move |config| {
            if config != &expected_config {
                return Err(environment::channel::ChannelError::Malformed);
            }
            Ok(Box::new(ExternalService(0)))
        }));
        s.recorded.set_config(config.clone());
        s.vmm.as_mut().unwrap().enable_sdk(
            RecordedEnv::new(s.recorded.seed(), Box::new(ExternalService(0))),
            &config,
        );

        let ring = |s: &mut ControlServer<MockBackend>, request_id: u64, seq: u32| {
            let mut payload = 19_u16.to_le_bytes().to_vec();
            payload.extend(request_id.to_le_bytes());
            payload.extend(b"choose");
            let mut frame = [0; 4096];
            let len = hypercall_proto::encode_request(
                hypercall_proto::ServiceId::Sdk,
                3,
                seq,
                &payload,
                &mut frame,
            )
            .unwrap();
            let v = s.vmm.as_mut().unwrap();
            v.guest_slice_mut(0xe000, len)
                .unwrap()
                .copy_from_slice(&frame[..len]);
            v.service_doorbell(len as u32).unwrap()
        };

        assert_eq!(ring(&mut s, 77, 12), crate::vmm::Step::SdkStop);
        let at = s.vmm().unwrap().effective_vns().unwrap();
        let until = StopConditions {
            deadline: Some(Moment(at)),
            on: StopMask::NONE,
        };
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: None,
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Decision {
                id: control_proto::DecisionId(77),
                ..
            }))
        ));

        let first = environment::channel::Answer::Data(b"first".to_vec());
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(first.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));

        assert_eq!(ring(&mut s, 88, 13), crate::vmm::Step::SdkStop);
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: None,
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Decision {
                id: control_proto::DecisionId(88),
                ..
            }))
        ));

        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(first.encode()),
                }),
            })
            .unwrap(),
            Err(ControlError::ResolveWithoutDecision)
        );
        assert_eq!(
            s.vmm()
                .unwrap()
                .pending_service_question()
                .map(|(_, q)| q.request_id()),
            Some(88)
        );

        let second = environment::channel::Answer::Data(b"second".to_vec());
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(88),
                    answer: Answer(second.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert_eq!(s.recorded_env().answers().get(&(at, 19, 88)), Some(&second));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "restores the complete VM through snapshot-store mmap; the service configuration assertion is a control-server replay invariant"
    )]
    fn replay_restores_generic_service_configuration_and_recorded_env() {
        let mut s = payload_server();
        hello(&mut s);
        let origin = snap(&mut s);
        let config_a = ServiceConfig {
            identity: b"test-service-v1".to_vec(),
            configuration: b"configuration-a".to_vec(),
        };
        let config_b = ServiceConfig {
            identity: b"test-service-v1".to_vec(),
            configuration: b"configuration-b".to_vec(),
        };
        let branch_env = |seed, config: ServiceConfig| {
            let mut spec = EnvSpec::seeded(seed);
            spec.set_config(config);
            Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        assert_eq!(
            s.handle(&Request::Branch {
                snap: origin,
                env: branch_env(0xA, config_a.clone()),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_a);
        let config_a_snapshot = snap(&mut s);

        assert_eq!(
            s.handle(&Request::Branch {
                snap: config_a_snapshot,
                env: branch_env(0xB, config_b.clone()),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_b);

        assert_eq!(
            s.handle(&Request::Replay(config_a_snapshot)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_a);
        assert_eq!(
            EnvSpec::decode(&s.recorded_env().encode())
                .unwrap()
                .config(),
            &config_a
        );
    }

    fn stage_payload_request(server: &mut ControlServer<MockBackend>, bytes: u32) -> u32 {
        const REQ_GPA: u64 = 0xE000;
        let mut frame = [0_u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Payload,
            1,
            1,
            &bytes.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        server
            .vmm
            .as_mut()
            .unwrap()
            .guest_slice_mut(REQ_GPA, n)
            .unwrap()
            .copy_from_slice(&frame[..n]);
        u32::try_from(n).unwrap()
    }

    fn ring_payload(server: &mut ControlServer<MockBackend>, bytes: u32) -> (u16, Vec<u8>) {
        const RESP_GPA: u64 = 0xF000;
        let n = stage_payload_request(server, bytes);
        let vmm = server.vmm.as_mut().unwrap();
        assert!(matches!(
            vmm.dispatch_out(0x0CA1, 4, n).unwrap(),
            crate::vmm::Step::Continued
        ));
        let page = vmm.guest_slice(RESP_GPA, 4096).unwrap();
        let (header, payload) = hypercall_proto::decode(page).unwrap();
        (header.status, payload.to_vec())
    }

    fn run_all(server: &mut ControlServer<MockBackend>) -> StopReason {
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Stop(stop)) => stop,
            other => panic!("run reply: {other:?}"),
        }
    }

    fn accumulated_session_server() -> ControlServer<MockArm64Backend> {
        let make_vmm = |exits: Vec<Exit<Arm64>>, seed: u64| {
            let mut backend = MockArm64Backend::with_exits(exits);
            backend.set_policy(&Arm64Policy::default()).unwrap();
            let mut vmm = Vmm::new(backend, GuestRam::new(RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
            vmm.wire_snapshot_hashing();
            vmm
        };
        let serial = || {
            Exit::Common(CommonExit::Mmio {
                gpa: vmm_backend::Gpa(crate::vendor::arm64::board::PL011.0),
                size: 1,
                write: Some(u64::from(b'x')),
            })
        };
        let mut live = make_vmm(vec![serial(), Exit::Common(CommonExit::Idle)], 0xBA5E);
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);
        let factory =
            Box::new(move || Ok(make_vmm(vec![serial(), Exit::Common(CommonExit::Idle)], 0)));
        let mut server = with_test_service(ControlServer::new(live, factory));
        assert_eq!(
            server.handle(&Request::Hello(server_caps())).unwrap(),
            Ok(Reply::Hello(server_caps()))
        );
        let base = match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot reply: {other:?}"),
        };
        for _ in 0..2 {
            assert_eq!(
                server.handle(&Request::Replay(base)).unwrap(),
                Ok(Reply::Unit)
            );
            let reply = server
                .handle(&Request::Run {
                    until: StopConditions {
                        deadline: None,
                        on: StopMask::NONE,
                    },
                    resolve: None,
                })
                .unwrap();
            assert!(matches!(
                reply,
                Ok(Reply::Stop(StopReason::Quiescent { .. }))
            ));
        }
        server
    }

    fn accumulated_session_trace() -> crate::session_trace::SessionVirtualTimeTrace {
        accumulated_session_server()
            .session_virtual_time_trace()
            .expect("virtual_time control server produces a session trace")
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot materialize through two production Replay verbs; the pure session comparator and negative control remain Miri-covered"
    )]
    fn control_session_accumulates_every_restore_delimited_trace() {
        use crate::session_trace::{
            SessionTraceStart, check_session_delivery_placement, compare_session_traces,
        };

        let left = accumulated_session_trace();
        let right = accumulated_session_trace();
        assert_eq!(left.segments().len(), 3);
        assert_eq!(left.segments()[0].start(), SessionTraceStart::InitialBoot);
        assert_eq!(
            left.segments()[1].start(),
            SessionTraceStart::Replay { snapshot: 1 }
        );
        assert_eq!(
            left.segments()[2].start(),
            SessionTraceStart::Replay { snapshot: 1 }
        );
        assert!(
            left.event_count() > left.segments()[2].normalized_log().events.len(),
            "the session oracle must cover more than the final VMM suffix: total={}, segments={:?}",
            left.event_count(),
            left.segments()
                .iter()
                .map(|segment| segment.normalized_log().events.len())
                .collect::<Vec<_>>()
        );
        assert_eq!(compare_session_traces(&left, &right), Ok(()));
        assert_eq!(check_session_delivery_placement(&left), Ok(()));
        assert_eq!(left.digest(), right.digest());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot materialize through two production Replay verbs; the pure session comparator and negative control remain Miri-covered"
    )]
    fn taking_the_session_trace_returns_and_drains_completed_segments() {
        let mut server = accumulated_session_server();
        let before = server.vmm().unwrap().state_hash().unwrap();
        let viewed = server.session_virtual_time_trace().unwrap();
        let taken = server.take_session_virtual_time_trace().unwrap();
        assert_eq!(taken, viewed);
        assert_eq!(taken.segments().len(), 3);

        let live_only = server.take_session_virtual_time_trace().unwrap();
        assert_eq!(live_only.segments().len(), 1);
        assert_eq!(live_only.segments()[0], taken.segments()[2]);
        assert_eq!(server.vmm().unwrap().state_hash().unwrap(), before);
        for _ in 0..8 {
            assert_eq!(server.take_session_virtual_time_trace().unwrap(), live_only);
            assert_eq!(server.vmm().unwrap().state_hash().unwrap(), before);
        }
    }

    fn chain_len(s: &ControlServer<MockBackend>, id: SnapId) -> u32 {
        s.snapshot_chain_len(id).expect("handle is live")
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the seal-path logic is covered by the non-mmap tests"
    )]
    fn seal_derives_from_tracked_parent_and_reproduces_the_image() {
        let mut s = server_tracked();
        hello(&mut s);
        let first = snap(&mut s);
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 0x40,
                bytes: (0xDEAD_BEEF_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, first), 1, "first seal is the base");
        assert_eq!(chain_len(&s, second), 2, "second seal derived (O(dirty))");
        let map = s.engine.materialize(s.snaps[&second.0]).unwrap();
        assert_eq!(map.as_slice(), s.vmm().unwrap().guest_memory());
        let base_twin = {
            let vmm = s.vmm.as_ref().unwrap();
            s.engine.snapshot_base(vmm.guest_memory(), b"twin").unwrap()
        };
        let twin = s.engine.materialize(base_twin).unwrap();
        assert_eq!(map.as_slice(), twin.as_slice());
    }

    #[test]
    fn restore_mode_is_memcpy_until_a_remap_factory_installs() {
        let s = server(vec![Exit::Common(CommonExit::Idle)]);
        assert_eq!(s.restore_mode(), super::RestoreMode::Memcpy);
        let s = server_with_remap(vec![Exit::Common(CommonExit::Idle)], false);
        assert_eq!(s.restore_mode(), super::RestoreMode::Remap);
    }

    #[test]
    fn restore_mode_defers_materialization_only_for_in_place() {
        assert!(restore_defers_materialization(super::RestoreMode::InPlace));
        assert!(!restore_defers_materialization(super::RestoreMode::Remap));
        assert!(!restore_defers_materialization(super::RestoreMode::Memcpy));
    }

    #[test]
    fn post_commit_restore_errors_are_never_recoverable_rejections() {
        assert!(restore_error_is_precommit(&VmmError::ContractViolation(
            "validation".to_owned()
        )));
        assert!(!restore_error_is_precommit(&VmmError::Backend(
            vmm_backend::BackendError::Internal("already mutated")
        )));
    }

    #[test]
    fn sparse_page_order_check_distinguishes_sorted_duplicate_and_descending() {
        assert!(sparse_page_order_error(None, 7).is_none());
        assert!(matches!(
            sparse_page_order_error(Some(3), 3),
            Some(crate::snapshot::SnapshotError::SparsePageDuplicate { gfn: 3 })
        ));
        assert!(matches!(
            sparse_page_order_error(Some(3), 1),
            Some(crate::snapshot::SnapshotError::SparsePagesNotSorted {
                previous: 3,
                current: 1,
            })
        ));
        assert!(sparse_page_order_error(Some(1), 3).is_none());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn untracked_seals_always_full_scan() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let first = snap(&mut s);
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, first), 1);
        assert_eq!(chain_len(&s, second), 1, "no tracking ⇒ base, never derive");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn seal_flattens_at_the_chain_bound() {
        let mut s = server_tracked();
        s.set_max_chain_len(2);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 2 * 4096 + 0x40,
                bytes: (0xABCD_1234_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let c = snap(&mut s);
        assert_eq!(chain_len(&s, a), 1);
        assert_eq!(chain_len(&s, b), 2, "under the bound: derive");
        assert_eq!(chain_len(&s, c), 1, "at the bound: flatten to a base");
        assert_eq!(
            s.last_seal_dirty_gfns(),
            Some(&[2][..]),
            "the bound seal drains and reports its exact dirty window"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn seal_falls_back_to_base_when_the_parent_was_dropped() {
        let mut s = server_tracked();
        hello(&mut s);
        let first = snap(&mut s);
        assert_eq!(s.handle(&Request::Drop(first)).unwrap(), Ok(Reply::Unit));
        assert_eq!(
            s.current_image, None,
            "dropping the tracked image clears it"
        );
        assert_eq!(
            s.derive_parent, None,
            "the dropped image cannot derive again"
        );
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, second), 1, "dead parent ⇒ full scan");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn seal_falls_back_after_a_wholesale_host_write() {
        let mut s = server_tracked();
        hello(&mut s);
        let _first = snap(&mut s);
        let image = vec![0x5Au8; RAM];
        s.vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, second), 1, "wholesale write ⇒ full scan");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the mode plumbing is covered by the non-mmap tests"
    )]
    fn branch_remap_and_memcpy_agree_bit_for_bit() {
        let mut s = server_with_remap(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            false,
        );
        hello(&mut s);
        let sp = snap(&mut s);

        s.set_restore_mode(super::RestoreMode::Memcpy);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert!(!s.vmm().unwrap().ram_backing_is_snapshot());
        let stop_memcpy = run_all(&mut s);
        let mem_memcpy = s.vmm().unwrap().guest_memory().to_vec();
        let hash_memcpy = s.handle(&Request::Hash {
            scope: HashScope::Whole,
        });

        s.set_restore_mode(super::RestoreMode::Remap);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert!(
            s.vmm().unwrap().ram_backing_is_snapshot(),
            "the remap arm's guest RAM is the materialized mapping itself"
        );
        let stop_remap = run_all(&mut s);
        let mem_remap = s.vmm().unwrap().guest_memory().to_vec();
        let hash_remap = s.handle(&Request::Hash {
            scope: HashScope::Whole,
        });

        assert_eq!(stop_memcpy, stop_remap);
        assert_eq!(mem_memcpy, mem_remap);
        assert_eq!(hash_memcpy.unwrap(), hash_remap.unwrap());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "snapshot materialization and page hashing use mmap-backed production paths"
    )]
    fn in_place_restore_unions_live_dirty_pages_and_reports_exact_bytes() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();

        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 3 * 4096,
                bytes: (0xA5A5_5A5A_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));

        assert_eq!(s.in_place_fallbacks(), 0);
        assert_eq!(s.last_restore_bytes_written(), 4096);
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the fallback mutant would materialize a snapshot-store mapping, which Miri cannot execute"
    )]
    fn in_place_restore_without_source_image_accepts_missing_dirty_log() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let target = snap(&mut s);

        assert_eq!(s.current_image, None);
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 0);
        assert_eq!(s.last_restore_bytes_written(), RAM as u64);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the fallback path materializes a snapshot-store mapping, which Miri cannot execute"
    )]
    fn in_place_restore_with_a_source_image_rejects_missing_dirty_log() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();

        s.vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&vec![0xA5; RAM])
            .unwrap();
        assert_eq!(s.current_image, s.snaps.get(&target.0).copied());
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 1);
        assert_eq!(s.last_restore_bytes_written(), 0);
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "snapshot materialization and page hashing use mmap-backed production paths"
    )]
    fn in_place_restore_falls_back_when_the_source_image_is_gone() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 4096,
                bytes: (0x55AA_33CC_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let lost_source = s
            .engine
            .snapshot_base(s.vmm.as_ref().unwrap().guest_memory(), b"stale-source")
            .unwrap();
        s.engine.release(lost_source).unwrap();
        s.engine.gc();
        s.current_image = Some(lost_source);

        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 1);
        assert_eq!(s.last_restore_bytes_written(), 0);
        assert_eq!(s.current_image, None, "fresh mock factory is untracked");
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the mode plumbing is covered by the non-mmap tests"
    )]
    fn remap_restore_failure_keeps_a_usable_session() {
        let mut s = server_with_remap(vec![Exit::Common(CommonExit::Idle)], true);
        hello(&mut s);
        let sp = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Err(ControlError::RestoreFailed)
        );
        assert!(!s.vmm().unwrap().ram_backing_is_snapshot());
        assert!(matches!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole
            })
            .unwrap(),
            Ok(Reply::Hash(_))
        ));
    }

    fn run_seeking_snapshot(server: &mut ControlServer<MockBackend>) -> StopReason {
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Stop(stop)) => stop,
            other => panic!("run reply: {other:?}"),
        }
    }

    fn run_all_res(server: &mut ControlServer<MockBackend>) -> Result<Reply, ControlError> {
        server
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: None,
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap()
    }

    fn hash(server: &mut ControlServer<MockBackend>) -> [u8; 32] {
        let req = Request::Hash {
            scope: HashScope::Whole,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Hash(h)) => h,
            other => panic!("hash reply: {other:?}"),
        }
    }

    #[test]
    fn hello_negotiates_the_pinned_caps() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let caps = server_caps();
        assert_eq!(caps.protocol_version, control_proto::APP_PROTOCOL_VERSION);
        assert_eq!(
            caps.protocol_version, 12,
            "protocol version numbers remain monotonic after retired tags"
        );
        assert_eq!(caps.env_version_min, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.env_version_max, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.coverage.map_bytes, 0, "no coverage producer exists");
        assert_eq!(caps.coverage.producer, 0);
        assert!(
            caps.flags.contains(CapFlags::GUEST_HAS_SDK),
            "the server services the doorbell, so GUEST_HAS_SDK is advertised"
        );
    }

    #[test]
    fn hello_rejects_an_application_version_mismatch_before_opening_the_session() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        let mut old = server_caps();
        old.protocol_version = old.protocol_version.checked_sub(1).unwrap();
        assert_eq!(
            s.handle(&Request::Hello(old)).unwrap(),
            Err(ControlError::Unsupported)
        );
        assert_eq!(
            s.handle(&Request::Snapshot).unwrap(),
            Err(ControlError::Unsupported),
            "a rejected hello must not enable later verbs"
        );
        hello(&mut s);
    }

    #[test]
    fn sdk_events_verb_is_routed_to_the_capture() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        match s.handle(&Request::SdkEvents { offset: 0 }).unwrap() {
            Ok(Reply::SdkEvents(events)) => {
                assert!(events.is_empty(), "the mock guest emits no doorbell events")
            }
            other => panic!("SdkEvents verb answered unexpectedly (paged): {other:?}"),
        }
    }

    fn read(
        server: &mut ControlServer<MockBackend>,
        gpa: u64,
        len: u32,
    ) -> Result<Reply, ControlError> {
        server.handle(&Request::Read { gpa, len }).unwrap()
    }

    fn regs(server: &mut ControlServer<MockBackend>) -> control_proto::RegsView {
        match server.handle(&Request::Regs).unwrap() {
            Ok(Reply::Regs(v)) => v,
            other => panic!("regs reply: {other:?}"),
        }
    }

    #[test]
    fn read_returns_the_guest_bytes() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        match read(&mut s, 0, 12) {
            Ok(Reply::Bytes(b)) => assert_eq!(&b, b"SERVER_BOOT\n"),
            other => panic!("read reply: {other:?}"),
        }
        assert_eq!(read(&mut s, RAM as u64, 0), Ok(Reply::Bytes(Vec::new())));
        assert_eq!(
            read(&mut s, RAM as u64 - 4, 4),
            Ok(Reply::Bytes(vec![0u8; 4])),
            "a read ending exactly at ram_len is in range"
        );
    }

    #[test]
    fn read_out_of_range_is_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert_eq!(
            read(&mut s, RAM as u64 - 3, 4),
            Err(ControlError::ReadOutOfRange {
                gpa: RAM as u64 - 3,
                len: 4,
                ram_len: RAM as u64,
            }),
            "one byte past the end is rejected, not clipped"
        );
        assert_eq!(
            read(&mut s, u64::MAX - 2, 8),
            Err(ControlError::ReadOutOfRange {
                gpa: u64::MAX - 2,
                len: 8,
                ram_len: RAM as u64,
            }),
            "a gpa+len that would overflow u64 is rejected, never wrapped"
        );
    }

    #[test]
    fn read_oversized_len_is_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert_eq!(
            read(&mut s, 0, READ_CAP + 1),
            Err(ControlError::ReadTooLarge {
                len: READ_CAP + 1,
                cap: READ_CAP,
            })
        );
        assert_eq!(
            read(&mut s, u64::MAX, u32::MAX),
            Err(ControlError::ReadTooLarge {
                len: u32::MAX,
                cap: READ_CAP,
            }),
            "the cap is checked before the range, so no slice is attempted"
        );
    }

    #[test]
    fn regs_reports_the_versioned_view_at_the_current_moment() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let v = regs(&mut s);
        assert_eq!(v.version, control_proto::RegsView::VERSION);
        let vns = s.vmm().unwrap().effective_vns().unwrap();
        assert_eq!(v.moment.0, vns, "moment is the current V-time");
        assert_eq!(v.vtime, vns, "vtime and moment coincide on the single axis");
        assert_eq!(v.moment.0, 500, "the fixture is wired at exit count 500");
    }

    #[test]
    fn observations_before_hello_are_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        assert_eq!(read(&mut s, 0, 4), Err(ControlError::Unsupported));
        assert_eq!(
            s.handle(&Request::Regs).unwrap(),
            Err(ControlError::Unsupported)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn read_and_regs_on_a_poisoned_server_are_session_fatal() {
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let mut s = ControlServer::new(
            live,
            Box::new(|| Err(VmmError::ContractViolation("no boot".into()))),
        );
        hello(&mut s);
        let base = snap(&mut s);
        assert!(
            matches!(
                s.handle(&Request::Branch {
                    snap: base,
                    env: seeded_env(1),
                }),
                Err(ServeError::Vmm(_))
            ),
            "the failing factory tears the session down"
        );
        assert!(matches!(
            s.handle(&Request::Read { gpa: 0, len: 4 }),
            Err(ServeError::Poisoned)
        ));
        assert!(matches!(
            s.handle(&Request::Regs),
            Err(ServeError::Poisoned)
        ));
        assert!(matches!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            }),
            Err(ServeError::Poisoned)
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn observations_do_not_perturb_hash_or_recorded_env() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        let h_before = hash(&mut s);
        let env_before = s.recorded_env().clone();
        let _ = read(&mut s, 0, 16);
        let _ = read(&mut s, RAM as u64 - 8, 8);
        let _ = regs(&mut s);
        let _ = read(&mut s, RAM as u64, 64);
        let _ = regs(&mut s);
        assert_eq!(hash(&mut s), h_before, "observation did not move the hash");
        assert_eq!(
            s.recorded_env(),
            &env_before,
            "observation was not recorded into the reproducer"
        );
    }

    #[derive(Clone, Debug)]
    enum ObsOp {
        Snapshot,
        Run,
        Hash,
        Branch(u64),
        Replay,
        Read(u64, u32),
        Regs,
    }

    fn arb_obs_op() -> impl Strategy<Value = ObsOp> {
        prop_oneof![
            Just(ObsOp::Snapshot),
            Just(ObsOp::Run),
            Just(ObsOp::Hash),
            (1u64..=8).prop_map(ObsOp::Branch),
            Just(ObsOp::Replay),
            (0u64..=(RAM as u64 + 64), 0u32..=(RAM as u32 + 64))
                .prop_map(|(gpa, len)| ObsOp::Read(gpa, len)),
            Just(ObsOp::Regs),
        ]
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Rec {
        Ctl(Result<Reply, ControlError>),
        Run(Result<Reply, ControlError>),
        Hash(Result<Reply, ControlError>),
    }

    fn run_obs_script(ops: &[ObsOp], include_obs: bool) -> Vec<Rec> {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let mut rec = Vec::new();
        for op in ops {
            match op {
                ObsOp::Snapshot => rec.push(Rec::Ctl(s.handle(&Request::Snapshot).unwrap())),
                ObsOp::Run => rec.push(Rec::Run(run_all_res(&mut s))),
                ObsOp::Hash => rec.push(Rec::Hash(
                    s.handle(&Request::Hash {
                        scope: HashScope::Whole,
                    })
                    .unwrap(),
                )),
                ObsOp::Branch(seed) => rec.push(Rec::Ctl(
                    s.handle(&Request::Branch {
                        snap: base,
                        env: seeded_env(*seed),
                    })
                    .unwrap(),
                )),
                ObsOp::Replay => rec.push(Rec::Ctl(s.handle(&Request::Replay(base)).unwrap())),
                ObsOp::Read(gpa, len) => {
                    if include_obs {
                        let _ = read(&mut s, *gpa, *len);
                    }
                }
                ObsOp::Regs => {
                    if include_obs {
                        let _ = s.handle(&Request::Regs).unwrap();
                    }
                }
            }
        }
        rec
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping")]
        fn observations_never_change_hash_or_stop_outcomes(
            ops in prop::collection::vec(arb_obs_op(), 1..16)
        ) {
            let with_obs = run_obs_script(&ops, true);
            let without_obs = run_obs_script(&ops, false);
            prop_assert_eq!(with_obs, without_obs,
                "interleaved read/regs changed a hash or stop outcome");
        }
    }

    #[test]
    fn sdk_events_pages_bound_to_the_frame_limit() {
        let per_event = 4_000usize;
        let count = control_proto::MAX_FRAME_LEN / (16 + per_event) + 5;
        let all: Vec<(u64, u32, Vec<u8>)> = (0..count)
            .map(|i| (i as u64, i as u32, vec![0xAB_u8; per_event]))
            .collect();

        let mut fetched: Vec<(u64, u32, Vec<u8>)> = Vec::new();
        let mut pages = 0;
        loop {
            let page = page_sdk_events(&all, fetched.len());
            if page.is_empty() {
                break;
            }
            pages += 1;
            let body: usize = 6 + page.iter().map(|e| 16 + e.2.len()).sum::<usize>();
            assert!(
                body <= control_proto::MAX_FRAME_LEN,
                "page {pages} body {body} exceeds the frame limit"
            );
            fetched.extend(page);
        }
        assert!(
            pages >= 2,
            "a capture over one frame splits into multiple pages, got {pages}"
        );
        assert_eq!(fetched, all, "paging reassembles the full capture exactly");
    }

    #[test]
    fn any_verb_before_hello_is_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        for req in [
            Request::Snapshot,
            Request::Drop(SnapId(1)),
            Request::Hash {
                scope: HashScope::Whole,
            },
        ] {
            assert_eq!(s.handle(&req).unwrap(), Err(ControlError::Unsupported));
        }
        hello(&mut s);
        assert!(s.handle(&Request::Snapshot).unwrap().is_ok());
    }

    #[test]
    fn snapshot_mints_fresh_handles_and_drop_releases_them() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        assert_ne!(a, b, "handles are pool-wide and never reused");
        assert_eq!(s.latest_snapshot(), Some(b));
        assert_eq!(s.handle(&Request::Drop(b)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.latest_snapshot(), Some(a));
        assert_eq!(s.handle(&Request::Drop(a)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.latest_snapshot(), None);
        assert_eq!(
            s.handle(&Request::Drop(a)).unwrap(),
            Err(ControlError::UnknownSnapshot(a)),
            "double drop is loud"
        );
        assert_eq!(
            s.handle(&Request::Replay(a)).unwrap(),
            Err(ControlError::UnknownSnapshot(a)),
            "a dropped handle cannot be restored"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize uses tempfile+mmap); payload tape semantics are covered by the Miri-run vmm and environment unit tests"
    )]
    fn payload_branch_snapshot_replay_and_exhaustion_close_the_control_loop() {
        let mut s = payload_server();
        hello(&mut s);
        let base = snap(&mut s);

        let chord_a = vec![0x81, 4];
        let chord_b = vec![0, 2];
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![chord_a.clone(), chord_b.clone()]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let staged_hash = hash(&mut s);

        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_a)
        );
        let remaining = match s.handle(&Request::RecordedEnv).unwrap() {
            Ok(Reply::Recorded(reproducer)) => EnvSpec::decode(&reproducer.bytes).unwrap(),
            other => panic!("recorded environment reply: {other:?}"),
        };
        assert_eq!(remaining.payloads(), Some([chord_b.clone()].as_slice()));

        let mid = snap(&mut s);
        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_b.clone())
        );
        assert_eq!(s.handle(&Request::Replay(mid)).unwrap(), Ok(Reply::Unit));
        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_b)
        );

        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![vec![0x81, 5], vec![0, 2]]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_ne!(
            hash(&mut s),
            staged_hash,
            "altering one staged chord must trip the full-state oracle"
        );

        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let n = stage_payload_request(&mut s, 2);
        s.vmm
            .as_mut()
            .unwrap()
            .backend
            .push_exit(Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n),
            }));
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
        let response = s.vmm.as_ref().unwrap().guest_slice(0xF000, 4096).unwrap();
        let (header, payload) = hypercall_proto::decode(response).unwrap();
        assert_eq!(header.status, hypercall_proto::Status::OutOfRange as u16);
        assert!(payload.is_empty());
    }

    #[test]
    fn branch_validates_the_env_before_touching_the_vm() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let mut env = seeded_env(7);
        env.blob_version = 99;
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Err(ControlError::BadEnvVersion(99))
        );
        let env = Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: vec![0xFF; 8],
        };
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        assert_eq!(
            s.handle(&Request::Branch {
                snap: SnapId(999),
                env: seeded_env(7)
            })
            .unwrap(),
            Err(ControlError::UnknownSnapshot(SnapId(999)))
        );
        let _ = hash(&mut s);
        let _ = snap(&mut s);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_accepts_mechanical_operations_and_rejects_unknown_extensions() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let mut spec = EnvSpec::seeded(7);
        spec.record_effect(1234, EnvHostEffect::InjectInterrupt { vector: 32 });
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: spec.encode()
                }
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let before = hash(&mut s);
        let mut unknown = EnvSpec::seeded(7);
        unknown.set_config(ServiceConfig {
            identity: b"uninstalled-service".to_vec(),
            configuration: vec![],
        });
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: unknown.encode()
                }
            })
            .unwrap(),
            Err(ControlError::Unsupported)
        );
        assert_eq!(hash(&mut s), before);
    }

    #[test]
    fn run_resolve_is_always_resolve_without_decision() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: Some(Resolution {
                vtime: Moment(0),
                service: 19,
                id: control_proto::DecisionId(1),
                answer: Answer(vec![1, 2, 3]),
            }),
        };
        assert_eq!(
            s.handle(&req).unwrap(),
            Err(ControlError::ResolveWithoutDecision)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn hash_whole_includes_control_state_and_other_scopes_are_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let h = hash(&mut s);
        assert_ne!(Some(h), s.vmm().map(|v| v.state_hash().unwrap()));
        s.exec_nonce = 1;
        assert_ne!(
            hash(&mut s),
            h,
            "the next command identity is part of whole state"
        );
        s.exec_nonce = 0;
        assert_eq!(hash(&mut s), h);
        let cut = snap(&mut s);
        let mut artifact = Vec::new();
        assert_eq!(
            s.export_portable_snapshot(cut, &mut artifact)
                .unwrap()
                .state_hash,
            h
        );
        for scope in [HashScope::Disk, HashScope::Region { base: 0, len: 4096 }] {
            assert_eq!(
                s.handle(&Request::Hash { scope }).unwrap(),
                Err(ControlError::Unsupported)
            );
        }
    }

    #[test]
    fn perturb_stages_faults_and_rejects_the_unenforceable() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let perturb = |fault: environment::channel::Effect, at: u64| Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(at),
        };
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 32 },
                1000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 33 },
                1000
            ))
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1000 })
        );
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 34 },
                100
            ))
            .unwrap(),
            Err(ControlError::PerturbPastMoment {
                at: 100,
                floor: 500
            })
        );
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 35 },
                500
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: RAM as u64 - 4,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                2000
            ))
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                3000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(vec![0xFF; 3]),
                at: Moment(5000),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
    }

    #[test]
    fn perturb_corrupt_memory_stage_time_resolves_high_arm_gpas() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let perturb = |fault: environment::channel::Effect, at: u64| Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(at),
        };
        s.vmm.as_mut().unwrap().ram_base_gpa = 0x4000_0000;

        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0x4000_0000,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                1000,
            ))
            .unwrap(),
            Ok(Reply::Unit),
            "a valid high arm64 GPA must be admissible at stage time"
        );

        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                2000,
            ))
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: 0,
                ram_len: RAM as u64,
            }),
            "a low unmapped GPA must be rejected at stage time, not at arrival"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn run_maps_terminals_workload_blind() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));

        let mut s = server(vec![Exit::Common(CommonExit::Shutdown)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(1)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        match run_all(&mut s) {
            StopReason::Crash { info, .. } => assert_eq!(info.kind, CrashKind::Shutdown),
            other => panic!("expected Crash{{Shutdown}}, got {other:?}"),
        }

        let dbg = |code: u32| {
            Exit::Arch(X86Exit::Io {
                port: 0xF4,
                size: 1,
                write: Some(code),
            })
        };
        let mut s = server(vec![dbg(0)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(1),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));

        let mut s = server(vec![dbg(1)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(1),
        })
        .unwrap()
        .unwrap();
        match run_all(&mut s) {
            StopReason::Crash { info, .. } => {
                assert_eq!(info.kind, CrashKind::Panic);
                assert_eq!(info.detail, vec![1]);
            }
            other => panic!("expected Crash{{Panic}}, got {other:?}"),
        }
    }

    #[test]
    fn run_stops_at_a_vtime_deadline_without_entering_when_already_past() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: Some(Moment(500)),
                on: StopMask::NONE,
            },
            resolve: None,
        };
        match s.handle(&req).unwrap() {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap; the strict artifact codec and SDK state logic run under Miri separately"
    )]
    fn portable_snapshot_replays_the_complete_mid_lineage_future() {
        let mut source = payload_server();
        hello(&mut source);
        let base = snap(&mut source);
        let first = vec![0x81, 4];
        let second = vec![0, 2];
        assert_eq!(
            source
                .handle(&Request::Branch {
                    snap: base,
                    env: payload_env(7, vec![first.clone(), second.clone()]),
                })
                .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            ring_payload(&mut source, 2),
            (hypercall_proto::Status::Ok as u16, first)
        );
        let midpoint = snap(&mut source);
        let mut artifact = Vec::new();
        let exported = source
            .export_portable_snapshot(midpoint, &mut artifact)
            .unwrap();
        assert_eq!(hash(&mut source), exported.state_hash);
        assert_eq!(
            ring_payload(&mut source, 2),
            (hypercall_proto::Status::Ok as u16, second.clone())
        );
        let uninterrupted_next = hash(&mut source);

        let mut destination = payload_server();
        let imported = destination
            .import_portable_snapshot(artifact.as_slice())
            .unwrap();
        assert_eq!(imported.at, exported.at);
        assert_eq!(imported.sdk_events, exported.sdk_events);
        assert_eq!(imported.state_hash, exported.state_hash);
        hello(&mut destination);
        assert_eq!(
            destination.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            hash(&mut destination),
            exported.state_hash,
            "cross-server restore must equal the source at the cut"
        );
        assert_eq!(
            ring_payload(&mut destination, 2),
            (hypercall_proto::Status::Ok as u16, second)
        );
        assert_eq!(
            hash(&mut destination),
            uninterrupted_next,
            "the restored payload suffix must reproduce the uninterrupted future"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap"
    )]
    fn fully_imported_snapshot_cannot_be_reexported_without_its_hash_suffix() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let sealed = snap(&mut source);
        let mut artifact = Vec::new();
        source
            .export_portable_snapshot(sealed, &mut artifact)
            .unwrap();

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        let imported = destination
            .import_portable_snapshot(artifact.as_slice())
            .unwrap();
        assert!(matches!(
            destination.export_sparse_snapshot(imported.id, imported.id),
            Err(PortableSnapshotError::Malformed(
                "sparse export target lacks a canonical state-blob suffix"
            ))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the replay half materializes a snapshot-store mapping, while sparse export/import validation is exercised by Miri-safe unit tests"
    )]
    fn sparse_portable_snapshot_replays_from_a_corresponding_base() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let base = snap(&mut source);
        let mut image = source.vmm().unwrap().guest_memory().to_vec();
        image[4096..8192].fill(0xA5);
        image[3 * 4096..4 * 4096].fill(0x5A);
        source
            .vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let target = snap(&mut source);
        let exported = source.export_sparse_snapshot(base, target).unwrap();
        assert_eq!(exported.pages.len(), 2);
        assert_eq!(
            exported
                .pages
                .iter()
                .map(|(gfn, _)| *gfn)
                .collect::<Vec<_>>(),
            vec![1, 3],
            "export pages are sorted and target-resolved"
        );
        assert_eq!(exported.pages[0].1.as_ref(), &[0xA5; 4096]);
        assert_eq!(exported.pages[1].1.as_ref(), &[0x5A; 4096]);
        assert!(
            !exported.sidecar.windows(4).any(|tag| tag == b"MEM\0"),
            "the sidecar carries no full-memory section"
        );
        let source_hash = hash(&mut source);

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let destination_base = snap(&mut destination);
        let imported = destination
            .import_sparse_snapshot(destination_base, exported)
            .unwrap();
        assert_eq!(
            destination.snapshot_stats(imported.id).unwrap().owned_pages,
            2
        );
        assert_eq!(
            destination.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut destination), source_hash);
        let mut round_trip_artifact = Vec::new();
        let round_trip = destination
            .export_portable_snapshot(imported.id, &mut round_trip_artifact)
            .unwrap();
        assert_eq!(
            round_trip.state_hash, source_hash,
            "restored state_blob_suffix keeps an on-demand whole-state hash correct"
        );
        assert_eq!(run_all(&mut source), run_all(&mut destination));
        assert_eq!(hash(&mut source), hash(&mut destination));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the foreign-vendor arm fixture performs an ordinary snapshot restore"
    )]
    fn sparse_portable_import_rejects_bad_input_before_minting() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let base = snap(&mut source);
        let mut image = source.vmm().unwrap().guest_memory().to_vec();
        image[4096..8192].fill(0xA5);
        image[3 * 4096..4 * 4096].fill(0x5A);
        source
            .vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let target = snap(&mut source);
        let exported = source.export_sparse_snapshot(base, target).unwrap();

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let destination_base = snap(&mut destination);
        let destination_pages = destination.engine.mem_pages();
        let mut assert_rejected = |candidate| {
            let before = destination.snapshot_store_stats();
            assert!(
                destination
                    .import_sparse_snapshot(destination_base, candidate)
                    .is_err()
            );
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(destination_base));
        };

        let mut unsorted = exported.clone();
        unsorted.pages.swap(0, 1);
        assert_rejected(unsorted);
        let mut duplicate = exported.clone();
        duplicate.pages[1] = duplicate.pages[0].clone();
        assert_rejected(duplicate);
        let mut out_of_range = exported.clone();
        out_of_range.pages[0].0 = destination_pages;
        assert_rejected(out_of_range);
        let mut truncated = exported.clone();
        truncated.sidecar.pop();
        assert_rejected(truncated);
        let mut corrupted = exported.clone();
        corrupted.sidecar[0] ^= 1;
        assert_rejected(corrupted);

        let decoded = super::decode_sparse_sidecar(&exported.sidecar).unwrap();
        let malformed_sidecar = super::encode_sparse_sidecar(&super::SparsePortableSidecarRef {
            vm_state: &decoded.vm_state,
            sdk: None,
            policy: &decoded.policy,
            control_state: &decoded.control_state,
            at: decoded.at,
            sdk_events: 1,
            trace_events: decoded.trace_events,
            trace_schedules: decoded.trace_schedules,
            tainted: decoded.tainted,
            state_blob_suffix: &decoded.state_blob_suffix,
        })
        .unwrap();
        let mut sdk_count_without_channel = exported.clone();
        sdk_count_without_channel.sidecar = malformed_sidecar;
        assert_rejected(sdk_count_without_channel);

        let mut arm = accumulated_session_server();
        let arm_base = arm.latest_snapshot().unwrap();
        let before = arm.snapshot_store_stats();
        assert!(arm.import_sparse_snapshot(arm_base, exported).is_err());
        assert_eq!(arm.snapshot_store_stats(), before);
        assert_eq!(arm.latest_snapshot(), Some(arm_base));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap; strict corruption rejection is covered by the Miri-safe codec test"
    )]
    fn portable_import_rejects_a_planted_ram_corruption_before_minting_a_handle() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let snap = snap(&mut source);
        let mut artifact = Vec::new();
        source
            .export_portable_snapshot(snap, &mut artifact)
            .unwrap();
        artifact[116] ^= 1;

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        let before = destination.snapshot_store_stats();
        assert!(matches!(
            destination.import_portable_snapshot(artifact.as_slice()),
            Err(crate::portable_snapshot::PortableSnapshotError::DigestMismatch)
        ));
        let after = destination.snapshot_store_stats();
        assert_eq!(after.snapshots, before.snapshots);
        assert_eq!(after.stored_unique_pages, before.stored_unique_pages);
    }

    fn full_artifact_with_vm_state(
        source: &ControlServer<MockBackend>,
        snap: SnapId,
        mutate: impl FnOnce(&mut VmState),
    ) -> Vec<u8> {
        let mut artifact = Vec::new();
        source
            .export_portable_snapshot(snap, &mut artifact)
            .unwrap();
        let portable = super::PortableSnapshot::read_from(
            artifact.as_slice(),
            source.vmm().unwrap().guest_memory().len(),
        )
        .unwrap();
        let mut state = VmState::decode(&portable.vm_state).unwrap();
        mutate(&mut state);
        let vm_state = state.encode().unwrap();
        let mut altered = Vec::new();
        super::PortableSnapshotRef {
            memory: &portable.memory,
            vm_state: &vm_state,
            sdk: portable.sdk.as_ref(),
            policy: &portable.policy,
            at: portable.at,
            sdk_events: portable.sdk_events,
            trace_events: portable.trace_events,
            trace_schedules: portable.trace_schedules,
            tainted: portable.tainted,
            state_hash: portable.state_hash,
            control_state: &portable.control_state,
        }
        .write_to(&mut altered)
        .unwrap();
        altered
    }

    fn sparse_artifact_with_vm_state(
        source: &ControlServer<MockBackend>,
        base: SnapId,
        target: SnapId,
        mutate: impl FnOnce(&mut VmState),
    ) -> super::SparsePortableSnapshot {
        let exported = source.export_sparse_snapshot(base, target).unwrap();
        let sidecar = super::decode_sparse_sidecar(&exported.sidecar).unwrap();
        let mut state = VmState::decode(&sidecar.vm_state).unwrap();
        mutate(&mut state);
        let vm_state = state.encode().unwrap();
        let sidecar_bytes = super::encode_sparse_sidecar(&super::SparsePortableSidecarRef {
            vm_state: &vm_state,
            sdk: sidecar.sdk.as_ref(),
            policy: &sidecar.policy,
            at: sidecar.at,
            sdk_events: sidecar.sdk_events,
            trace_events: sidecar.trace_events,
            trace_schedules: sidecar.trace_schedules,
            tainted: sidecar.tainted,
            state_blob_suffix: &sidecar.state_blob_suffix,
            control_state: &sidecar.control_state,
        })
        .unwrap();
        super::SparsePortableSnapshot {
            pages: exported.pages,
            sidecar: sidecar_bytes,
        }
    }

    fn seed_import_session_state(destination: &mut ControlServer<MockBackend>) {
        let config = ServiceConfig {
            identity: b"test-service-v1".to_vec(),
            configuration: b"preflight".to_vec(),
        };
        destination.recorded.set_config(config.clone());
        let seed = destination.recorded.seed();
        destination.vmm.as_mut().unwrap().enable_sdk(
            RecordedEnv::new(
                seed,
                Box::new(TestService {
                    response: vec![0xA5],
                    calls: 7,
                }),
            ),
            &config,
        );
        let at = destination.vmm().unwrap().effective_vns().unwrap();
        destination
            .schedule
            .insert(at, EnvHostEffect::write_memory(0, vec![0x5A; 4]).unwrap());
        destination.reseed_schedule.insert(at, 0x5151);
        destination.exec_nonce = 7;
    }

    fn assert_post_rejection_continuation(destination: &mut ControlServer<MockBackend>) {
        let mut reference = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut reference);
        seed_import_session_state(&mut reference);
        let deadline = destination.vmm().unwrap().effective_vns().unwrap() + 1;
        let request = Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        };
        let destination_reply = destination.handle(&request).unwrap();
        let reference_reply = reference.handle(&request).unwrap();
        assert_eq!(destination_reply, reference_reply);
        assert!(matches!(
            destination_reply,
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));
        assert_eq!(hash(destination), hash(&mut reference));
        assert_eq!(destination.recorded, reference.recorded);
        assert_eq!(destination.schedule, reference.schedule);
        assert_eq!(destination.reseed_schedule, reference.reseed_schedule);
        assert_eq!(
            destination.vmm().unwrap().guest_memory(),
            reference.vmm().unwrap().guest_memory()
        );
    }

    struct ImportStateBefore {
        ram: Vec<u8>,
        hash: [u8; 32],
        stats: snapshot_store::StoreStats,
        latest: Option<SnapId>,
        recorded: EnvSpec,
        schedule: BTreeMap<u64, EnvHostEffect>,
        reseeds: BTreeMap<u64, u64>,
        exec_nonce: u64,
        schedule_poisoned: Option<ScheduleFailure>,
        fallbacks: u64,
        bytes: u64,
    }

    impl ImportStateBefore {
        fn capture(destination: &ControlServer<MockBackend>) -> Self {
            Self {
                ram: destination.vmm().unwrap().guest_memory().to_vec(),
                hash: destination.vmm().unwrap().state_hash().unwrap(),
                stats: destination.snapshot_store_stats(),
                latest: destination.latest_snapshot(),
                recorded: destination.recorded.clone(),
                schedule: destination.schedule.clone(),
                reseeds: destination.reseed_schedule.clone(),
                exec_nonce: destination.exec_nonce,
                schedule_poisoned: destination.schedule_poisoned,
                fallbacks: destination.in_place_fallbacks(),
                bytes: destination.last_restore_bytes_written(),
            }
        }
    }

    fn assert_import_rejection_preserves_server(
        destination: &mut ControlServer<MockBackend>,
        before: &ImportStateBefore,
        result: Result<(), PortableSnapshotError>,
    ) {
        assert!(result.is_err());
        assert_eq!(destination.vmm().unwrap().guest_memory(), before.ram);
        assert_eq!(
            destination.vmm().unwrap().state_hash().unwrap(),
            before.hash
        );
        assert_eq!(destination.snapshot_store_stats(), before.stats);
        assert_eq!(destination.latest_snapshot(), before.latest);
        assert_eq!(&destination.recorded, &before.recorded);
        assert_eq!(&destination.schedule, &before.schedule);
        assert_eq!(&destination.reseed_schedule, &before.reseeds);
        assert_eq!(destination.exec_nonce, before.exec_nonce);
        assert_eq!(destination.schedule_poisoned, before.schedule_poisoned);
        assert_eq!(destination.in_place_fallbacks(), before.fallbacks);
        assert_eq!(destination.last_restore_bytes_written(), before.bytes);
    }

    #[test]
    #[cfg_attr(miri, ignore = "portable import allocates snapshot-store mappings")]
    fn portable_import_preflights_vm_state_before_storing_or_mutating_session() {
        for malformed in [0_u8, 1] {
            let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut source);
            let target = snap(&mut source);
            let full = full_artifact_with_vm_state(&source, target, |state| {
                if malformed == 0 {
                    state.engine_state = vec![1];
                } else {
                    state.xsave_restore_bv = Some(u64::MAX);
                }
            });
            let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut destination);
            seed_import_session_state(&mut destination);
            let before = ImportStateBefore::capture(&destination);
            let result = destination
                .import_portable_snapshot(full.as_slice())
                .map(|_| ());
            assert_import_rejection_preserves_server(&mut destination, &before, result);
            assert_post_rejection_continuation(&mut destination);
        }

        for malformed in [0_u8, 1] {
            let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut source);
            let base = snap(&mut source);
            let target = snap(&mut source);
            let sparse = sparse_artifact_with_vm_state(&source, base, target, |state| {
                if malformed == 0 {
                    state.engine_state = vec![1];
                } else {
                    state.xsave_restore_bv = Some(u64::MAX);
                }
            });
            let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut destination);
            let destination_base = snap(&mut destination);
            seed_import_session_state(&mut destination);
            let before = ImportStateBefore::capture(&destination);
            let result = destination
                .import_sparse_snapshot(destination_base, sparse)
                .map(|_| ());
            assert_import_rejection_preserves_server(&mut destination, &before, result);
            assert_post_rejection_continuation(&mut destination);
        }
    }

    #[test]
    fn portable_import_fails_closed_without_a_live_vm_validator() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let target = snap(&mut source);
        let full = full_artifact_with_vm_state(&source, target, |state| {
            state.engine_state = vec![1];
        });

        let mut full_destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut full_destination);
        full_destination.vmm = None;
        let full_before = full_destination.snapshot_store_stats();
        assert!(matches!(
            full_destination.import_portable_snapshot(full.as_slice()),
            Err(PortableSnapshotError::Malformed(
                "portable VM state restore validation unavailable"
            ))
        ));
        assert_eq!(full_destination.snapshot_store_stats(), full_before);
        assert_eq!(full_destination.latest_snapshot(), None);

        let mut sparse_source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut sparse_source);
        let base = snap(&mut sparse_source);
        let target = snap(&mut sparse_source);
        let sparse = sparse_artifact_with_vm_state(&sparse_source, base, target, |state| {
            state.engine_state = vec![1];
        });
        let mut sparse_destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut sparse_destination);
        let destination_base = snap(&mut sparse_destination);
        sparse_destination.vmm = None;
        let sparse_before = sparse_destination.snapshot_store_stats();
        assert!(matches!(
            sparse_destination.import_sparse_snapshot(destination_base, sparse),
            Err(PortableSnapshotError::Malformed(
                "portable VM state restore validation unavailable"
            ))
        ));
        assert_eq!(sparse_destination.snapshot_store_stats(), sparse_before);
        assert_eq!(sparse_destination.latest_snapshot(), Some(destination_base));
    }

    #[test]
    #[cfg_attr(miri, ignore = "portable import allocates snapshot-store mappings")]
    fn accumulated_transport_inputs_survive_local_and_portable_replay() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let floor = source.vmm().unwrap().effective_vns().unwrap();
        for at in 1..=128 {
            let effect =
                environment::channel::Effect::write_memory(0, vec![at as u8; 10_000]).unwrap();
            assert_eq!(
                source
                    .handle(&Request::Perturb {
                        fault: control_proto::HostFault(effect.encode()),
                        at: Moment(floor + at),
                    })
                    .unwrap(),
                Ok(Reply::Unit)
            );
        }
        assert!(
            source.capture_control_state().pending.encode().len()
                > environment::channel::MAX_CHANNEL_BYTES
        );
        let before = hash(&mut source);
        let cut = snap(&mut source);
        let mut artifact = Vec::new();
        source.export_portable_snapshot(cut, &mut artifact).unwrap();
        assert_eq!(
            source.handle(&Request::Replay(cut)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut source), before);
        let mut cold = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut cold);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut cold), before);
        assert_eq!(cold.schedule, source.schedule);
        assert_eq!(cold.schedule.len(), 128);
    }

    #[test]
    #[cfg_attr(miri, ignore = "sparse snapshot import uses mapped snapshot storage")]
    fn sparse_import_rejects_a_different_control_hash_preimage() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let cut = snap(&mut source);
        let meta = source.snapshot_meta.get_mut(&cut.0).unwrap();
        let mut changed = super::ControlState::decode(&meta.control_state)
            .unwrap()
            .unwrap();
        changed.exec_nonce += 1;
        meta.control_state = changed.encode();
        let sparse = source.export_sparse_snapshot(cut, cut).unwrap();
        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let base = snap(&mut destination);
        let before = destination.snapshot_store_stats();
        let before_hash = hash(&mut destination);
        assert!(matches!(
            destination.import_sparse_snapshot(base, sparse),
            Err(PortableSnapshotError::Malformed(
                "sparse control hash preimage"
            ))
        ));
        assert_eq!(destination.snapshot_store_stats(), before);
        assert_eq!(destination.latest_snapshot(), Some(base));
        assert_eq!(hash(&mut destination), before_hash);
    }

    #[test]
    #[cfg_attr(miri, ignore = "portable import allocates snapshot-store mappings")]
    fn imports_reject_contradictory_control_and_cut_before_minting() {
        for field in 0..4 {
            let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut source);
            let cut = snap(&mut source);
            let meta = source.snapshot_meta.get_mut(&cut.0).unwrap();
            match field {
                0 => meta.at += 1,
                1 => meta.sdk_events += 1,
                2 => meta.control_state[0] ^= 1,
                3 => meta.policy.configuration.push(1),
                _ => unreachable!(),
            }
            let mut artifact = Vec::new();
            source.export_portable_snapshot(cut, &mut artifact).unwrap();
            let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut destination);
            let base = snap(&mut destination);
            let before = destination.snapshot_store_stats();
            let before_hash = hash(&mut destination);
            assert!(matches!(
                destination.import_portable_snapshot(artifact.as_slice()),
                Err(PortableSnapshotError::Malformed(_))
            ));
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(base));
            assert_eq!(hash(&mut destination), before_hash);

            let sparse = source.export_sparse_snapshot(cut, cut).unwrap();
            assert!(matches!(
                destination.import_sparse_snapshot(base, sparse),
                Err(PortableSnapshotError::Malformed(_))
            ));
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(base));
            assert_eq!(hash(&mut destination), before_hash);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_reseeds_and_replay_does_not() {
        let mut s = server(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        hello(&mut s);
        let base = snap(&mut s);
        let h_base = hash(&mut s);

        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert_eq!(hash(&mut s), h_base, "replay is verbatim (same state hash)");

        let mut branch_hash = |seed: u64| -> [u8; 32] {
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(seed),
            })
            .unwrap()
            .unwrap();
            hash(&mut s)
        };
        let h1 = branch_hash(0x1111);
        let h2 = branch_hash(0x2222);
        let h1_again = branch_hash(0x1111);
        assert_eq!(h1, h1_again, "same seed ⇒ same branched state");
        assert_ne!(h1, h2, "distinct seeds ⇒ divergent futures");
        assert_ne!(h1, h_base, "a branch is not the verbatim replay");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn a_failed_factory_is_session_fatal_and_poisons_the_server() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let mut s = ControlServer::new(
            live,
            Box::new(|| Err(VmmError::ContractViolation("no fresh VM".into()))),
        );
        hello(&mut s);
        let base2 = snap(&mut s);
        let err = s.handle(&Request::Replay(base2)).unwrap_err();
        assert!(matches!(err, ServeError::Vmm(_)));
        assert!(matches!(
            s.handle(&Request::Snapshot).unwrap_err(),
            ServeError::Poisoned
        ));
        let _ = base;
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn restore_validation_rejection_is_recoverable_and_keeps_the_fresh_vm() {
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let factory = Box::new(|| {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            Ok(Vmm::new(m, GuestRam::new(RAM).unwrap()))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(1)
            })
            .unwrap(),
            Err(ControlError::RestoreFailed),
            "a validation-class restore rejection is the recoverable RestoreFailed"
        );
        assert!(
            s.vmm().is_some(),
            "the fresh VM was kept after the rejection"
        );
        assert!(
            s.vmm().unwrap().sdk_is_enabled(),
            "the kept fresh VM stays SDK-capable after a recoverable RestoreFailed"
        );
        let _ = hash(&mut s);
    }

    struct RestoreFailBackend(MockBackend);
    impl Backend for RestoreFailBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region
            // (no dereference) — no obligation beyond the trait contract.
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
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.0.save()
        }
        fn validate_restore_state(
            &self,
            state: &vmm_backend::VcpuState,
        ) -> vmm_backend::Result<()> {
            if state.xsave.len() == 1 {
                Err(vmm_backend::BackendError::InvalidState)
            } else {
                Ok(())
            }
        }
        fn restore(&mut self, _s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
            Err(vmm_backend::BackendError::Memory("induced restore failure"))
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

    #[test]
    fn backend_restore_shape_is_preflighted_before_portable_import() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let source_snap = snap(&mut source);
        let artifact = full_artifact_with_vm_state(&source, source_snap, |state| {
            state.xsave.0.push(0);
        });

        let build = || -> Vmm<RestoreFailBackend> {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(RestoreFailBackend(m), GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            v.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            v
        };
        let mut destination = with_test_service(ControlServer::new(
            build(),
            Box::new(|| Err(VmmError::ContractViolation("unused".into()))),
        ));
        assert!(
            destination
                .handle(&Request::Hello(server_caps()))
                .unwrap()
                .is_ok()
        );
        let before_ram = destination.vmm().unwrap().guest_memory().to_vec();
        let before_hash = destination.vmm().unwrap().state_hash().unwrap();
        let before_stats = destination.snapshot_store_stats();
        assert!(matches!(
            destination.import_portable_snapshot(artifact.as_slice()),
            Err(PortableSnapshotError::Malformed(
                "portable VM state restore validation"
            ))
        ));
        assert_eq!(destination.vmm().unwrap().guest_memory(), before_ram);
        assert_eq!(
            destination.vmm().unwrap().state_hash().unwrap(),
            before_hash
        );
        assert_eq!(destination.snapshot_store_stats(), before_stats);
        assert_eq!(destination.latest_snapshot(), None);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn restore_substrate_failure_is_session_fatal_and_poisons_the_server() {
        let build = || -> Vmm<RestoreFailBackend> {
            let mut m = MockBackend::with_exits(vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(RestoreFailBackend(m), GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
            v.wire_snapshot_hashing();
            v.restore_guest_memory(&vec![0u8; RAM]).unwrap();
            v
        };
        let mut live = build();
        live.step().unwrap();
        let mut s = ControlServer::new(live, Box::new(move || Ok(build())));
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
        let base = match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot: {other:?}"),
        };
        let err = s
            .handle(&Request::Branch {
                snap: base,
                env: seeded_env(1),
            })
            .unwrap_err();
        assert!(
            matches!(err, ServeError::Vmm(_)),
            "a post-validation restore fault is session-fatal, got {err:?}"
        );
        assert!(s.vmm().is_none(), "the unvouched VM was dropped");
        assert!(matches!(
            s.handle(&Request::Snapshot).unwrap_err(),
            ServeError::Poisoned
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "drives a real UnixStream socketpair across threads; Miri can't execute the socket syscalls"
    )]
    fn serve_speaks_frames_over_an_in_memory_stream() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let client_thread = std::thread::spawn(move || {
            let mut seq = 0u32;
            let mut send =
                |client: &mut UnixStream, req: &Request| -> Result<Reply, ControlError> {
                    seq += 1;
                    let mut buf = Vec::new();
                    control_proto::encode_request(seq, req, &mut buf).unwrap();
                    client.write_all(&buf).unwrap();
                    let mut inbuf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        if let Some((got_seq, reply, consumed)) =
                            control_proto::decode_reply(&inbuf).unwrap()
                        {
                            assert_eq!(got_seq, seq, "the reply echoes the request seq");
                            assert_eq!(consumed, inbuf.len());
                            return reply;
                        }
                        let n = client.read(&mut chunk).unwrap();
                        assert_ne!(n, 0, "server closed mid-reply");
                        inbuf.extend_from_slice(&chunk[..n]);
                    }
                };
            assert_eq!(
                send(&mut client, &Request::Hello(server_caps())),
                Ok(Reply::Hello(server_caps()))
            );
            let reply = send(&mut client, &Request::Snapshot);
            assert!(matches!(reply, Ok(Reply::Snapshot { .. })));
            assert!(matches!(
                send(
                    &mut client,
                    &Request::Hash {
                        scope: HashScope::Whole
                    }
                ),
                Ok(Reply::Hash(_))
            ));
        });
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        s.serve(server_end).unwrap();
        client_thread.join().unwrap();
    }

    fn enforce_vmm(exit_count: usize, image: [u8; RAM], seed: u64) -> Vmm<MockBackend> {
        let mut exits = vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 }); exit_count];
        exits.push(Exit::Common(CommonExit::Idle));
        let mut m = MockBackend::with_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&image).unwrap();
        v
    }

    fn enforce_image() -> [u8; RAM] {
        let mut image = [0u8; RAM];
        image[..12].copy_from_slice(b"ENFORCE_BOOT");
        image
    }

    fn exits_to_cover(schedule: &[(u64, EnvHostEffect)]) -> usize {
        schedule
            .iter()
            .map(|(m, _)| usize::try_from(*m).unwrap_or(usize::MAX))
            .max()
            .unwrap_or(0)
    }

    fn enforce_run(schedule: &[(u64, EnvHostEffect)], seed: u64) -> ([u8; 32], EnvSpec) {
        let live = enforce_vmm(exits_to_cover(schedule), enforce_image(), seed);
        let factory = Box::new(|| {
            Err(VmmError::ContractViolation(
                "factory unused in a direct enforcement run".into(),
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        for (m, fault) in schedule {
            let req = Request::Perturb {
                fault: HostFault(fault.encode()),
                at: Moment(*m),
            };
            assert_eq!(
                s.handle(&req).unwrap(),
                Ok(Reply::Unit),
                "staging fault at Moment {m}"
            );
        }
        let stop = run_all(&mut s);
        assert!(
            matches!(stop, StopReason::Quiescent { .. }),
            "an enforcement run halts cleanly, got {stop:?}"
        );
        (hash(&mut s), s.recorded_env().clone())
    }

    fn enforce_hash(schedule: &[(u64, EnvHostEffect)], seed: u64) -> [u8; 32] {
        enforce_run(schedule, seed).0
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "enforce_run boots + runs + state-hashes VMs several times (~2 s/KiB sha256 under Miri); it never restores, and its boot-path map_memory seam is Miri-run via bringup::tests::compose_drives_guestram_and_unsafe_map_memory — the same grounds as its proptest sibling arbitrary_schedule_applied_twice_is_identical; covered natively"
    )]
    fn same_schedule_run_twice_is_bit_identical_and_control_differs() {
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xDEAD_BEEF_0000_0001_u64).to_le_bytes().to_vec(),
                },
            ),
            (2, EnvHostEffect::InjectInterrupt { vector: 0x40 }),
        ];
        let seed = 0x5EED59;
        let h1 = enforce_hash(&schedule, seed);
        let h2 = enforce_hash(&schedule, seed);
        assert_eq!(h1, h2, "same schedule ⇒ bit-identical state_hash");

        let control = enforce_hash(&[], seed);
        assert_ne!(
            h1, control,
            "the empty control run differs (the faults land)"
        );
    }

    #[test]
    fn corrupt_memory_lands_the_exact_xor_at_the_gpa() {
        let gpa = 0x80usize;
        let mask = 0xA5A5_0000_1234_5678u64;
        let fault = EnvHostEffect::XorMemory {
            gpa: gpa as u64,
            bytes: mask.to_le_bytes().to_vec(),
        };
        let live = enforce_vmm(1, enforce_image(), 7);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(1),
        })
        .unwrap()
        .unwrap();
        let _ = run_all(&mut s);
        let ram = s.vmm().unwrap().guest_memory();
        let word = u64::from_le_bytes(ram[gpa..gpa + 8].try_into().unwrap());
        assert_eq!(word, mask, "CorruptMemory XORs the mask into the gpa word");
    }

    #[test]
    fn a_fault_at_the_deferred_snapshot_boundary_drains_before_the_seal() {
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 1;

        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        let fault = EnvHostEffect::XorMemory {
            gpa: 0x1000,
            bytes: (0xDEAD_BEEF_u64).to_le_bytes().to_vec(),
        };
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(m),
        })
        .unwrap()
        .unwrap();

        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "the deferred point surfaced, got {stop:?}"
        );

        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => {
                panic!("seal at the deferred boundary failed: {other:?}")
            }
        }
        let ram = s.vmm().unwrap().guest_memory();
        let word = u64::from_le_bytes(ram[0x1000..0x1008].try_into().unwrap());
        assert_eq!(
            word, 0xDEAD_BEEF,
            "the boundary fault was applied before the seal"
        );
    }

    #[test]
    fn a_future_fault_is_retained_at_the_first_snapshot_point() {
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 2;

        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        let fault = EnvHostEffect::XorMemory {
            gpa: 0x1000,
            bytes: (0x00C0_FFEE_u64).to_le_bytes().to_vec(),
        };
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(m),
        })
        .unwrap()
        .unwrap();

        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert!(vtime.0 < m),
            other => panic!("expected SnapshotPoint, got {other:?}"),
        }
        assert!(s.schedule.contains_key(&m), "future input remains pending");
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => panic!("seal failed with a future fault mishandled: {other:?}"),
        }
    }

    #[test]
    fn a_future_reseed_is_retained_at_the_first_snapshot_point() {
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 2;

        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        s.reseed_schedule.insert(m, 9);

        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert!(vtime.0 < m),
            other => panic!("expected SnapshotPoint, got {other:?}"),
        }
        assert!(
            s.reseed_schedule.contains_key(&m),
            "future input remains pending"
        );
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => panic!("seal failed with a staged reseed mishandled: {other:?}"),
        }
    }

    #[test]
    fn an_sdk_stop_with_a_staged_reseed_poisons_loud() {
        const REQ_GPA: usize = 0xE000;

        let viol_id: u32 = (1 << 24) | 20;
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]);
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &payload,
            &mut frame,
        )
        .unwrap();

        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        let reseed_moment: u64 = 1_000_000;
        s.reseed_schedule.insert(reseed_moment, 9);

        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(control_proto::class_bit::ASSERTION),
            },
            resolve: None,
        };
        assert!(
            matches!(
                s.handle(&req).unwrap(),
                Err(ControlError::ScheduleUnsatisfiable { moment, .. }) if moment == reseed_moment
            ),
            "an SDK stop with a staged reseed must poison loud"
        );
        assert!(matches!(
            s.handle(&req).unwrap(),
            Err(ControlError::ScheduleUnsatisfiable { .. })
        ));
    }

    #[test]
    fn stop_mask_gates_the_sdk_snapshot_point_and_assertion() {
        const REQ_GPA: usize = 0xE000;

        let frame_for = |payload: &[u8]| -> Vec<u8> {
            let mut buf = [0u8; 4096];
            let n = hypercall_proto::encode_request(
                hypercall_proto::ServiceId::Event,
                1,
                1,
                payload,
                &mut buf,
            )
            .unwrap();
            buf[..n].to_vec()
        };
        let run_with = |rest: Vec<Exit<X86>>, frame: &[u8], on: StopMask| -> StopReason {
            let mut script = vec![Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(frame.len() as u32),
            })];
            script.extend(rest);
            let mut mb = MockBackend::with_exits(script);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 3).unwrap());
            live.wire_snapshot_hashing();
            let mut ram = vec![0u8; BIG_RAM];
            ram[REQ_GPA..REQ_GPA + frame.len()].copy_from_slice(frame);
            live.restore_guest_memory(&ram).unwrap();
            let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
            let mut s = with_test_service(ControlServer::new(live, factory));
            hello(&mut s);
            match s
                .handle(&Request::Run {
                    until: StopConditions { deadline: None, on },
                    resolve: None,
                })
                .unwrap()
            {
                Ok(Reply::Stop(stop)) => stop,
                other => panic!("run reply: {other:?}"),
            }
        };

        let setup = frame_for(&(4u32 << 24).to_le_bytes());
        assert!(
            matches!(
                run_with(
                    vec![
                        Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                        Exit::Common(CommonExit::Idle)
                    ],
                    &setup,
                    StopMask::NONE
                ),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through setup_complete straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![
                        Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                        Exit::Common(CommonExit::Idle)
                    ],
                    &setup,
                    StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT)
                ),
                StopReason::SnapshotPoint { .. }
            ),
            "arming SNAPSHOT_POINT surfaces the deferred point at the RDTSC"
        );

        let mut viol = ((1u32 << 24) | 20).to_le_bytes().to_vec();
        viol.extend_from_slice(&[1, 0, 0]);
        let viol = frame_for(&viol);
        assert!(
            matches!(
                run_with(vec![Exit::Common(CommonExit::Idle)], &viol, StopMask::NONE),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through an assertion straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![Exit::Common(CommonExit::Idle)],
                    &viol,
                    StopMask::NONE.arm(control_proto::class_bit::ASSERTION)
                ),
                StopReason::Assertion { .. }
            ),
            "arming ASSERTION surfaces the assertion"
        );
    }

    #[test]
    fn host_events_apply_at_the_first_covering_exit_boundary_in_order() {
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (1_u64).to_le_bytes().to_vec(),
                },
            ),
            (
                2,
                EnvHostEffect::XorMemory {
                    gpa: 0x48,
                    bytes: (2_u64).to_le_bytes().to_vec(),
                },
            ),
        ];
        let live = enforce_vmm(exits_to_cover(&schedule), enforce_image(), 1);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        for (m, f) in &schedule {
            s.handle(&Request::Perturb {
                fault: HostFault(f.encode()),
                at: Moment(*m),
            })
            .unwrap()
            .unwrap();
        }
        let _ = run_all(&mut s);
        assert_eq!(
            s.vmm().unwrap().effective_vns(),
            Some(2),
            "the run reached the last staged Moment"
        );
        let ram = s.vmm().unwrap().guest_memory();
        assert_ne!(&ram[0x40..0x48], &[0u8; 8], "first upset landed");
        assert_ne!(&ram[0x48..0x50], &[0u8; 8], "second upset landed");
    }

    #[test]
    fn second_fault_at_one_moment_is_loudly_rejected() {
        let live = enforce_vmm(1, enforce_image(), 0xAB);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let stage = |f: EnvHostEffect, m: u64| Request::Perturb {
            fault: HostFault(f.encode()),
            at: Moment(m),
        };
        assert_eq!(
            s.handle(&stage(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0x0F0F_0F0F_u64).to_le_bytes().to_vec(),
                },
                1,
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&stage(EnvHostEffect::InjectInterrupt { vector: 0x50 }, 1))
                .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1 }),
            "a second fault at Moment 1 is rejected (not silently dropped)"
        );
        let _ = run_all(&mut s);
        assert_ne!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the one accepted upset landed"
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "exactly the accepted fault is recorded"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn recorded_env_replays_to_the_same_hash() {
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x20,
                    bytes: (0x1234_5678_9ABC_DEF0_u64).to_le_bytes().to_vec(),
                },
            ),
            (2, EnvHostEffect::InjectInterrupt { vector: 0x60 }),
        ];
        let seed = 0xC105u64;
        let (h1, recorded) = enforce_run(&schedule, seed);

        let host: Vec<_> = recorded
            .effects()
            .iter()
            .map(|(&at, effect)| (at, effect.clone()))
            .collect();
        assert_eq!(host.len(), 2, "both applied faults were stamped");
        let reencoded = EnvSpec::decode(&recorded.encode()).expect("recorded env round-trips");
        let replay_schedule: Vec<(u64, EnvHostEffect)> = reencoded
            .effects()
            .iter()
            .map(|(&at, effect)| (at, effect.clone()))
            .collect();

        let h2 = enforce_hash(&replay_schedule, seed);
        assert_eq!(
            h1, h2,
            "the recorded env replays to the identical state_hash"
        );
    }

    fn rdtsc_then_hlt_vmm(rdtsc_work: u64) -> Vmm<MockBackend> {
        let mut m = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        let mut cfg = contract_vclock_config();
        cfg.vns_base = rdtsc_work.saturating_sub(1);
        v.wire_vtime(VtimeWiring::new_virtual_time(cfg, 1).unwrap());
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    fn rdtsc_then_hlt_server(rdtsc_work: u64) -> ControlServer<MockBackend> {
        ControlServer::new(
            rdtsc_then_hlt_vmm(rdtsc_work),
            Box::new(move || Ok(rdtsc_then_hlt_vmm(rdtsc_work))),
        )
    }

    fn stage_corrupt(s: &mut ControlServer<MockBackend>, at: u64) {
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xFFFF_FFFF_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(at),
        })
        .unwrap()
        .unwrap();
    }

    fn run_with_deadline(
        s: &mut ControlServer<MockBackend>,
        deadline: u64,
    ) -> Result<Reply, ControlError> {
        s.handle(&Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        })
        .unwrap()
    }

    #[test]
    fn a_beyond_deadline_fault_not_yet_crossed_stays_staged() {
        let mut s = rdtsc_then_hlt_server(1000);
        hello(&mut s);
        stage_corrupt(&mut s, 1500);
        match run_with_deadline(&mut s, 1000) {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(1000)),
            other => panic!("expected Deadline, got {other:?}"),
        }
        assert_eq!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the beyond-deadline fault did not apply"
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            0,
            "nothing recorded — the fault is still staged for a later run"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn snapshot_with_a_staged_fault_preserves_the_plan() {
        let mut s = rdtsc_then_hlt_server(2000);
        hello(&mut s);
        let base = snap(&mut s);
        stage_corrupt(&mut s, 3000);
        let before = hash(&mut s);
        let counts = s.vmm().unwrap().exit_counts();
        let pending = snap(&mut s);
        assert_eq!(hash(&mut s), before);
        assert_eq!(s.vmm().unwrap().exit_counts(), counts);
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(s.schedule.is_empty());
        assert_eq!(
            s.handle(&Request::Replay(pending)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut s), before);
        assert!(s.schedule.contains_key(&3000));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_env_host_faults_go_through_the_same_validation_as_perturb() {
        let host_env = |m: u64, fault: EnvHostEffect| {
            let mut spec = EnvSpec::seeded(7);
            spec.record_effect(m, fault);
            Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostEffect::XorMemory {
                        gpa: RAM as u64 - 4,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    },
                ),
            })
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        assert!(s.vmm().is_some());

        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(100, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Err(ControlError::PerturbPastMoment {
                at: 100,
                floor: 500
            })
        );

        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: {
                    let mut spec = EnvSpec::seeded(7);
                    spec.set_config(ServiceConfig {
                        identity: b"unavailable".to_vec(),
                        configuration: vec![],
                    });
                    Reproducer {
                        blob_version: EnvSpec::BLOB_VERSION,
                        bytes: spec.encode(),
                    }
                },
            })
            .unwrap(),
            Err(ControlError::Unsupported)
        );

        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    fn host_env(m: u64, fault: EnvHostEffect) -> Reproducer {
        let mut spec = EnvSpec::seeded(7);
        spec.record_effect(m, fault);
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn a_rejected_branch_env_fault_is_side_effect_free() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let before = s.vmm().unwrap().state_hash().unwrap();

        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostEffect::XorMemory {
                        gpa: RAM as u64 - 4,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    },
                ),
            })
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        assert_eq!(
            s.vmm().unwrap().state_hash().unwrap(),
            before,
            "a rejected branch env fault must leave the old VM untouched"
        );
        let base2 = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base2,
                env: seeded_env(9),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn a_terminal_stop_with_a_staged_fault_poisons_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        stage_corrupt(&mut s, 100_000);
        let poisoned = Err(ControlError::ScheduleUnsatisfiable {
            moment: 100_000,
            vtime: 500,
        });
        assert_eq!(run_all_res(&mut s), poisoned);
        assert_eq!(s.recorded_env().effects().len(), 0);
        assert_eq!(run_all_res(&mut s), poisoned);
        let failed = snap(&mut s);
        let mut artifact = Vec::new();
        let receipt = s.export_portable_snapshot(failed, &mut artifact).unwrap();
        let mut cold = server(vec![Exit::Common(CommonExit::Idle)]);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        hello(&mut cold);
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut cold), receipt.state_hash);
        assert_eq!(run_all_res(&mut cold), poisoned);
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(run_all_res(&mut cold), poisoned);
        cold.handle(&Request::Branch {
            snap: imported.id,
            env: seeded_env(9),
        })
        .unwrap()
        .unwrap();
        assert!(cold.schedule_poisoned.is_none());
        assert!(cold.schedule.is_empty());
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(matches!(
            run_all_res(&mut s),
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn perturb_after_a_synchronized_deadline_stop_reproduces() {
        let run_to = |s: &mut ControlServer<ExitBoundaryBackend>, d: u64| {
            s.handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(d)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap()
        };
        let perturb = |s: &mut ControlServer<ExitBoundaryBackend>, gpa: u64, at: u64| {
            s.handle(&Request::Perturb {
                fault: HostFault(
                    EnvHostEffect::XorMemory {
                        gpa,
                        bytes: (0xDEAD_0000_BEEF_u64).to_le_bytes().to_vec(),
                    }
                    .encode(),
                ),
                at: Moment(at),
            })
            .unwrap()
        };

        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        assert_eq!(perturb(&mut s, 0x40, 100), Ok(Reply::Unit));
        assert!(matches!(
            run_to(&mut s, 100),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert_eq!(perturb(&mut s, 0x80, 300), Ok(Reply::Unit));
        assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
        let h_live = arr_hash(&s);
        let recorded = s.recorded_env().clone();
        assert_eq!(recorded.effects().len(), 2, "both faults recorded");

        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: recorded.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(
            h_live,
            arr_hash(&r),
            "recorded env reproduces the multi-run hash"
        );
        let _ = base;
    }

    #[test]
    fn a_fault_at_current_vtime_with_an_expired_deadline_is_not_poisoned() {
        let mut s = rdtsc_then_hlt_server(500);
        hello(&mut s);
        stage_corrupt(&mut s, 500);
        match run_with_deadline(&mut s, 500) {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "the m==vns fault applied"
        );
        assert_ne!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the exit-boundary upset landed"
        );
        assert!(
            matches!(run_all_res(&mut s), Ok(Reply::Stop(_))),
            "not poisoned"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn a_branch_env_moment_occupies_the_schedule_for_ruling_b() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 41 }.encode()),
                at: Moment(1000),
            })
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1000 }),
            "a branch-staged Moment occupies the schedule for ruling B"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(384))]

        #[test]
        #[cfg_attr(miri, ignore = "VM-running property test, too slow under Miri; covered natively")]
        fn arbitrary_schedule_applied_twice_is_identical(
            schedule in proptest::collection::btree_map(
                1u64..=32u64,
                prop_oneof![
                    (0u64..(RAM as u64 - 8), any::<u64>())
                        .prop_map(|(gpa, m)| EnvHostEffect::XorMemory { gpa, bytes: m.to_le_bytes().to_vec() }),
                    (16u32..=255u32)
                        .prop_map(|vector| EnvHostEffect::InjectInterrupt { vector }),
                ],
                0..8usize,
            ),
        ) {
            let sched: Vec<(u64, EnvHostEffect)> = schedule.into_iter().collect();
            let seed = 0x9159_2653;
            let h1 = enforce_hash(&sched, seed);
            let h2 = enforce_hash(&sched, seed);
            prop_assert_eq!(h1, h2, "same schedule ⇒ identical state evolution");
        }
    }

    struct ExitBoundaryBackend {
        inner: MockBackend,
        exits_left: u64,
    }
    impl Backend for ExitBoundaryBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.inner.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.inner.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            if self.exits_left == 0 {
                self.inner.extend_exits([Exit::Common(CommonExit::Idle)]);
            } else {
                self.exits_left -= 1;
                self.inner
                    .extend_exits([Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
            }
            self.inner.run()
        }
        fn inject(&mut self, e: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.inner.inject(e)
        }
        fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
            self.inner.set_pending_irq(v)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.inner.take_accepted_interrupt()
        }
        fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
            self.inner.complete_read(v)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.inner.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.inner.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.inner.save()
        }
        fn restore(&mut self, s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
            self.inner.restore(s)?;
            self.exits_left = 512;
            Ok(())
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.inner.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.inner.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.inner.capabilities()
        }
    }

    fn exit_boundary_vmm(seed: u64) -> Vmm<ExitBoundaryBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(
            ExitBoundaryBackend {
                inner: m,
                exits_left: 512,
            },
            GuestRam::new(RAM).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    fn exit_boundary_server() -> ControlServer<ExitBoundaryBackend> {
        let live = exit_boundary_vmm(0x59);
        ControlServer::new(live, Box::new(|| Ok(exit_boundary_vmm(0x59))))
    }

    fn arr_hello<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) {
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
    }
    fn arr_snap<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) -> SnapId {
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot: {other:?}"),
        }
    }
    fn arr_run<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) -> Result<Reply, ControlError> {
        s.handle(&Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        })
        .unwrap()
    }
    fn arr_hash<B: Backend<A: Vendor>>(s: &ControlServer<B>) -> [u8; 32] {
        s.vmm().unwrap().state_hash().unwrap()
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "full and sparse import materialize snapshot-store mappings"
    )]
    fn queued_inputs_continue_identically_without_reapplying_the_consumed_prefix() {
        fn whole(s: &mut ControlServer<ExitBoundaryBackend>) -> [u8; 32] {
            match s
                .handle(&Request::Hash {
                    scope: HashScope::Whole,
                })
                .unwrap()
                .unwrap()
            {
                Reply::Hash(hash) => hash,
                other => panic!("unexpected hash reply: {other:?}"),
            }
        }
        fn run_to(s: &mut ControlServer<ExitBoundaryBackend>, at: u64) {
            assert_eq!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(at)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { vtime: Moment(at) }))
            );
        }
        fn setup() -> (ControlServer<ExitBoundaryBackend>, SnapId) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            let mut plan = EnvSpec::seeded(7);
            plan.record_effect(
                1,
                EnvHostEffect::WriteMemory {
                    gpa: 0x40,
                    bytes: vec![0x55],
                },
            );
            plan.record_effect(
                2,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: vec![0x0f],
                },
            );
            plan.record_effect(3, EnvHostEffect::InjectInterrupt { vector: 0x60 });
            plan.record_reseed(0, 7);
            plan.record_reseed(2, 99);
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: plan.encode(),
                },
            })
            .unwrap()
            .unwrap();
            run_to(&mut s, 1);
            s.exec_nonce = 17;
            (s, base)
        }
        let mut endpoints = Vec::new();
        for path in 0..4 {
            let (mut s, base) = setup();
            let before_hash = whole(&mut s);
            let before_counts = s.vmm().unwrap().exit_counts();
            let before_vm = s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap();
            if path != 0 {
                let cut = arr_snap(&mut s);
                assert_eq!(whole(&mut s), before_hash, "save executed no input");
                assert_eq!(
                    s.vmm().unwrap().exit_counts(),
                    before_counts,
                    "save entered no guest"
                );
                assert_eq!(
                    s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                    before_vm
                );
                if path >= 2 {
                    let mut cold = exit_boundary_server();
                    arr_hello(&mut cold);
                    let imported = if path == 2 {
                        let mut artifact = Vec::new();
                        let receipt = s.export_portable_snapshot(cut, &mut artifact).unwrap();
                        assert_eq!(receipt.state_hash, before_hash);
                        cold.import_portable_snapshot(artifact.as_slice())
                            .unwrap()
                            .id
                    } else {
                        let cold_base = arr_snap(&mut cold);
                        let artifact = s.export_sparse_snapshot(base, cut).unwrap();
                        cold.import_sparse_snapshot(cold_base, artifact).unwrap().id
                    };
                    drop(s);
                    cold.handle(&Request::Replay(imported)).unwrap().unwrap();
                    s = cold;
                    assert_eq!(
                        whole(&mut s),
                        before_hash,
                        "cold restore retained complete state"
                    );
                    assert_eq!(
                        s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                        before_vm
                    );
                }
            }
            assert_eq!(s.exec_nonce, 17);
            assert_eq!(s.recorded.effects().len(), 1);
            assert_eq!(s.schedule.len(), 2);
            assert_eq!(s.reseed_schedule.len(), 1);
            assert_eq!(
                s.handle(&Request::Perturb {
                    fault: HostFault(
                        EnvHostEffect::WriteMemory {
                            gpa: 0x40,
                            bytes: vec![0]
                        }
                        .encode()
                    ),
                    at: Moment(1),
                })
                .unwrap(),
                Err(ControlError::PerturbMomentTaken { at: 1 }),
                "the consumed prefix survives"
            );
            run_to(&mut s, 3);
            assert!(s.schedule.is_empty());
            assert!(s.reseed_schedule.is_empty());
            assert_eq!(
                s.vmm().unwrap().guest_memory()[0x40],
                0x5a,
                "XOR executed exactly once"
            );
            assert_eq!(s.recorded.effects().len(), 3);
            assert_eq!(s.recorded.reseeds().get(&2), Some(&99));
            let endpoint = (
                whole(&mut s),
                s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                s.recorded.encode(),
                s.vmm().unwrap().sdk_events().to_vec(),
                s.vmm().unwrap().effective_vns(),
            );
            run_to(&mut s, 3);
            assert_eq!(
                whole(&mut s),
                endpoint.0,
                "repeated continuation does not reapply inputs"
            );
            endpoints.push(endpoint);
        }
        for endpoint in &endpoints[1..] {
            assert_eq!(
                endpoint, &endpoints[0],
                "uninterrupted, saved, full cold, and sparse cold agree"
            );
        }
    }

    fn schedule_marker(at: u64) -> Request {
        Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0,
                    bytes: (0_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(at),
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn moment_address_materializes_identically_twice() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let genesis = arr_snap(&mut s);
        let env = seeded_env_arr(0x0080_0080);

        let materialize = |s: &mut ControlServer<ExitBoundaryBackend>,
                           moment: u64|
         -> (control_proto::RegsView, Vec<u8>, [u8; 32]) {
            assert_eq!(
                s.handle(&Request::Branch {
                    snap: genesis,
                    env: env.clone()
                })
                .unwrap(),
                Ok(Reply::Unit)
            );
            s.handle(&schedule_marker(moment)).unwrap().unwrap();
            let stop = match s
                .handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(moment)),
                        on: StopMask::NONE,
                    },
                    resolve: None,
                })
                .unwrap()
            {
                Ok(Reply::Stop(st)) => st,
                other => panic!("run answered {other:?}"),
            };
            assert_eq!(
                stop,
                StopReason::Deadline {
                    vtime: Moment(moment)
                },
                "materialization lands exactly at the addressed Moment"
            );
            let view = match s.handle(&Request::Regs).unwrap() {
                Ok(Reply::Regs(v)) => v,
                other => panic!("regs answered {other:?}"),
            };
            assert_eq!(
                view.moment.0, moment,
                "regs reports the exit count == the addressed Moment"
            );
            assert_eq!(view.vtime, moment, "vtime coincides with moment");
            let bytes = match s.handle(&Request::Read { gpa: 0, len: 128 }).unwrap() {
                Ok(Reply::Bytes(b)) => b,
                other => panic!("read answered {other:?}"),
            };
            (view, bytes, arr_hash(s))
        };

        for moment in [1u64, 5, 50, 250] {
            let (r1, b1, h1) = materialize(&mut s, moment);
            let (r2, b2, h2) = materialize(&mut s, moment);
            assert_eq!(
                r1, r2,
                "regs identical across two materializations @ {moment}"
            );
            assert_eq!(
                b1, b2,
                "read identical across two materializations @ {moment}"
            );
            assert_eq!(
                h1, h2,
                "hash identical across two materializations @ {moment}"
            );
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn inspection_mid_materialization_does_not_perturb_the_continuation() {
        let env = seeded_env_arr(0x0B5E_0BED);
        let (mid, late) = (10u64, 90u64);

        let run_to_late = |inspect: bool| -> [u8; 32] {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let genesis = arr_snap(&mut s);
            s.handle(&Request::Branch {
                snap: genesis,
                env: env.clone(),
            })
            .unwrap()
            .unwrap();
            s.handle(&schedule_marker(mid)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(mid)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { .. }))
            ));
            if inspect {
                let _ = s.handle(&Request::Regs).unwrap();
                let _ = s.handle(&Request::Read { gpa: 0, len: 64 }).unwrap();
                let _ = s
                    .handle(&Request::Read {
                        gpa: RAM as u64 - 16,
                        len: 16,
                    })
                    .unwrap();
                let _ = s.handle(&Request::Regs).unwrap();
            }
            s.handle(&schedule_marker(late)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(late)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { .. }))
            ));
            arr_hash(&s)
        };

        assert_eq!(
            run_to_late(true),
            run_to_late(false),
            "an inspection pass mid-materialization perturbed the continuation"
        );
    }

    #[test]
    fn reperturb_at_an_applied_moment_is_rejected() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(100),
        })
        .unwrap()
        .unwrap();
        let stop = s
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(100)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap();
        assert!(matches!(stop, Ok(Reply::Stop(StopReason::Deadline { .. }))));
        assert_eq!(s.recorded_env().effects().len(), 1, "the fault applied");
        assert_eq!(s.vmm().unwrap().effective_vns(), Some(100));
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x41 }.encode()),
                at: Moment(100),
            })
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 100 })
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "the applied fault is still recorded (not overwritten)"
        );
    }

    #[test]
    fn perturb_on_an_unarmable_backend_is_unsupported() {
        let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        let mut s = ControlServer::new(
            v,
            Box::new(|| Err(VmmError::ContractViolation("unused".into()))),
        );
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
                at: Moment(10),
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "an unarmable backend cannot enforce host faults exactly"
        );
    }

    #[test]
    fn perturb_inject_interrupt_reserved_vector_is_rejected_at_stage_time() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        for vector in [0u32, 1, 15] {
            assert_eq!(
                s.handle(&Request::Perturb {
                    fault: HostFault(EnvHostEffect::InjectInterrupt { vector }.encode()),
                    at: Moment(100),
                })
                .unwrap(),
                Err(ControlError::PerturbReservedVector {
                    vector: vector as u8
                })
            );
        }
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 16 }.encode()),
                at: Moment(100),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    fn perturb_inject_interrupt_on_a_no_lapic_vm_is_unsupported() {
        let mut s = rdtsc_then_hlt_server(500);
        hello(&mut s);
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
                at: Moment(1000),
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "no LAPIC ⇒ InjectInterrupt cannot be delivered — rejected at stage time"
        );
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(
                    EnvHostEffect::XorMemory {
                        gpa: 0x40,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    }
                    .encode(),
                ),
                at: Moment(1000),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn recoverable_restore_failure_clears_the_stale_schedule() {
        let live = exit_boundary_vmm(0x59);
        let factory = Box::new(|| {
            let mut m = MockBackend::with_exits(vec![]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            Ok(Vmm::new(
                ExitBoundaryBackend {
                    inner: m,
                    exits_left: 512,
                },
                GuestRam::new(RAM).unwrap(),
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(50),
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: EnvSpec::seeded(7).encode(),
                },
            })
            .unwrap(),
            Err(ControlError::RestoreFailed)
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            0,
            "the stale schedule/recorded was cleared on the recoverable failure"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn replay_derives_recorded_seed_from_the_restored_stream() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: EnvSpec::seeded(0xDEAD).encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x80,
                    bytes: (0x1234_5678_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(42),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
        let h_live = arr_hash(&s);
        let e = s.recorded_env().clone();

        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: e.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(
            h_live,
            arr_hash(&r),
            "recorded_env() after a replay reproduces the live hash (right stream)"
        );
    }

    #[derive(Clone, Debug)]
    enum VerbOp {
        Perturb(EnvHostEffect, u64),
        Run,
        Branch(u64),
        Replay,
        Snapshot,
    }

    fn arb_verb_op() -> impl Strategy<Value = VerbOp> {
        prop_oneof![
            (
                prop_oneof![
                    (0u64..(RAM as u64 - 8), any::<u64>()).prop_map(|(gpa, m)| {
                        EnvHostEffect::XorMemory {
                            gpa,
                            bytes: m.to_le_bytes().to_vec(),
                        }
                    }),
                    (16u32..=255u32).prop_map(|vector| EnvHostEffect::InjectInterrupt { vector }),
                ],
                1u64..=400,
            )
                .prop_map(|(f, off)| VerbOp::Perturb(f, off)),
            Just(VerbOp::Run),
            (1u64..=8).prop_map(VerbOp::Branch),
            Just(VerbOp::Replay),
            Just(VerbOp::Snapshot),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping")]
        fn verb_sequence_recorded_env_reproduces_live_hash(ops in prop::collection::vec(arb_verb_op(), 1..12)) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);

            for op in ops {
                match op {
                    VerbOp::Perturb(fault, off) => {
                        let floor = s.vmm().unwrap().effective_vns().unwrap_or(0);
                        let at = floor.saturating_add(off);
                        let _ = s.handle(&Request::Perturb {
                            fault: HostFault(fault.encode()),
                            at: Moment(at),
                        }).unwrap();
                    }
                    VerbOp::Run => {
                        match arr_run(&mut s) {
                            Ok(Reply::Stop(_)) => {
                                let e = s.recorded_env().clone();
                                let h_live = arr_hash(&s);
                                let mut r = exit_boundary_server();
                                arr_hello(&mut r);
                                let base_r = arr_snap(&mut r);
                                r.handle(&Request::Branch {
                                    snap: base_r,
                                    env: Reproducer { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
                                }).unwrap().unwrap();
                                prop_assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
                                prop_assert_eq!(h_live, arr_hash(&r), "recorded_env() must reproduce the live hash");
                            }
                            Ok(other) => prop_assert!(false, "unexpected run reply: {other:?}"),
                            Err(_) => { /* loud rejection — the model skips this op */ }
                        }
                    }
                    VerbOp::Branch(seed) => {
                        let _ = s.handle(&Request::Branch { snap: base, env: seeded_env_arr(seed) }).unwrap();
                    }
                    VerbOp::Replay => {
                        let _ = s.handle(&Request::Replay(base)).unwrap();
                    }
                    VerbOp::Snapshot => {
                        let _ = s.handle(&Request::Snapshot).unwrap();
                    }
                }
            }
        }
    }

    fn seeded_env_arr(seed: u64) -> Reproducer {
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::seeded(seed).encode(),
        }
    }

    struct IdleBackend(MockBackend);
    impl Backend for IdleBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            Ok(Exit::Common(CommonExit::Idle))
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
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.0.save()
        }
        fn restore(&mut self, s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
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

    fn idle_vmm(seed: u64) -> Vmm<IdleBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        m.set_state(vmm_backend::VcpuState {
            regs: vmm_backend::VcpuRegs {
                rflags: (1 << 9) | 0x2,
                ..Default::default()
            },
            ..Default::default()
        });
        let mut v = Vmm::new(IdleBackend(m), GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    fn idle_server() -> ControlServer<IdleBackend> {
        ControlServer::new(idle_vmm(0x1D1E), Box::new(|| Ok(idle_vmm(0x1D1E))))
    }

    fn idle_seeded_env(seed: u64) -> Reproducer {
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::seeded(seed).encode(),
        }
    }

    #[derive(Clone, Debug)]
    enum IdleOp {
        Perturb(u64, u64),
        Run,
        Branch(u64),
        Replay,
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping")]
        fn idle_hlt_before_fault_recorded_env_reproduces(ops in prop::collection::vec(
            prop_oneof![
                (0u64..(RAM as u64 - 8), 1u64..=400).prop_map(|(g, off)| IdleOp::Perturb(g, off)),
                Just(IdleOp::Run),
                (1u64..=8).prop_map(IdleOp::Branch),
                Just(IdleOp::Replay),
            ],
            1..12,
        )) {
            let mut s = idle_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            for op in ops {
                match op {
                    IdleOp::Perturb(gpa, off) => {
                        let floor = s.vmm().unwrap().effective_vns().unwrap_or(0);
                        let at = floor.saturating_add(off);
                        let _ = s.handle(&Request::Perturb {
                            fault: HostFault(EnvHostEffect::XorMemory { gpa, bytes: (0xA5A5_5A5A_u64).to_le_bytes().to_vec() }.encode()),
                            at: Moment(at),
                        }).unwrap();
                    }
                    IdleOp::Run => {
                        match arr_run(&mut s) {
                            Ok(Reply::Stop(_)) => {
                                let e = s.recorded_env().clone();
                                let h_live = arr_hash(&s);
                                let mut r = idle_server();
                                arr_hello(&mut r);
                                let base_r = arr_snap(&mut r);
                                r.handle(&Request::Branch {
                                    snap: base_r,
                                    env: Reproducer { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
                                }).unwrap().unwrap();
                                prop_assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
                                prop_assert_eq!(h_live, arr_hash(&r), "idle-path recorded_env() must reproduce the live hash");
                            }
                            Ok(other) => prop_assert!(false, "unexpected run reply: {other:?}"),
                            Err(_) => { /* loud rejection — skip */ }
                        }
                    }
                    IdleOp::Branch(seed) => {
                        let _ = s.handle(&Request::Branch { snap: base, env: idle_seeded_env(seed) }).unwrap();
                    }
                    IdleOp::Replay => {
                        let _ = s.handle(&Request::Replay(base)).unwrap();
                    }
                }
            }
        }
    }

    fn marker_env(seed: u64, markers: &[(u64, u64)]) -> Reproducer {
        let mut spec = EnvSpec::seeded(seed);
        for &(m, s) in markers {
            spec.record_reseed(m, s);
        }
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    fn reseed_arrival_requirement_is_strictly_beyond_the_restore_floor() {
        assert!(!reseed_marker_requires_arrival(99, 100));
        assert!(!reseed_marker_requires_arrival(100, 100));
        assert!(reseed_marker_requires_arrival(101, 100));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_with_a_floor_marker_reseeds_from_the_marker_not_the_env_seed() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        let mut branch_hash = |env: Reproducer| -> [u8; 32] {
            s.handle(&Request::Branch { snap: base, env })
                .unwrap()
                .unwrap();
            arr_hash(&s)
        };
        let h_marker = branch_hash(marker_env(7, &[(0, 0x1111)]));
        let h_seed = branch_hash(seeded_env(0x1111));
        let h_env_seed = branch_hash(seeded_env(7));
        assert_eq!(
            h_marker, h_seed,
            "a floor marker reseeds exactly like a plain branch on the marker's seed"
        );
        assert_ne!(
            h_marker, h_env_seed,
            "the env's own seed (7) is NOT the reseed value when markers are present"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn mid_run_reseed_marker_applies_at_its_moment_and_recorded_env_reproduces() {
        let run_leg = |mid_seed: u64| -> ([u8; 32], EnvSpec) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            s.handle(&Request::Branch {
                snap: base,
                env: marker_env(0x1111, &[(0, 0x1111), (300, mid_seed)]),
            })
            .unwrap()
            .unwrap();
            assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
            (arr_hash(&s), s.recorded_env().clone())
        };
        let (h1, rec1) = run_leg(0x2222);
        let (h1_again, _) = run_leg(0x2222);
        assert_eq!(h1, h1_again, "same markers ⇒ bit-identical terminal hash");
        let (h2, _) = run_leg(0x3333);
        assert_ne!(h2, h1, "the mid-run reseed value reaches the state");

        assert_eq!(rec1.reseeds().len(), 2, "floor + mid-run markers recorded");
        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: rec1.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(arr_hash(&r), h1, "recorded_env replays the reseed schedule");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn reseed_marker_behind_the_restore_floor_is_rejected() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(100),
        })
        .unwrap()
        .unwrap();
        let stop = s
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(100)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap();
        assert!(matches!(stop, Ok(Reply::Stop(_))));
        let base = arr_snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: marker_env(7, &[(50, 0xAB)]),
            })
            .unwrap(),
            Err(ControlError::PerturbPastMoment { at: 50, floor: 100 }),
            "a marker behind the snapshot floor can only apply later than recorded — reject"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn snapshot_with_a_staged_reseed_preserves_the_plan() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: marker_env(7, &[(0, 7), (300, 9)]),
        })
        .unwrap()
        .unwrap();
        let pending = arr_snap(&mut s);
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(s.reseed_schedule.is_empty());
        assert_eq!(
            s.handle(&Request::Replay(pending)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.reseed_schedule.get(&300), Some(&9));
        assert_eq!(s.recorded.reseeds().get(&0), Some(&7));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn terminal_with_a_staged_reseed_poisons_and_rewind_recovers() {
        let mut s = server(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: marker_env(7, &[(1_000_000, 9)]),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(
            run_all_res(&mut s),
            Err(ControlError::ScheduleUnsatisfiable {
                moment: 1_000_000,
                ..
            })
        ));
        assert!(matches!(
            run_all_res(&mut s),
            Err(ControlError::ScheduleUnsatisfiable { .. })
        ));
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(run_all_res(&mut s).is_ok());
    }

    fn drain_console(server: &mut ControlServer<MockBackend>) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            let (total, chunk) = match server
                .handle(&Request::Console {
                    offset: buf.len() as u32,
                })
                .unwrap()
            {
                Ok(Reply::Console { total, chunk }) => (total, chunk),
                other => panic!("console reply: {other:?}"),
            };
            if chunk.is_empty() {
                break;
            }
            buf.extend(chunk);
            if buf.len() as u32 >= total {
                break;
            }
        }
        buf
    }

    #[test]
    fn page_console_paging_math() {
        let serial = b"ORDER_READY\nphase one\nphase two\n";
        let (total, chunk) = page_console(serial, 0);
        assert_eq!(total as usize, serial.len());
        assert_eq!(chunk, serial);
        let (total2, chunk2) = page_console(serial, 12);
        assert_eq!(total2 as usize, serial.len());
        assert_eq!(chunk2, &serial[12..]);
        assert!(page_console(serial, serial.len()).1.is_empty());
        assert!(page_console(serial, serial.len() + 99).1.is_empty());
        assert_eq!(page_console(&[], 0), (0u32, Vec::new()));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn console_drain_is_determinism_neutral() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);

        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        run_all(&mut s);
        let without_drain = hash(&mut s);

        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        run_all(&mut s);
        let _drained = drain_console(&mut s);
        let with_drain = hash(&mut s);

        assert_eq!(
            without_drain, with_drain,
            "draining the console must not perturb state_hash (determinism-neutral)"
        );
    }

    fn seal_cut_server() -> ControlServer<MockBackend> {
        const REQ_GPA: usize = 0xE000;
        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let serial = |b: u8| {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(b as u32),
            })
        };
        let ring = Exit::Arch(X86Exit::Io {
            port: 0x0CA1,
            size: 4,
            write: Some(n as u32),
        });
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            serial(b'a'),
            ring.clone(),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            ring,
            serial(b'b'),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        live.step().unwrap();
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    fn drain_sdk(server: &mut ControlServer<MockBackend>) -> Vec<(u64, u32, Vec<u8>)> {
        let mut all = Vec::new();
        loop {
            let page = match server
                .handle(&Request::SdkEvents {
                    offset: all.len() as u32,
                })
                .unwrap()
            {
                Ok(Reply::SdkEvents(events)) => events,
                other => panic!("SdkEvents reply: {other:?}"),
            };
            if page.is_empty() {
                return all;
            }
            all.extend(page);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal logic over the mock server VM; pure safe code, logic covered natively"
    )]
    fn sdk_fixture_cuts_by_prefix_length() {
        let mut s = seal_cut_server();
        hello(&mut s);

        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "expected the first deferred snapshot point, got {stop:?}"
        );
        let (snap1, at1, n1, t1) = snap_cut(&mut s);
        assert_eq!(at1, 10002, "the seal Moment is the assigned V-time");
        assert_eq!(n1, 1, "the event emitted before the seal is included");
        assert!(!t1);

        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "expected the second deferred snapshot point, got {stop:?}"
        );
        let (snap2, at2, n2, _) = snap_cut(&mut s);
        assert_ne!(snap1, snap2);
        assert_eq!(
            (at2, n2),
            (20002, 2),
            "later exit-count boundary, strictly larger prefix"
        );

        let events = drain_sdk(&mut s);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, 10001);
        assert_eq!(events[1].0, 10002);
        assert_eq!(events[0].1, events[1].1, "identical event ids");
        assert_eq!(events[0].2, events[1].2, "identical payloads");

        assert!(matches!(
            run_all_res(&mut s),
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));

        let console = drain_console(&mut s);
        assert_eq!(console, b"ab", "the serial capture saw both bytes");
        assert_eq!(
            (n1, n2),
            (1, 2),
            "console bytes never entered the SDK count"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_and_replay_preserve_the_captured_sdk_prefix_length() {
        let mut s = seal_cut_server();
        hello(&mut s);
        assert!(matches!(
            run_seeking_snapshot(&mut s),
            StopReason::SnapshotPoint { .. }
        ));
        let (snap1, at1, n1, _) = snap_cut(&mut s);
        assert_eq!(n1, 1);
        assert!(matches!(
            run_seeking_snapshot(&mut s),
            StopReason::SnapshotPoint { .. }
        ));
        let (snap2, at2, n2, _) = snap_cut(&mut s);
        assert_eq!(n2, 2);

        assert_eq!(replay(&mut s, snap1), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 1, "replay restored the prefix");
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at1, n1), "the re-sealed cut is identical");

        assert_eq!(replay(&mut s, snap2), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 2);
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at2, n2));

        assert_eq!(branch(&mut s, snap1, 0xD1CE), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 1, "branch kept the event prefix");
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at1, n1), "the branched fork re-stamps the cut");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal logic over the mock server VM; pure safe code, logic covered natively"
    )]
    fn the_cut_is_identical_across_same_seed_sessions() {
        #[allow(clippy::type_complexity)]
        let session = || -> (
            (SnapId, u64, u64, bool),
            (SnapId, u64, u64, bool),
            Vec<(u64, u32, Vec<u8>)>,
            Vec<u8>,
        ) {
            let mut s = seal_cut_server();
            hello(&mut s);
            assert!(matches!(
                run_seeking_snapshot(&mut s),
                StopReason::SnapshotPoint { .. }
            ));
            let c1 = snap_cut(&mut s);
            assert!(matches!(
                run_seeking_snapshot(&mut s),
                StopReason::SnapshotPoint { .. }
            ));
            let c2 = snap_cut(&mut s);
            let events = drain_sdk(&mut s);
            let console = drain_console(&mut s);
            (c1, c2, events, console)
        };
        assert_eq!(session(), session(), "same seed ⇒ bit-identical cuts");
    }

    fn snap_tainted(server: &mut ControlServer<MockBackend>) -> (SnapId, bool) {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, tainted, .. }) => (id, tainted),
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    fn exec(server: &mut ControlServer<MockBackend>, cmd: &str) -> (Vec<u8>, bool) {
        match server
            .handle(&Request::Exec {
                cmd: cmd.to_string(),
                deadline: Moment(0),
            })
            .unwrap()
        {
            Ok(Reply::ExecResult { output, ok }) => (output, ok),
            other => panic!("exec reply: {other:?}"),
        }
    }

    fn recorded_env_res(server: &mut ControlServer<MockBackend>) -> Result<Reply, ControlError> {
        server.handle(&Request::RecordedEnv).unwrap()
    }

    fn branch(
        server: &mut ControlServer<MockBackend>,
        snap: SnapId,
        seed: u64,
    ) -> Result<Reply, ControlError> {
        server
            .handle(&Request::Branch {
                snap,
                env: seeded_env(seed),
            })
            .unwrap()
    }

    fn replay(
        server: &mut ControlServer<MockBackend>,
        snap: SnapId,
    ) -> Result<Reply, ControlError> {
        server.handle(&Request::Replay(snap)).unwrap()
    }

    #[test]
    fn exec_taints_and_recorded_env_then_fails_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert!(matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))));
        let (output, ok) = exec(&mut s, "ps aux");
        assert!(!ok, "deadline-0 exec on a non-shell mock does not complete");
        assert!(output.is_empty());
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests"
    )]
    fn snapshot_reply_carries_the_taint() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let (_clean, clean_at, clean_sdk, clean_taint) = snap_cut(&mut s);
        assert!(!clean_taint, "a pre-exec snapshot is untainted");
        exec(&mut s, "ls /");
        let (_dirty, dirty_at, dirty_sdk, dirty_taint) = snap_cut(&mut s);
        assert!(dirty_taint, "a post-exec snapshot is tainted");
        assert_eq!(
            (dirty_at, dirty_sdk),
            (clean_at, clean_sdk),
            "the tainted reply carries the same server-stamped cut"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn branch_and_replay_from_a_tainted_snapshot_stay_tainted() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        exec(&mut s, "ls /");
        let (tainted_snap, t) = snap_tainted(&mut s);
        assert!(t);
        assert_eq!(branch(&mut s, tainted_snap, 0x1111), Ok(Reply::Unit));
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
        assert!(
            snap_tainted(&mut s).1,
            "branch-of-tainted snapshots tainted"
        );
        assert_eq!(replay(&mut s, tainted_snap), Ok(Reply::Unit));
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn rewind_to_an_untainted_ancestor_clears_the_live_taint() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let (clean_snap, _) = snap_tainted(&mut s);
        exec(&mut s, "rm -rf /");
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
        assert_eq!(replay(&mut s, clean_snap), Ok(Reply::Unit));
        assert!(
            matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))),
            "an untainted ancestor restores an untainted, recordable timeline"
        );
        assert_eq!(branch(&mut s, clean_snap, 0x2222), Ok(Reply::Unit));
        assert!(matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))));
        assert!(!snap_tainted(&mut s).1);
    }

    #[derive(Clone, Debug)]
    enum TaintOp {
        Snapshot,
        Exec,
        Branch(usize),
        Replay(usize),
    }

    fn arb_taint_ops() -> impl Strategy<Value = Vec<TaintOp>> {
        let op = prop_oneof![
            Just(TaintOp::Snapshot),
            Just(TaintOp::Exec),
            any::<usize>().prop_map(TaintOp::Branch),
            any::<usize>().prop_map(TaintOp::Replay),
        ];
        prop::collection::vec(op, 0..40)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]
        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping")]
        fn taint_propagates_exactly_along_ancestry(ops in arb_taint_ops()) {
            let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut s);
            let mut snap_taint: Vec<bool> = Vec::new();
            let mut snap_ids: Vec<SnapId> = Vec::new();
            let mut current = false;

            for op in ops {
                match op {
                    TaintOp::Snapshot => {
                        let (id, reported) = snap_tainted(&mut s);
                        prop_assert_eq!(reported, current, "snapshot taint mismatch");
                        snap_ids.push(id);
                        snap_taint.push(current);
                    }
                    TaintOp::Exec => {
                        exec(&mut s, "echo hi");
                        current = true;
                    }
                    TaintOp::Branch(i) => {
                        if snap_ids.is_empty() {
                            continue;
                        }
                        let k = i % snap_ids.len();
                        prop_assert_eq!(branch(&mut s, snap_ids[k], 0x99), Ok(Reply::Unit));
                        current = snap_taint[k];
                    }
                    TaintOp::Replay(i) => {
                        if snap_ids.is_empty() {
                            continue;
                        }
                        let k = i % snap_ids.len();
                        prop_assert_eq!(replay(&mut s, snap_ids[k]), Ok(Reply::Unit));
                        current = snap_taint[k];
                    }
                }
                match recorded_env_res(&mut s) {
                    Ok(Reply::Recorded(_)) => prop_assert!(!current, "clean mint on a tainted timeline"),
                    Err(ControlError::Tainted) => prop_assert!(current, "Tainted on an untainted timeline"),
                    other => prop_assert!(false, "unexpected recorded_env reply: {:?}", other),
                }
            }
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the pvclock carry logic itself is pure and covered natively"
    )]
    fn branch_carries_the_pvclock_registration() {
        const REQ_GPA: usize = 0xE000;
        const PV_GPA: u64 = 0x4000;
        let compose = |exits: Vec<Exit<X86>>, _work: u64| {
            let mut mb = MockBackend::with_exits(exits);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
            v.wire_snapshot_hashing();
            v.enable_pvclock();
            v
        };
        let mut live = compose(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            100,
        );
        let mut frame = [0_u8; 64];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Pvclock,
            1,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);
        live.service_doorbell(n as u32).unwrap();
        assert_eq!(live.pvclock_registration(), Some(PV_GPA));
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        let factory = Box::new(move || {
            Ok(compose(
                vec![
                    Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                    Exit::Common(CommonExit::Idle),
                ],
                9_999,
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        match s
            .handle(&Request::Branch {
                snap: base,
                env: seeded_env(7),
            })
            .unwrap()
        {
            Ok(Reply::Unit) => {}
            other => panic!("branch failed: {other:?}"),
        }
        let vmm = s.vmm().unwrap();
        assert_eq!(vmm.pvclock_registration(), Some(PV_GPA));
        vmm.pvclock_check_oracle()
            .expect("restored page matches the restored clock");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the mismatch rejection is pure and covered natively via Vmm::pvclock_restore"
    )]
    fn branch_into_an_unoffered_composition_fails_loud() {
        const REQ_GPA: usize = 0xE000;
        const PV_GPA: u64 = 0x4000;
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        live.enable_pvclock();
        let mut frame = [0_u8; 64];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Pvclock,
            1,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        live.service_doorbell(n as u32).unwrap();
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        let factory = Box::new(|| {
            let mut mb = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(7),
            })
            .unwrap(),
            Err(ControlError::RestoreFailed),
            "a pvclock composition mismatch rejects the restore loudly"
        );
        let _ = hash(&mut s);
    }
}
