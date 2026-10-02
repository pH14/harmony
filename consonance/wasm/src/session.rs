// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    admission::AdmittedModule,
    artifact::{self, Body, Sections},
    meter::{Cancellation, ExecutionControl, Meter},
    runtime::{Capture, Invocation, Runtime, error},
    services::{Host, Prepared, range},
};
use consonance_client::{
    cache::{CacheIndex, Lease, Namespace},
    session::{SdkEvent, SearchSession, SessionCapabilities, SparseSnapshot},
};
use control_proto::{
    CrashInfo, CrashKind, Moment, Resolution, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{
    channel::Effect,
    input_spec::{InputSpec, ServiceConfig, ServiceFactory},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, rc::Rc, sync::Arc};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Clone, Debug)]
struct Snapshot {
    capture: Capture,
    meter: Meter,
    input: InputSpec,
    storage: snapshot_store::SnapshotId,
    body: Body,
    sections: Sections,
}
pub struct WasmSession {
    runtime: Runtime,
    meter: Meter,
    input: InputSpec,
    factory: ServiceFactory,
    control: ExecutionControl,
    snapshots: BTreeMap<SnapId, Rc<Snapshot>>,
    next: u64,
    setup: (SnapId, u64),
    store: snapshot_store::Store,
    near: Option<SnapId>,
    dirty: Vec<u64>,
    restore_pages: u64,
}
impl std::fmt::Debug for WasmSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmSession")
            .field("identity", &self.runtime.admitted.execution_digest())
            .field("snapshots", &self.snapshots.len())
            .field("abandoned", &self.control.abandoned())
            .finish()
    }
}
impl WasmSession {
    pub fn debug_map(&self) -> crate::source_map::DebugMap {
        self.runtime.debug_map()
    }
    pub fn new(
        module: AdmittedModule,
        input: InputSpec,
        entry: Invocation,
        factory: ServiceFactory,
    ) -> Result<Self> {
        if !input.effects().is_empty() || !input.reseeds().is_empty() {
            return Err(error("scheduled machine effects and reseeds are unsupported").into());
        }
        let host = Host::new(input.materialize(&factory)?);
        let runtime = Runtime::new(Arc::new(module), host, entry)?;
        let store = snapshot_store::Store::new(snapshot_store::StoreConfig {
            mem_pages: u64::from(runtime.admitted.profile().memory_pages) * 16,
        });
        let mut session = Self {
            runtime,
            meter: Meter::default(),
            input,
            factory,
            control: ExecutionControl::default(),
            snapshots: BTreeMap::new(),
            next: 1,
            setup: (SnapId(0), 0),
            store,
            near: None,
            dirty: Vec::new(),
            restore_pages: 0,
        };
        session.setup = session.snapshot()?;
        Ok(session)
    }
    pub fn from_snapshot(
        module: AdmittedModule,
        snapshot: &SparseSnapshot,
        factory: ServiceFactory,
    ) -> Result<Self> {
        let (capture, meter, input, sections, body) =
            artifact::decode(&module, snapshot, &factory)?;
        let runtime = Runtime::restore(Arc::new(module), &capture)?;
        let store = snapshot_store::Store::new(snapshot_store::StoreConfig {
            mem_pages: u64::from(runtime.admitted.profile().memory_pages) * 16,
        });
        let at = meter.moment(capture.fuel)?;
        let mut session = Self {
            runtime,
            meter: meter.clone(),
            input: input.clone(),
            factory,
            control: ExecutionControl::default(),
            snapshots: BTreeMap::new(),
            next: 1,
            setup: (SnapId(1), at),
            store,
            near: Some(SnapId(1)),
            dirty: Vec::new(),
            restore_pages: (capture.memory.len() / 4096) as u64,
        };
        session.insert_snapshot(SnapId(1), 2, capture, meter, input, sections, body, None)?;
        Ok(session)
    }
    pub fn cancellation(&self) -> Cancellation {
        self.control.cancellation()
    }
    #[cfg(not(miri))]
    pub fn run_with_watchdog(
        &mut self,
        until: StopConditions,
        resolve: Option<Resolution>,
        idle_budget: std::time::Duration,
    ) -> Result<StopReason> {
        self.control.check()?;
        let watchdog = consonance_client::watchdog::Watchdog::start_with_progress(
            idle_budget,
            self.control.cancellation().flag,
            self.control.progress(),
        )?;
        let result = self.drive(until, resolve);
        if !watchdog.claim() {
            self.control.check()?;
            return Err(error("watchdog abandoned the execution").into());
        }
        result
    }
    pub fn current_moment(&self) -> Result<u64> {
        Ok(self.meter.moment(self.runtime.store.get_fuel()?)?)
    }
    pub fn seal_setup(&mut self) -> Result<(SnapId, u64)> {
        self.control.check()?;
        let setup = self.snapshot()?;
        let previous = self.setup.0;
        self.setup = setup;
        if let Some(snapshot) = self.snapshots.remove(&previous) {
            self.store.release(snapshot.storage)?;
            self.store.gc();
        }
        Ok(setup)
    }
    pub fn branch_input(&mut self, snapshot: SnapId, input: InputSpec) -> Result<()> {
        self.control.check()?;
        if !input.effects().is_empty() || !input.reseeds().is_empty() {
            return Err(error("scheduled machine effects and reseeds are unsupported").into());
        }
        let captured = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        let host = input.materialize(&self.factory)?;
        let mut capture = self.materialize(captured)?;
        capture.host.env = host;
        let candidate = self.runtime.restore_cached(&capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = input;
        self.near = Some(snapshot);
        self.restore_pages = (capture.memory.len() / 4096) as u64;
        Ok(())
    }
    fn materialize(&self, snapshot: &Snapshot) -> Result<Capture> {
        if self.store.vm_state(snapshot.storage)? != snapshot.body.encode()? {
            return Err(error("snapshot sidecar differs from its sealed state").into());
        }
        let mut capture = snapshot.capture.clone();
        capture.memory = vec![0; self.runtime.admitted.profile().memory_pages as usize * 65536];
        for (gfn, page) in self.store.diff_pages(None, snapshot.storage)? {
            let at = gfn as usize * 4096;
            capture.memory[at..at + 4096].copy_from_slice(page);
        }
        snapshot.body.verify_memory(&capture.memory)?;
        Ok(capture)
    }
    #[allow(clippy::too_many_arguments)]
    fn insert_snapshot(
        &mut self,
        id: SnapId,
        next: u64,
        mut capture: Capture,
        meter: Meter,
        input: InputSpec,
        sections: Sections,
        body: Body,
        parent: Option<SnapId>,
    ) -> Result<()> {
        let parent = parent
            .and_then(|parent| self.snapshots.get(&parent))
            .map(|parent| parent.storage);
        let (storage, dirty) = write_snapshot(&mut self.store, &capture.memory, &body, parent)?;
        self.dirty = dirty;
        capture.memory = Vec::new();
        self.record_snapshot(
            id,
            next,
            Snapshot {
                capture,
                meter,
                input,
                sections,
                body,
                storage,
            },
        );
        Ok(())
    }
    fn record_snapshot(&mut self, id: SnapId, next: u64, snapshot: Snapshot) {
        self.snapshots.insert(id, Rc::new(snapshot));
        self.next = next;
    }
    fn drive(
        &mut self,
        until: StopConditions,
        mut resolve: Option<Resolution>,
    ) -> Result<StopReason> {
        self.control.check()?;
        let deadline = until
            .deadline
            .map(|at| Meter::rounded_deadline(at.0))
            .transpose()?;
        if resolve.is_some() && self.runtime.store.data().pending.is_none() {
            return Err(error("resolution requires a pending import").into());
        }
        loop {
            self.control.check()?;
            let moment = self.current_moment()?;
            self.control.record_progress(moment);
            if self.runtime.store.data().pending.is_some() {
                let prepared = self.runtime.store.data().prepare(
                    self.runtime.memory.data(&self.runtime.store),
                    moment,
                    until,
                    resolve.as_ref(),
                )?;
                match prepared {
                    Prepared::External(question) => {
                        return Ok(Host::decision_stop(&question, moment));
                    }
                    Prepared::Complete(completion) => {
                        let input = if let Some(resolve) = &resolve {
                            let mut input = self.input.clone();
                            input.record_answer(
                                moment,
                                resolve.service,
                                resolve.id.0,
                                environment::channel::Answer::decode(&resolve.answer.0)?,
                            )?;
                            Some(input)
                        } else {
                            None
                        };
                        let mut meter = self.meter.clone();
                        meter.charge_service(completion.request_bytes, completion.answer_bytes)?;
                        for (pointer, bytes) in &completion.writes {
                            range(
                                self.runtime.memory.data(&self.runtime.store),
                                *pointer as u64,
                                bytes.len() as u64,
                            )?;
                        }
                        let call = self
                            .runtime
                            .call
                            .as_mut()
                            .ok_or_else(|| error("pending import lacks a continuation"))?;
                        call.harmony_complete_i32(&self.runtime.store, completion.value)?;
                        for (pointer, bytes) in completion.writes {
                            self.runtime
                                .memory
                                .write(&mut self.runtime.store, pointer, &bytes)?;
                        }
                        *self.runtime.store.data_mut() = completion.host;
                        self.meter = meter;
                        if let Some(input) = input {
                            self.input = input;
                        }
                        resolve = None;
                        let actual = self.current_moment()?;
                        self.control.record_progress(actual);
                        if let Some(stop) = completion.stop {
                            return Ok(with_moment(stop, actual));
                        }
                    }
                }
            }
            let moment = self.current_moment()?;
            if let Some(detail) = &self.runtime.trap {
                return Ok(StopReason::Crash {
                    vtime: Moment(moment),
                    info: CrashInfo {
                        kind: CrashKind::UnrecoverableFault,
                        detail: detail.as_bytes().to_vec(),
                    },
                });
            }
            if self.runtime.finished {
                return Ok(StopReason::Quiescent {
                    vtime: Moment(moment),
                });
            }
            if deadline.is_some_and(|deadline| moment >= deadline) {
                return Ok(StopReason::Deadline {
                    vtime: Moment(moment),
                });
            }
            let needs_grant = !self.runtime.started
                || matches!(self.runtime.call.as_ref(), Some(wasmi::ResumableCall::OutOfFuel(call)) if call.required_fuel() != 0);
            if needs_grant {
                let fuel = self.meter.grant(self.runtime.store.get_fuel()?)?;
                self.runtime.store.set_fuel(fuel)?;
            }
            self.runtime.execute()?;
        }
    }
}
fn write_snapshot(
    store: &mut snapshot_store::Store,
    memory: &[u8],
    body: &Body,
    parent: Option<snapshot_store::SnapshotId>,
) -> Result<(snapshot_store::SnapshotId, Vec<u64>)> {
    let sidecar = body.encode()?;
    if let Some(parent) = parent {
        let changed: Vec<_> = store
            .diff_pages(None, parent)?
            .into_iter()
            .filter_map(|(gfn, page)| {
                let at = gfn as usize * 4096;
                (memory[at..at + 4096] != page[..]).then_some(gfn)
            })
            .collect();
        let mut builder = store.derive(parent)?;
        for &gfn in &changed {
            let at = gfn as usize * 4096;
            builder.write_page(gfn, &memory[at..at + 4096])?;
        }
        Ok((builder.seal(sidecar), changed))
    } else {
        let mut builder = store.begin_base();
        for (gfn, page) in memory.chunks_exact(4096).enumerate() {
            builder.write_page(gfn as u64, page)?;
        }
        Ok((
            builder.seal(sidecar),
            (0..memory.len() as u64 / 4096).collect(),
        ))
    }
}
fn with_moment(stop: StopReason, moment: u64) -> StopReason {
    match stop {
        StopReason::SnapshotPoint { .. } => StopReason::SnapshotPoint {
            vtime: Moment(moment),
        },
        StopReason::Assertion { ev, .. } => StopReason::Assertion {
            vtime: Moment(moment),
            ev,
        },
        StopReason::Quiescent { .. } => StopReason::Quiescent {
            vtime: Moment(moment),
        },
        _ => unreachable!(),
    }
}
impl SearchSession for WasmSession {
    fn setup_handle(&self) -> (SnapId, u64) {
        self.setup
    }
    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            workload_composition: true,
            stopped_observations: true,
            machine_effects: false,
            portable_snapshots: true,
            fresh_process_restore: true,
        }
    }
    fn state_hash(&mut self) -> Result<[u8; 32]> {
        let capture = self.runtime.capture()?;
        let mut hash = Sha256::new();
        hash.update(b"harmony-wasm-state-v1");
        hash.update(self.runtime.admitted.execution_digest());
        hash.update(self.input.encode());
        hash.update(postcard::to_allocvec(&self.meter)?);
        hash.update(&capture.memory);
        hash.update(postcard::to_allocvec(&(
            &capture.globals,
            &capture.tables,
            capture.fuel,
            &capture.continuation,
            &capture.data,
            &capture.elements,
            &capture.invocation,
            capture.started,
            capture.finished,
            &capture.trap,
            &capture.outputs,
        ))?);
        hash.update(capture.host.env.snapshot_state()?.encode());
        hash.update(postcard::to_allocvec(&(
            &capture.host.pending,
            &capture.host.observations,
            &capture.host.thresholds,
            &capture.host.events,
            &capture.host.console,
            capture.host.closed,
            capture.host.imports,
        ))?);
        Ok(hash.finalize().into())
    }
    fn console_tail(&mut self) -> Result<Vec<u8>> {
        Ok(self.runtime.store.data().console.materialize())
    }
    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>> {
        Ok(self.runtime.store.data().events()?)
    }
    fn telemetry_counters(&self) -> Vec<(String, u64)> {
        vec![
            ("wasm_imports".into(), self.runtime.store.data().imports),
            ("wasm_snapshots".into(), self.snapshots.len() as u64),
        ]
    }
    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        self.snapshots
            .get(&snapshot)
            .and_then(|snapshot| self.store.stats(snapshot.storage).ok())
            .map(|stats| stats.owned_pages)
    }
    fn store_bytes(&self) -> Option<u64> {
        let mut chunks = std::collections::BTreeSet::new();
        let mut execution = 0;
        for snapshot in self.snapshots.values() {
            for section in &snapshot.sections.0 {
                for chunk in section.chunks() {
                    chunks.insert(Arc::as_ptr(chunk) as usize);
                }
            }
            if let Some(continuation) = &snapshot.capture.continuation {
                execution +=
                    continuation.values.capacity() * 8 + continuation.frames.capacity() * 40;
            }
        }
        Some(self.store.store_stats().bytes_resident + (chunks.len() * 512 + execution) as u64)
    }
    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease> {
        use consonance_client::cache::extent::{extent_len, write_extent};
        let parent =
            parent.filter(|(_, lease)| lease.depth() + 1 < consonance_client::cache::ANCHOR_DEPTH);
        let target = self.export_sparse_snapshot(target, None)?;
        let parent_artifact = parent
            .map(|(id, _)| self.export_sparse_snapshot(id, None))
            .transpose()?;
        let old: BTreeMap<_, _> = parent_artifact
            .as_ref()
            .map(|artifact| {
                artifact
                    .pages()
                    .iter()
                    .map(|(gfn, page)| (*gfn, page))
                    .collect()
            })
            .unwrap_or_default();
        let pages: Vec<_> = target
            .pages()
            .iter()
            .filter(|(gfn, page)| {
                old.get(gfn)
                    .is_none_or(|prior| prior.as_ref() != page.as_ref())
            })
            .map(|(gfn, page)| (*gfn, *blake3::hash(page.as_ref()).as_bytes(), page.as_ref()))
            .collect();
        let new: std::collections::BTreeSet<_> =
            target.pages().iter().map(|(gfn, _)| *gfn).collect();
        let reverted: Vec<_> = old
            .keys()
            .filter(|gfn| !new.contains(gfn))
            .copied()
            .collect();
        let sidecar = target.sidecar();
        let len = extent_len(pages.len(), reverted.len(), sidecar.len())
            .ok_or_else(|| error("cache extent length overflow"))?;
        let mut extent = index.extent(len)?;
        let borrowed: Vec<_> = pages
            .iter()
            .map(|(gfn, hash, page)| (*gfn, hash, *page))
            .collect();
        write_extent(extent.bytes_mut(), &borrowed, &reverted, &sidecar)?;
        Ok(index.publish(namespace, key, parent.map(|(_, lease)| lease), extent, cost)?)
    }
    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64)> {
        use consonance_client::cache::extent::{read_extent, resolve};
        self.control.check()?;
        if !self.snapshots.contains_key(&near) {
            return Err(error("unknown nearby snapshot").into());
        }
        let chain = index.chain(lease)?;
        let deltas = chain
            .iter()
            .map(|extent| read_extent(extent.bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let resolved = resolve(&deltas)?;
        let pages = resolved
            .pages
            .iter()
            .map(|(gfn, _, page)| (*gfn, Arc::new(**page)))
            .collect();
        let artifact = SparseSnapshot::from_parts(
            0,
            self.runtime.admitted.execution_digest(),
            pages,
            resolved.sidecar,
            None,
        )
        .map_err(error)?;
        let id = self.import_sparse_snapshot(&artifact)?;
        Ok((id, self.snapshot_time(id).unwrap()))
    }
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<()> {
        self.control.check()?;
        let captured = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        let capture = self.materialize(captured)?;
        let candidate = self.runtime.restore_cached(&capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = captured.input.clone();
        self.near = Some(snapshot);
        self.restore_pages = (capture.memory.len() / 4096) as u64;
        Ok(())
    }
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<()> {
        if snapshot == self.setup.0 {
            return Err(error("setup snapshot is retained by the session").into());
        }
        let removed = self
            .snapshots
            .remove(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        self.store.release(removed.storage)?;
        self.store.gc();
        if self.near == Some(snapshot) {
            self.near = self.snapshots.keys().next_back().copied();
        }
        Ok(())
    }
    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
    ) -> Result<()> {
        self.control.check()?;
        if !effects.is_empty() {
            return Err(error("machine effects are unsupported").into());
        }
        let captured = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        let mut capture = self.materialize(captured)?;
        let mut input = captured.input.clone();
        if &config != input.config() {
            let mut handler = (self.factory)(&config)?;
            if handler.identity() != config.identity
                || handler.configuration() != config.configuration
            {
                return Err(error("factory returned a different service contract").into());
            }
            handler.reseed(input.seed());
            capture.host.env.replace_handler(handler);
            input.set_config(config);
        }
        capture.host.env.set_payloads(Some(payloads.clone()))?;
        input.set_payloads(Some(payloads));
        let candidate = self.runtime.restore_cached(&capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = input;
        self.near = Some(snapshot);
        self.restore_pages = (capture.memory.len() / 4096) as u64;
        Ok(())
    }
    fn branch_payloads(&mut self, snapshot: SnapId, payloads: Vec<Vec<u8>>) -> Result<()> {
        let config = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?
            .input
            .config()
            .clone();
        self.branch_with_service(snapshot, config, payloads, Vec::new())
    }
    fn run_until(&mut self, deadline: u64) -> Result<StopReason> {
        self.drive(
            StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            None,
        )
    }
    fn run(&mut self, until: StopConditions, resolve: Option<Resolution>) -> Result<StopReason> {
        #[cfg(not(miri))]
        {
            self.run_with_watchdog(until, resolve, std::time::Duration::from_secs(5))
        }
        #[cfg(miri)]
        self.drive(until, resolve)
    }
    fn snapshot(&mut self) -> Result<(SnapId, u64)> {
        self.control.check()?;
        let at = self.current_moment()?;
        let id = SnapId(self.next);
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| error("snapshot handles exhausted"))?;
        let capture = self.runtime.capture_without_memory()?;
        let base = self.near.and_then(|near| self.snapshots.get(&near));
        let sections = Sections::capture(&capture, &self.input, base.map(|base| &base.sections))?;
        let memory = self.runtime.memory.data(&self.runtime.store);
        let body = Body::capture(
            &self.runtime.admitted,
            &capture,
            &self.meter,
            &sections,
            memory,
        )?;
        let parent = base.map(|base| base.storage);
        let (storage, dirty) = write_snapshot(&mut self.store, memory, &body, parent)?;
        let input = self.input.clone();
        let meter = self.meter.clone();
        self.dirty = dirty;
        self.record_snapshot(
            id,
            next,
            Snapshot {
                capture,
                meter,
                input,
                sections,
                body,
                storage,
            },
        );
        self.near = Some(id);
        Ok((id, at))
    }
    fn export_sparse_snapshot(
        &self,
        snapshot: SnapId,
        base: Option<&SparseSnapshot>,
    ) -> Result<SparseSnapshot> {
        let stored = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        let capture = self.materialize(stored)?;
        Ok(artifact::export(
            &stored.body,
            &capture.memory,
            &stored.sections,
            base,
        )?)
    }
    fn import_sparse_snapshot(&mut self, snapshot: &SparseSnapshot) -> Result<SnapId> {
        self.control.check()?;
        let (capture, meter, input, sections, body) =
            artifact::decode(&self.runtime.admitted, snapshot, &self.factory)?;
        let candidate = self.runtime.restore_cached(&capture)?;
        drop(candidate);
        let id = SnapId(self.next);
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| error("snapshot handles exhausted"))?;
        self.insert_snapshot(id, next, capture, meter, input, sections, body, None)?;
        Ok(id)
    }
    fn last_seal_dirty_gfns(&self) -> Option<Vec<u64>> {
        Some(self.dirty.clone())
    }
    fn snapshot_chain_len(&self, snapshot: SnapId) -> Option<u32> {
        self.snapshots
            .get(&snapshot)
            .and_then(|snapshot| self.store.stats(snapshot.storage).ok())
            .map(|stats| stats.chain_len)
    }
    fn last_restore_stats(&self) -> (u64, u64) {
        (self.restore_pages, self.restore_pages)
    }

    fn snapshot_time(&self, snapshot: SnapId) -> Option<u64> {
        self.snapshots
            .get(&snapshot)
            .and_then(|snapshot| snapshot.meter.moment(snapshot.capture.fuel).ok())
    }
    fn read_observation(&mut self, handle: u32, offset: u32, len: u32) -> Result<Vec<u8>> {
        self.control.check()?;
        let (address, capacity) = self
            .runtime
            .store
            .data()
            .observations
            .get(&handle)
            .copied()
            .ok_or_else(|| error("observation handle is unavailable"))?;
        if offset.checked_add(len).is_none_or(|end| end > capacity) {
            return Err(error("observation read exceeds registration").into());
        }
        let memory = self.runtime.memory.data(&self.runtime.store);
        Ok(memory[range(memory, address + u64::from(offset), u64::from(len))?].to_vec())
    }
    fn abandoned(&self) -> bool {
        self.control.abandoned()
    }
}
