// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use vmm_backend::{Backend, ExitReason};
use vtime::{IdlePlanner, TimerQueue, TimerToken, VClock, VClockConfig};

pub const PLACEHOLDER_INTERRUPT_CONTROLLER_MMIO_VNS: u64 = 1;

pub const PLACEHOLDER_SERIAL_MMIO_VNS: u64 = 1;

pub const PLACEHOLDER_PARAVIRTUAL_DEVICE_MMIO_VNS: u64 = 1;

pub const PLACEHOLDER_TRAPPED_TIME_READ_VNS: u64 = 1;

pub const PLACEHOLDER_ARCHITECTURAL_CONTROL_VNS: u64 = 1;

pub const PLACEHOLDER_EXECUTION_TICK_VNS: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceClass {
    InterruptController,
    Serial,
    Paravirtual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualTimeTiming {
    pub interrupt_controller_mmio_vns: u64,
    pub serial_mmio_vns: u64,
    pub paravirtual_device_mmio_vns: u64,
    pub trapped_time_read_vns: u64,
    pub architectural_control_vns: u64,
    pub execution_tick_vns: u64,
}

impl Default for VirtualTimeTiming {
    fn default() -> Self {
        Self {
            interrupt_controller_mmio_vns: PLACEHOLDER_INTERRUPT_CONTROLLER_MMIO_VNS,
            serial_mmio_vns: PLACEHOLDER_SERIAL_MMIO_VNS,
            paravirtual_device_mmio_vns: PLACEHOLDER_PARAVIRTUAL_DEVICE_MMIO_VNS,
            trapped_time_read_vns: PLACEHOLDER_TRAPPED_TIME_READ_VNS,
            architectural_control_vns: PLACEHOLDER_ARCHITECTURAL_CONTROL_VNS,
            execution_tick_vns: PLACEHOLDER_EXECUTION_TICK_VNS,
        }
    }
}

impl VirtualTimeTiming {
    pub(crate) fn mmio_vns(self, class: DeviceClass) -> u64 {
        match class {
            DeviceClass::InterruptController => self.interrupt_controller_mmio_vns,
            DeviceClass::Serial => self.serial_mmio_vns,
            DeviceClass::Paravirtual => self.paravirtual_device_mmio_vns,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizedEventClass {
    Doorbell,
    DeviceMmio(DeviceClass),
    TimeRead,
    ArchitecturalControl,
    Idle,
    Terminal,
}

impl NormalizedEventClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Doorbell => "doorbell",
            Self::DeviceMmio(DeviceClass::InterruptController) => "ic_mmio",
            Self::DeviceMmio(DeviceClass::Serial) => "serial_mmio",
            Self::DeviceMmio(DeviceClass::Paravirtual) => "pv_mmio",
            Self::TimeRead => "time_read",
            Self::ArchitecturalControl => "arch_control",
            Self::Idle => "idle",
            Self::Terminal => "terminal",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::Doorbell => 0,
            Self::DeviceMmio(DeviceClass::InterruptController) => 1,
            Self::DeviceMmio(DeviceClass::Serial) => 2,
            Self::DeviceMmio(DeviceClass::Paravirtual) => 3,
            Self::TimeRead => 4,
            Self::Idle => 5,
            Self::Terminal => 6,
            Self::ArchitecturalControl => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdvanceRule {
    Doorbell(u64),
    DeviceMmio(DeviceClass),
    TimeRead,
    ArchitecturalControl,
    Idle,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedExit {
    class: NormalizedEventClass,
    payload: Vec<u8>,
    advance: AdvanceRule,
    terminal: bool,
}

impl ClassifiedExit {
    pub fn doorbell(payload: Vec<u8>, duration_vns: u64) -> Self {
        Self {
            class: NormalizedEventClass::Doorbell,
            payload,
            advance: AdvanceRule::Doorbell(duration_vns),
            terminal: false,
        }
    }

    pub fn device_mmio(class: DeviceClass, payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::DeviceMmio(class),
            payload,
            advance: AdvanceRule::DeviceMmio(class),
            terminal: false,
        }
    }

    pub fn time_read(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::TimeRead,
            payload,
            advance: AdvanceRule::TimeRead,
            terminal: false,
        }
    }

    pub fn architectural_control(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::ArchitecturalControl,
            payload,
            advance: AdvanceRule::ArchitecturalControl,
            terminal: false,
        }
    }

    pub fn idle(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::Idle,
            payload,
            advance: AdvanceRule::Idle,
            terminal: false,
        }
    }

    pub fn terminal(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::Terminal,
            payload,
            advance: AdvanceRule::None,
            terminal: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledInterrupt {
    pub deadline_vns: u64,
    pub schedule_index: u64,
    pub armed_for_event: u64,
    pub canceled_at_event: Option<u64>,
    pub interrupt_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptDelivery {
    pub deadline_vns: u64,
    pub schedule_index: u64,
    pub interrupt_id: u32,
}

impl From<ScheduledInterrupt> for InterruptDelivery {
    fn from(value: ScheduledInterrupt) -> Self {
        Self {
            deadline_vns: value.deadline_vns,
            schedule_index: value.schedule_index,
            interrupt_id: value.interrupt_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub event_index: u64,
    pub portable_event_index: Option<u64>,
    pub reason: ExitReason,
    pub backend_debug: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvent {
    pub event_index: u64,
    pub class: NormalizedEventClass,
    pub payload_digest: [u8; 32],
    pub vns_after: u64,
    pub interrupts: Vec<InterruptDelivery>,
    pub state_hash: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizedLog {
    pub events: Vec<NormalizedEvent>,
}

#[derive(Clone, Debug)]
struct PendingLiveEvent {
    raw: RawEvent,
    class: NormalizedEventClass,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct LiveVirtualTimeTrace {
    raw: Vec<RawEvent>,
    normalized: NormalizedLog,
    schedule: Vec<ScheduledInterrupt>,
    next_schedule_index: u64,
    active_clockevent_schedule: Option<u64>,
    pending: Option<PendingLiveEvent>,
    current_interrupts: Vec<InterruptDelivery>,
}

impl LiveVirtualTimeTrace {
    pub fn raw_log(&self) -> &[RawEvent] {
        &self.raw
    }

    pub fn normalized_log(&self) -> &NormalizedLog {
        &self.normalized
    }

    pub fn schedule(&self) -> &[ScheduledInterrupt] {
        &self.schedule
    }

    pub fn normalized_digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"consonance.live-prescriptive-log.v1\0");
        h.update(
            u64::try_from(self.normalized.events.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for event in &self.normalized.events {
            h.update(event.event_index.to_le_bytes());
            h.update([event.class.tag()]);
            h.update(event.payload_digest);
            h.update(event.vns_after.to_le_bytes());
            h.update(
                u64::try_from(event.interrupts.len())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for delivery in &event.interrupts {
                h.update(delivery.deadline_vns.to_le_bytes());
                h.update(delivery.schedule_index.to_le_bytes());
                h.update(delivery.interrupt_id.to_le_bytes());
            }
            match event.state_hash {
                Some(hash) => {
                    h.update([1]);
                    h.update(hash);
                }
                None => h.update([0]),
            }
        }
        h.update(
            u64::try_from(self.schedule.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for scheduled in &self.schedule {
            h.update(scheduled.deadline_vns.to_le_bytes());
            h.update(scheduled.schedule_index.to_le_bytes());
            h.update(scheduled.armed_for_event.to_le_bytes());
            match scheduled.canceled_at_event {
                Some(event) => {
                    h.update([1]);
                    h.update(event.to_le_bytes());
                }
                None => h.update([0]),
            }
            h.update(scheduled.interrupt_id.to_le_bytes());
        }
        h.finalize().into()
    }

    pub(crate) fn begin(
        &mut self,
        reason: ExitReason,
        backend_debug: String,
        class: NormalizedEventClass,
        payload: Vec<u8>,
    ) -> Result<(), &'static str> {
        if self.pending.is_some() {
            return Err("virtual_time trace began a second event before finishing the first");
        }
        let event_index = u64::try_from(self.raw.len()).unwrap_or(u64::MAX);
        self.pending = Some(PendingLiveEvent {
            raw: RawEvent {
                event_index,
                portable_event_index: Some(
                    u64::try_from(self.normalized.events.len()).unwrap_or(u64::MAX),
                ),
                reason,
                backend_debug,
            },
            class,
            payload,
        });
        self.current_interrupts.clear();
        Ok(())
    }

    pub(crate) fn current_event_index(&self) -> Result<u64, &'static str> {
        self.pending
            .as_ref()
            .map(|_| u64::try_from(self.normalized.events.len()).unwrap_or(u64::MAX))
            .ok_or("virtual_time trace operation outside an active event")
    }

    pub(crate) fn record_raw_only(
        &mut self,
        reason: ExitReason,
        backend_debug: String,
    ) -> Result<(), &'static str> {
        if self.pending.is_some() {
            return Err("virtual_time trace recorded a raw-only exit during an active event");
        }
        self.raw.push(RawEvent {
            event_index: u64::try_from(self.raw.len()).unwrap_or(u64::MAX),
            portable_event_index: None,
            reason,
            backend_debug,
        });
        Ok(())
    }

    pub(crate) fn schedule_clockevent(
        &mut self,
        deadline_vns: u64,
        interrupt_id: u32,
    ) -> Result<(), &'static str> {
        let event = self.current_event_index()?;
        self.cancel_clockevent_at(event)?;
        let schedule_index = self.next_schedule_index;
        self.next_schedule_index = self
            .next_schedule_index
            .checked_add(1)
            .ok_or("virtual_time clockevent schedule index exhausted")?;
        self.schedule.push(ScheduledInterrupt {
            deadline_vns,
            schedule_index,
            armed_for_event: event,
            canceled_at_event: None,
            interrupt_id,
        });
        self.active_clockevent_schedule = Some(schedule_index);
        Ok(())
    }

    pub(crate) fn restore_clockevent_schedule(
        &mut self,
        restored: Option<(u64, u32)>,
    ) -> Result<(), &'static str> {
        if self.pending.is_some() {
            return Err("virtual_time trace restore occurred during an active event");
        }
        let event = u64::try_from(self.normalized.events.len()).unwrap_or(u64::MAX);
        self.cancel_clockevent_at(event)?;
        let Some((deadline_vns, interrupt_id)) = restored else {
            return Ok(());
        };
        let schedule_index = self.next_schedule_index;
        self.next_schedule_index = self
            .next_schedule_index
            .checked_add(1)
            .ok_or("virtual_time restored schedule index exhausted")?;
        self.schedule.push(ScheduledInterrupt {
            deadline_vns,
            schedule_index,
            armed_for_event: event,
            canceled_at_event: None,
            interrupt_id,
        });
        self.active_clockevent_schedule = Some(schedule_index);
        Ok(())
    }

    pub(crate) fn cancel_clockevent(&mut self) -> Result<(), &'static str> {
        let event = self.current_event_index()?;
        self.cancel_clockevent_at(event)
    }

    pub(crate) fn defer_clockevent(&mut self) -> Result<(), &'static str> {
        if self.pending.is_none() {
            return Ok(());
        }
        let event = self.current_event_index()?;
        let active = self
            .active_clockevent_schedule
            .ok_or("clockevent deferral has no active virtual_time schedule")?;
        let position = self
            .schedule
            .iter()
            .position(|scheduled| scheduled.schedule_index == active)
            .ok_or("deferred virtual_time clockevent schedule record is missing")?;
        let prior = self.schedule[position];
        if prior.canceled_at_event.is_some() {
            return Err("active virtual_time clockevent schedule was already canceled");
        }
        let armed_for_event = event
            .checked_add(1)
            .ok_or("virtual_time clockevent deferral event exhausted")?;
        let schedule_index = self.next_schedule_index;
        self.next_schedule_index = self
            .next_schedule_index
            .checked_add(1)
            .ok_or("virtual_time clockevent schedule index exhausted")?;
        self.schedule[position].canceled_at_event = Some(event);
        self.schedule.push(ScheduledInterrupt {
            deadline_vns: prior.deadline_vns,
            schedule_index,
            armed_for_event,
            canceled_at_event: None,
            interrupt_id: prior.interrupt_id,
        });
        self.active_clockevent_schedule = Some(schedule_index);
        Ok(())
    }

    fn cancel_clockevent_at(&mut self, event: u64) -> Result<(), &'static str> {
        let Some(schedule_index) = self.active_clockevent_schedule.take() else {
            return Ok(());
        };
        let scheduled = self
            .schedule
            .iter_mut()
            .find(|scheduled| scheduled.schedule_index == schedule_index)
            .ok_or("active virtual_time clockevent schedule record is missing")?;
        if scheduled.canceled_at_event.is_some() {
            return Err("active virtual_time clockevent schedule was already canceled");
        }
        scheduled.canceled_at_event = Some(event);
        Ok(())
    }

    pub(crate) fn deliver_clockevent(&mut self) -> Result<(), &'static str> {
        let schedule_index = self
            .active_clockevent_schedule
            .take()
            .ok_or("clockevent delivery has no active virtual_time schedule")?;
        let scheduled = self
            .schedule
            .iter()
            .find(|scheduled| scheduled.schedule_index == schedule_index)
            .ok_or("delivered virtual_time clockevent schedule record is missing")?;
        if scheduled.canceled_at_event.is_some() {
            return Err("canceled virtual_time clockevent was delivered");
        }
        self.current_interrupts.push((*scheduled).into());
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        vns_after: u64,
        state_hash: Option<[u8; 32]>,
    ) -> Result<(), &'static str> {
        let pending = self
            .pending
            .take()
            .ok_or("virtual_time trace finished with no active event")?;
        self.raw.push(pending.raw);
        self.normalized.events.push(NormalizedEvent {
            event_index: u64::try_from(self.normalized.events.len()).unwrap_or(u64::MAX),
            class: pending.class,
            payload_digest: digest_payload(pending.class, &pending.payload),
            vns_after,
            interrupts: std::mem::take(&mut self.current_interrupts),
            state_hash,
        });
        Ok(())
    }

    pub(crate) fn checkpoint_last(&mut self, state_hash: [u8; 32]) -> Result<(), &'static str> {
        let last = self
            .normalized
            .events
            .last_mut()
            .ok_or("cannot checkpoint an empty virtual_time trace")?;
        last.state_hash = Some(state_hash);
        Ok(())
    }

    pub(crate) fn checkpoint_at(
        &mut self,
        event_index: u64,
        state_hash: [u8; 32],
    ) -> Result<(), &'static str> {
        let index = usize::try_from(event_index)
            .map_err(|_| "deferred virtual_time checkpoint index does not fit usize")?;
        let event = self
            .normalized
            .events
            .get_mut(index)
            .ok_or("deferred virtual_time checkpoint event does not exist")?;
        if event.event_index != event_index {
            return Err("deferred virtual_time checkpoint ordinal mismatch");
        }
        if event.state_hash.is_some() {
            return Err("deferred virtual_time checkpoint would overwrite an existing hash");
        }
        event.state_hash = Some(state_hash);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualTimeCheckpoint {
    pub vns: u64,
    pub pending_interrupts: u64,
    pub event_index: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum VirtualTimeError {
    #[error(transparent)]
    Backend(#[from] vmm_backend::BackendError),
    #[error(transparent)]
    Clock(#[from] vtime::VtimeError),
    #[error("checkpoint interval must be at least one event")]
    ZeroCheckpointInterval,
    #[error("idle exit has no scheduled interrupt deadline")]
    IdleWithoutDeadline,
    #[error("{counter} index exhausted")]
    IndexExhausted { counter: &'static str },
    #[error("cannot run after a terminal event")]
    AlreadyTerminal,
    #[error("timer queue returned unknown token {token}")]
    UnknownTimerToken { token: u64 },
    #[error("exit classification failed: {0}")]
    Classification(String),
}

pub struct VirtualTimeRunLoop<B: Backend> {
    backend: B,
    timing: VirtualTimeTiming,
    clock: VClock,
    idle: IdlePlanner,
    timers: TimerQueue,
    pending: BTreeMap<TimerToken, ScheduledInterrupt>,
    schedule: Vec<ScheduledInterrupt>,
    raw: Vec<RawEvent>,
    normalized: NormalizedLog,
    checkpoint_every: u64,
    next_event_index: u64,
    next_schedule_index: u64,
    terminal: bool,
}

impl<B: Backend> VirtualTimeRunLoop<B> {
    pub fn new(
        backend: B,
        clock_config: VClockConfig,
        timing: VirtualTimeTiming,
        checkpoint_every: u64,
    ) -> Result<Self, VirtualTimeError> {
        if checkpoint_every == 0 {
            return Err(VirtualTimeError::ZeroCheckpointInterval);
        }
        Ok(Self {
            backend,
            timing,
            clock: VClock::new(clock_config)?,
            idle: IdlePlanner::new(),
            timers: TimerQueue::new(),
            pending: BTreeMap::new(),
            schedule: Vec::new(),
            raw: Vec::new(),
            normalized: NormalizedLog::default(),
            checkpoint_every,
            next_event_index: 0,
            next_schedule_index: 0,
            terminal: false,
        })
    }

    pub fn vns(&self) -> u64 {
        self.clock.vns()
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn raw_log(&self) -> &[RawEvent] {
        &self.raw
    }

    pub fn normalized_log(&self) -> &NormalizedLog {
        &self.normalized
    }

    pub fn schedule(&self) -> &[ScheduledInterrupt] {
        &self.schedule
    }

    pub fn schedule_interrupt(
        &mut self,
        deadline_vns: u64,
        interrupt_id: u32,
    ) -> Result<ScheduledInterrupt, VirtualTimeError> {
        let schedule_index = self.next_schedule_index;
        self.next_schedule_index =
            self.next_schedule_index
                .checked_add(1)
                .ok_or(VirtualTimeError::IndexExhausted {
                    counter: "schedule",
                })?;
        let scheduled = ScheduledInterrupt {
            deadline_vns,
            schedule_index,
            armed_for_event: self.next_event_index,
            canceled_at_event: None,
            interrupt_id,
        };
        let token = TimerToken(schedule_index);
        self.timers.schedule_oneshot(deadline_vns, token);
        self.pending.insert(token, scheduled);
        self.schedule.push(scheduled);
        Ok(scheduled)
    }

    pub fn run_backend_once<C, D, H>(
        &mut self,
        classify: C,
        mut deliver: D,
        hash: H,
    ) -> Result<&NormalizedEvent, VirtualTimeError>
    where
        C: FnOnce(&mut B, &vmm_backend::Exit<B::A>) -> Result<ClassifiedExit, VirtualTimeError>,
        D: FnMut(&mut B, InterruptDelivery) -> Result<(), VirtualTimeError>,
        H: FnOnce(&B, VirtualTimeCheckpoint) -> [u8; 32],
        B::A: std::fmt::Debug,
    {
        if self.terminal {
            return Err(VirtualTimeError::AlreadyTerminal);
        }
        let exit = self.backend.run()?;
        let reason = exit.reason();
        let backend_debug = format!("{exit:?}");
        let classified = classify(&mut self.backend, &exit)?;
        self.record(reason, backend_debug, classified, &mut deliver, hash)
    }

    fn record<D, H>(
        &mut self,
        reason: ExitReason,
        backend_debug: String,
        classified: ClassifiedExit,
        deliver: &mut D,
        hash: H,
    ) -> Result<&NormalizedEvent, VirtualTimeError>
    where
        D: FnMut(&mut B, InterruptDelivery) -> Result<(), VirtualTimeError>,
        H: FnOnce(&B, VirtualTimeCheckpoint) -> [u8; 32],
    {
        let event_index = self.next_event_index;
        self.next_event_index = self
            .next_event_index
            .checked_add(1)
            .ok_or(VirtualTimeError::IndexExhausted { counter: "event" })?;

        let now = self.vns();
        let advance_vns = match classified.advance {
            AdvanceRule::Doorbell(duration_vns) => duration_vns,
            AdvanceRule::DeviceMmio(class) => self.timing.mmio_vns(class),
            AdvanceRule::TimeRead => self.timing.trapped_time_read_vns,
            AdvanceRule::ArchitecturalControl => self.timing.architectural_control_vns,
            AdvanceRule::Idle => {
                let (deadline, _) = self
                    .timers
                    .peek_next()
                    .ok_or(VirtualTimeError::IdleWithoutDeadline)?;
                self.idle.plan(now, deadline).advance_vns
            }
            AdvanceRule::None => 0,
        };
        self.clock.advance(advance_vns);
        let vns_after = self.vns();

        let mut interrupts = Vec::new();
        for (_, token) in self.timers.pop_due(vns_after) {
            let scheduled = self
                .pending
                .remove(&token)
                .ok_or(VirtualTimeError::UnknownTimerToken { token: token.0 })?;
            let delivery = scheduled.into();
            deliver(&mut self.backend, delivery)?;
            interrupts.push(delivery);
        }

        let checkpoint_due = (event_index + 1).is_multiple_of(self.checkpoint_every);
        let checkpoint = VirtualTimeCheckpoint {
            vns: vns_after,
            pending_interrupts: u64::try_from(self.pending.len()).unwrap_or(u64::MAX),
            event_index,
        };
        let state_hash =
            (checkpoint_due || classified.terminal).then(|| hash(&self.backend, checkpoint));

        self.raw.push(RawEvent {
            event_index,
            portable_event_index: Some(event_index),
            reason,
            backend_debug,
        });
        self.normalized.events.push(NormalizedEvent {
            event_index,
            class: classified.class,
            payload_digest: digest_payload(classified.class, &classified.payload),
            vns_after,
            interrupts,
            state_hash,
        });
        self.terminal = classified.terminal;
        self.normalized
            .events
            .last()
            .ok_or(VirtualTimeError::Classification(
                "normalized event append produced no event".to_string(),
            ))
    }
}

pub(crate) fn digest_payload(class: NormalizedEventClass, payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"consonance.prescriptive-event.v1\0");
    hasher.update([class.tag()]);
    hasher.update(
        u64::try_from(payload.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(payload);
    hasher.finalize().into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogField {
    Length,
    EventIndex,
    Class,
    PayloadDigest,
    VnsAfter,
    Interrupts,
    StateHash,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("normalized logs diverged at event {event_index} in {field:?}")]
pub struct LogDivergence {
    pub event_index: u64,
    pub field: LogField,
}

pub fn compare_normalized_logs(
    left: &NormalizedLog,
    right: &NormalizedLog,
) -> Result<(), LogDivergence> {
    for (offset, (a, b)) in left.events.iter().zip(&right.events).enumerate() {
        let event_index = u64::try_from(offset).unwrap_or(u64::MAX);
        let field = if a.event_index != b.event_index {
            Some(LogField::EventIndex)
        } else if a.class != b.class {
            Some(LogField::Class)
        } else if a.payload_digest != b.payload_digest {
            Some(LogField::PayloadDigest)
        } else if a.vns_after != b.vns_after {
            Some(LogField::VnsAfter)
        } else if a.interrupts != b.interrupts {
            Some(LogField::Interrupts)
        } else if a.state_hash != b.state_hash {
            Some(LogField::StateHash)
        } else {
            None
        };
        if let Some(field) = field {
            return Err(LogDivergence { event_index, field });
        }
    }
    if left.events.len() != right.events.len() {
        return Err(LogDivergence {
            event_index: u64::try_from(left.events.len().min(right.events.len()))
                .unwrap_or(u64::MAX),
            field: LogField::Length,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlacementViolation {
    #[error("normalized log event index {actual} appeared at position {position}")]
    BadEventIndex { position: u64, actual: u64 },
    #[error("normalized log V-time moved backwards at event {event_index}: {before} -> {after}")]
    VtimeRegressed {
        event_index: u64,
        before: u64,
        after: u64,
    },
    #[error("interrupt placement differs at event {event_index}")]
    WrongDelivery {
        event_index: u64,
        expected: Vec<InterruptDelivery>,
        actual: Vec<InterruptDelivery>,
    },
    #[error("scheduled interrupt {schedule_index} at {deadline_vns} vns was not delivered")]
    Undelivered {
        schedule_index: u64,
        deadline_vns: u64,
    },
}

pub fn check_delivery_placement(
    schedule: &[ScheduledInterrupt],
    log: &NormalizedLog,
) -> Result<(), PlacementViolation> {
    let mut ordered = schedule.to_vec();
    ordered.sort_by_key(|s| (s.deadline_vns, s.schedule_index));
    let mut delivered = vec![false; ordered.len()];
    let mut previous_vns = 0u64;

    for (position, event) in log.events.iter().enumerate() {
        let position_u64 = u64::try_from(position).unwrap_or(u64::MAX);
        if event.event_index != position_u64 {
            return Err(PlacementViolation::BadEventIndex {
                position: position_u64,
                actual: event.event_index,
            });
        }
        if event.vns_after < previous_vns {
            return Err(PlacementViolation::VtimeRegressed {
                event_index: event.event_index,
                before: previous_vns,
                after: event.vns_after,
            });
        }
        previous_vns = event.vns_after;

        let due_indices: Vec<_> = ordered
            .iter()
            .enumerate()
            .filter_map(|(index, scheduled)| {
                (!delivered[index]
                    && scheduled.armed_for_event <= event.event_index
                    && scheduled
                        .canceled_at_event
                        .is_none_or(|canceled| canceled > event.event_index)
                    && scheduled.deadline_vns <= event.vns_after)
                    .then_some(index)
            })
            .collect();
        let expected: Vec<_> = due_indices
            .iter()
            .map(|index| InterruptDelivery::from(ordered[*index]))
            .collect();
        if event.interrupts != expected {
            return Err(PlacementViolation::WrongDelivery {
                event_index: event.event_index,
                expected,
                actual: event.interrupts.clone(),
            });
        }
        for index in due_indices {
            delivered[index] = true;
        }
    }

    if let Some(last) = log.events.last()
        && let Some((_, missing)) = ordered.iter().enumerate().find(|(index, scheduled)| {
            !delivered[*index]
                && scheduled.canceled_at_event.is_none()
                && scheduled.armed_for_event <= last.event_index
                && scheduled.deadline_vns <= last.vns_after
        })
    {
        return Err(PlacementViolation::Undelivered {
            schedule_index: missing.schedule_index,
            deadline_vns: missing.deadline_vns,
        });
    }
    Ok(())
}

#[cfg(test)]
mod live_trace_tests {
    use super::*;

    #[test]
    fn calibration_labels_are_the_frozen_fit_script_vocabulary() {
        for (class, label) in [
            (NormalizedEventClass::Doorbell, "doorbell"),
            (
                NormalizedEventClass::DeviceMmio(DeviceClass::InterruptController),
                "ic_mmio",
            ),
            (
                NormalizedEventClass::DeviceMmio(DeviceClass::Serial),
                "serial_mmio",
            ),
            (
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                "pv_mmio",
            ),
            (NormalizedEventClass::TimeRead, "time_read"),
            (NormalizedEventClass::ArchitecturalControl, "arch_control"),
            (NormalizedEventClass::Idle, "idle"),
            (NormalizedEventClass::Terminal, "terminal"),
        ] {
            assert_eq!(class.label(), label);
        }
    }

    #[test]
    fn frozen_v1_empty_serial_payload_digest_is_stable() {
        assert_eq!(
            digest_payload(NormalizedEventClass::DeviceMmio(DeviceClass::Serial), &[],),
            [
                0x63, 0xa3, 0x23, 0x2e, 0xe8, 0xd6, 0x98, 0xd8, 0xfd, 0xd4, 0xd5, 0x38, 0x73, 0xef,
                0x9a, 0xeb, 0x02, 0x0f, 0x90, 0x52, 0x1a, 0xa8, 0x53, 0xb4, 0x8f, 0xb4, 0x36, 0x93,
                0xd9, 0xf0, 0x2d, 0x49,
            ],
        );
    }

    #[test]
    fn raw_only_exit_does_not_consume_a_portable_ordinal() {
        let mut trace = LiveVirtualTimeTrace::default();
        trace
            .record_raw_only(ExitReason::Mmio, "private GIC MMIO".to_string())
            .unwrap();
        trace
            .begin(
                ExitReason::Mmio,
                "portable serial MMIO".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Serial),
                vec![1],
            )
            .unwrap();

        assert_eq!(trace.current_event_index().unwrap(), 0);
        trace.finish(2, None).unwrap();
        assert_eq!(trace.raw.len(), 2);
        assert_eq!(trace.raw[0].event_index, 0);
        assert_eq!(trace.raw[0].portable_event_index, None);
        assert_eq!(trace.raw[1].event_index, 1);
        assert_eq!(trace.raw[1].portable_event_index, Some(0));
        assert_eq!(trace.normalized.events.len(), 1);
        assert_eq!(trace.normalized.events[0].event_index, 0);
        assert_eq!(trace.raw_log().len(), 2);
        assert_ne!(trace.normalized_digest(), [0; 32]);
        assert_ne!(trace.normalized_digest(), [1; 32]);
        trace.checkpoint_last([9; 32]).unwrap();
        assert_eq!(trace.normalized.events[0].state_hash, Some([9; 32]));

        let mut empty = LiveVirtualTimeTrace::default();
        assert!(empty.checkpoint_last([1; 32]).is_err());
    }

    #[test]
    fn replacement_and_disarm_are_recorded_as_cancellations() {
        let mut trace = LiveVirtualTimeTrace::default();
        trace
            .begin(
                ExitReason::Mmio,
                "Mmio".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![1],
            )
            .unwrap();
        trace.schedule_clockevent(10, 20).unwrap();
        trace.schedule_clockevent(20, 20).unwrap();
        trace.finish(1, None).unwrap();

        trace
            .begin(
                ExitReason::Mmio,
                "Mmio".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![2],
            )
            .unwrap();
        trace.cancel_clockevent().unwrap();
        trace.finish(2, None).unwrap();

        assert_eq!(trace.schedule.len(), 2);
        assert_eq!(trace.schedule[0].canceled_at_event, Some(0));
        assert_eq!(trace.schedule[1].canceled_at_event, Some(1));
        check_delivery_placement(trace.schedule(), trace.normalized_log()).unwrap();
    }

    #[test]
    fn live_delivery_is_bound_to_the_active_schedule() {
        let mut trace = LiveVirtualTimeTrace::default();
        trace
            .begin(
                ExitReason::Mmio,
                "Mmio".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![3],
            )
            .unwrap();
        trace.schedule_clockevent(4, 20).unwrap();
        trace.finish(1, None).unwrap();
        trace
            .begin(
                ExitReason::Mmio,
                "Mmio".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Serial),
                vec![4],
            )
            .unwrap();
        trace.deliver_clockevent().unwrap();
        trace.finish(4, None).unwrap();

        check_delivery_placement(trace.schedule(), trace.normalized_log()).unwrap();
        assert_eq!(trace.normalized.events[1].interrupts.len(), 1);
    }

    #[test]
    fn restored_timer_seeds_the_new_trace_segment_before_delivery() {
        let mut trace = LiveVirtualTimeTrace::default();
        trace.restore_clockevent_schedule(Some((4, 20))).unwrap();
        trace
            .begin(
                ExitReason::Mmio,
                "first restored exit".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Serial),
                vec![5],
            )
            .unwrap();
        trace.deliver_clockevent().unwrap();
        trace.finish(4, None).unwrap();

        check_delivery_placement(trace.schedule(), trace.normalized_log()).unwrap();
        assert_eq!(trace.schedule.len(), 1);
        assert_eq!(trace.schedule[0].armed_for_event, 0);
        assert_eq!(trace.normalized.events[0].interrupts.len(), 1);
    }

    #[test]
    fn masked_due_epochs_are_explicit_and_late_delivery_still_fails() {
        let mut trace = LiveVirtualTimeTrace::default();
        trace
            .begin(
                ExitReason::Mmio,
                "program".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![1],
            )
            .unwrap();
        trace.schedule_clockevent(4, 27).unwrap();
        trace.finish(1, None).unwrap();

        trace
            .begin(
                ExitReason::Mmio,
                "masked".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![2],
            )
            .unwrap();
        trace.defer_clockevent().unwrap();
        trace.finish(4, None).unwrap();

        trace
            .begin(
                ExitReason::Mmio,
                "unmask-fence".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![3],
            )
            .unwrap();
        trace.deliver_clockevent().unwrap();
        trace.finish(5, None).unwrap();
        trace
            .begin(
                ExitReason::Mmio,
                "next".to_string(),
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
                vec![4],
            )
            .unwrap();
        trace.finish(6, None).unwrap();

        assert_eq!(trace.schedule.len(), 2);
        assert_eq!(trace.schedule[0].canceled_at_event, Some(1));
        assert_eq!(trace.schedule[1].armed_for_event, 2);
        assert_eq!(trace.schedule[1].deadline_vns, 4);
        check_delivery_placement(trace.schedule(), trace.normalized_log()).unwrap();

        let mut early = trace.normalized_log().clone();
        let delivery = early.events[2].interrupts.remove(0);
        early.events[1].interrupts.push(delivery);
        assert!(matches!(
            check_delivery_placement(trace.schedule(), &early),
            Err(PlacementViolation::WrongDelivery { event_index: 1, .. })
        ));

        let mut late = trace.normalized_log().clone();
        let delivery = late.events[2].interrupts.remove(0);
        late.events[3].interrupts.push(delivery);
        assert!(matches!(
            check_delivery_placement(trace.schedule(), &late),
            Err(PlacementViolation::WrongDelivery { event_index: 2, .. })
        ));
    }

    #[test]
    fn finite_prefix_permits_only_deadlines_beyond_its_final_vtime() {
        let event = NormalizedEvent {
            event_index: 0,
            class: NormalizedEventClass::Terminal,
            payload_digest: [0; 32],
            vns_after: 9,
            interrupts: Vec::new(),
            state_hash: None,
        };
        let schedule = [ScheduledInterrupt {
            deadline_vns: 10,
            schedule_index: 0,
            armed_for_event: 0,
            canceled_at_event: None,
            interrupt_id: 20,
        }];
        let prefix = NormalizedLog {
            events: vec![event.clone()],
        };
        check_delivery_placement(&schedule, &prefix).unwrap();

        let due = NormalizedLog {
            events: vec![NormalizedEvent {
                vns_after: 10,
                ..event
            }],
        };
        assert!(matches!(
            check_delivery_placement(&schedule, &due),
            Err(PlacementViolation::WrongDelivery { event_index: 0, .. })
        ));

        let future_epoch = [ScheduledInterrupt {
            deadline_vns: 0,
            armed_for_event: 1,
            ..schedule[0]
        }];
        assert_eq!(check_delivery_placement(&future_epoch, &prefix), Ok(()));
    }
}
