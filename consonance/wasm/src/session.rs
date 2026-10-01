// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    admission::AdmittedModule,
    meter::{Cancellation, ExecutionControl, Meter},
    runtime::{Capture, Invocation, Runtime, error},
    services::{Host, Prepared, range},
};
use consonance_client::{
    cache::{CacheIndex, Lease, Namespace},
    session::{SdkEvent, SearchSession, SessionCapabilities},
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
        let mut session = Self {
            runtime,
            meter: Meter::default(),
            input,
            factory,
            control: ExecutionControl::default(),
            snapshots: BTreeMap::new(),
            next: 1,
            setup: (SnapId(0), 0),
        };
        session.setup = session.snapshot()?;
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
        self.snapshots.remove(&previous);
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
        let mut capture = captured.capture.clone();
        capture.host.env = host;
        let candidate = Runtime::restore_trusted(self.runtime.admitted.clone(), &capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = input;
        Ok(())
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
            portable_snapshots: false,
            fresh_process_restore: false,
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
        Ok(self.runtime.store.data().console.clone())
    }
    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>> {
        Ok(self.runtime.store.data().events.clone())
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
            .map(|snapshot| (snapshot.capture.memory.len() / 4096) as u64)
    }
    fn store_bytes(&self) -> Option<u64> {
        Some(
            self.snapshots
                .values()
                .map(|snapshot| snapshot.capture.memory.len() as u64)
                .sum(),
        )
    }
    fn publish_snapshot(
        &self,
        _: &dyn CacheIndex,
        _: Namespace,
        _: &[u8],
        _: Option<(SnapId, &Lease)>,
        _: SnapId,
        _: u64,
    ) -> Result<Lease> {
        Err(error("cache export awaits portable artifact validation").into())
    }
    fn import_cached(&mut self, _: &dyn CacheIndex, _: &Lease, _: SnapId) -> Result<(SnapId, u64)> {
        Err(error("cache import awaits portable artifact validation").into())
    }
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<()> {
        self.control.check()?;
        let captured = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
        let candidate = Runtime::restore_trusted(self.runtime.admitted.clone(), &captured.capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = captured.input.clone();
        Ok(())
    }
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<()> {
        if snapshot == self.setup.0 {
            return Err(error("setup snapshot is retained by the session").into());
        }
        self.snapshots
            .remove(&snapshot)
            .ok_or_else(|| error("unknown snapshot"))?;
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
        let mut capture = captured.capture.clone();
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
        let candidate = Runtime::restore_trusted(self.runtime.admitted.clone(), &capture)?;
        self.runtime = candidate;
        self.meter = captured.meter.clone();
        self.input = input;
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
        let snapshot = Snapshot {
            capture: self.runtime.capture()?,
            meter: self.meter.clone(),
            input: self.input.clone(),
        };
        self.snapshots.insert(id, Rc::new(snapshot));
        self.next = next;
        Ok((id, at))
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
