// SPDX-License-Identifier: AGPL-3.0-or-later
//! VirtualTime V-time advancement, normalized logging, and delivery checking.
//!
//! In this mode the run loop assigns V-time at VM exits. The [`vtime::VClock`] is
//! always queried at work zero; every increment is applied to its `vns_base`
//! through [`vtime::VClock::advance_idle`]. A deadline is raised at the first exit
//! whose post-advance V-time reaches it.  This module is architecture-neutral:
//! a vendor dispatcher classifies the backend's exit and supplies only the
//! normalized event payload needed by the clock contract.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use vmm_backend::{Backend, ExitReason};
use vtime::{IdlePlanner, TimerQueue, TimerToken, VClock, VClockConfig};

/// Placeholder duration for an interrupt-controller MMIO exit.
///
/// M1 replaces this clearly non-normative value when the arm64 determinism
/// contract gains its measured per-device constants.
pub const PLACEHOLDER_INTERRUPT_CONTROLLER_MMIO_VNS: u64 = 1;

/// Placeholder duration for a serial-device MMIO exit.
pub const PLACEHOLDER_SERIAL_MMIO_VNS: u64 = 1;

/// Placeholder duration for a paravirtual-device MMIO exit.
pub const PLACEHOLDER_PARAVIRTUAL_DEVICE_MMIO_VNS: u64 = 1;

/// Placeholder duration for a trapped guest time read.
pub const PLACEHOLDER_TRAPPED_TIME_READ_VNS: u64 = 1;

/// Placeholder duration for a trapped architectural-control access.
pub const PLACEHOLDER_ARCHITECTURAL_CONTROL_VNS: u64 = 1;

/// Device classes whose contract constants advance virtual_time V-time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceClass {
    /// Interrupt-controller distributor, redistributor, or CPU-interface access.
    InterruptController,
    /// Guest serial device access.
    Serial,
    /// A paravirtual device other than the doorbell transport itself.
    Paravirtual,
}

/// The per-exit constants used by virtual_time advancement.
///
/// The default contains deliberately named placeholders.  A production
/// composition must pass the normative values from its determinism contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualTimeTiming {
    /// V-ns assigned to interrupt-controller MMIO.
    pub interrupt_controller_mmio_vns: u64,
    /// V-ns assigned to serial MMIO.
    pub serial_mmio_vns: u64,
    /// V-ns assigned to paravirtual-device MMIO.
    pub paravirtual_device_mmio_vns: u64,
    /// V-ns assigned to a trapped time read.
    pub trapped_time_read_vns: u64,
    /// V-ns assigned to a trapped deterministic architectural control.
    pub architectural_control_vns: u64,
}

impl Default for VirtualTimeTiming {
    fn default() -> Self {
        Self {
            interrupt_controller_mmio_vns: PLACEHOLDER_INTERRUPT_CONTROLLER_MMIO_VNS,
            serial_mmio_vns: PLACEHOLDER_SERIAL_MMIO_VNS,
            paravirtual_device_mmio_vns: PLACEHOLDER_PARAVIRTUAL_DEVICE_MMIO_VNS,
            trapped_time_read_vns: PLACEHOLDER_TRAPPED_TIME_READ_VNS,
            architectural_control_vns: PLACEHOLDER_ARCHITECTURAL_CONTROL_VNS,
        }
    }
}

impl VirtualTimeTiming {
    fn mmio_vns(self, class: DeviceClass) -> u64 {
        match class {
            DeviceClass::InterruptController => self.interrupt_controller_mmio_vns,
            DeviceClass::Serial => self.serial_mmio_vns,
            DeviceClass::Paravirtual => self.paravirtual_device_mmio_vns,
        }
    }
}

/// The guest-visible event classes carried by a normalized log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizedEventClass {
    /// SDK yield, input fetch, or paravirtual tick doorbell.
    Doorbell,
    /// Device MMIO access.
    DeviceMmio(DeviceClass),
    /// Counter-shaped sysreg read or pvclock refresh.
    TimeRead,
    /// Deterministic architectural-control trap outside a device model.
    ArchitecturalControl,
    /// Guest WFI/HLT idle exit.
    Idle,
    /// A terminal exit, which advances by zero.
    Terminal,
}

impl NormalizedEventClass {
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

/// One backend exit after vendor classification.
///
/// Constructors bind each normalized class to its only legal advancement rule,
/// so a caller cannot label an MMIO exit while applying a doorbell duration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedExit {
    class: NormalizedEventClass,
    payload: Vec<u8>,
    advance: AdvanceRule,
    terminal: bool,
}

impl ClassifiedExit {
    /// A doorbell whose request declares or implies `duration_vns`.
    pub fn doorbell(payload: Vec<u8>, duration_vns: u64) -> Self {
        Self {
            class: NormalizedEventClass::Doorbell,
            payload,
            advance: AdvanceRule::Doorbell(duration_vns),
            terminal: false,
        }
    }

    /// A device MMIO exit, advanced by the constant for `class`.
    pub fn device_mmio(class: DeviceClass, payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::DeviceMmio(class),
            payload,
            advance: AdvanceRule::DeviceMmio(class),
            terminal: false,
        }
    }

    /// A trapped guest time read.
    pub fn time_read(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::TimeRead,
            payload,
            advance: AdvanceRule::TimeRead,
            terminal: false,
        }
    }

    /// A deterministic architectural-control trap.
    pub fn architectural_control(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::ArchitecturalControl,
            payload,
            advance: AdvanceRule::ArchitecturalControl,
            terminal: false,
        }
    }

    /// A WFI/HLT exit.  The clock jumps to the earliest scheduled deadline.
    pub fn idle(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::Idle,
            payload,
            advance: AdvanceRule::Idle,
            terminal: false,
        }
    }

    /// A terminal exit.  It advances by zero but still delivers anything that
    /// was already due, because it is an exit boundary.
    pub fn terminal(payload: Vec<u8>) -> Self {
        Self {
            class: NormalizedEventClass::Terminal,
            payload,
            advance: AdvanceRule::None,
            terminal: true,
        }
    }
}

/// One interrupt deadline recorded when it is scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledInterrupt {
    /// Absolute V-time deadline.
    pub deadline_vns: u64,
    /// FIFO insertion sequence, unique within a run.
    pub schedule_index: u64,
    /// First event index at which this deadline exists and may be delivered.
    ///
    /// A guest may arm an already-due timer between exits.  Recording this
    /// boundary prevents the independent checker from requiring delivery at an
    /// earlier event, before the timer existed.
    pub armed_for_event: u64,
    /// Exit that canceled/replaced this deadline before post-exit delivery, or
    /// closed one delivery-eligibility epoch while the guest IRQ mask was set.
    /// `None` means this epoch remains live until delivered.
    pub canceled_at_event: Option<u64>,
    /// Vendor-neutral wire interrupt identity.
    pub interrupt_id: u32,
}

/// One deadline raised into the interrupt fabric at an exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptDelivery {
    /// The deadline that became due.
    pub deadline_vns: u64,
    /// The deadline's FIFO insertion sequence.
    pub schedule_index: u64,
    /// Vendor-neutral wire interrupt identity.
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

/// Backend-local debugging record.  Raw logs are never compared across substrates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// Zero-based exit index.
    pub event_index: u64,
    /// Payload-free backend exit reason.
    pub reason: ExitReason,
    /// Backend's debug rendering of the complete exit.
    pub backend_debug: String,
}

/// Guest-visible record for one VM exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvent {
    /// Zero-based exit index.
    pub event_index: u64,
    /// Normalized event class.
    pub class: NormalizedEventClass,
    /// Domain-separated digest of the class and complete guest-visible payload.
    pub payload_digest: [u8; 32],
    /// V-time after this exit's advancement.
    pub vns_after: u64,
    /// Interrupts raised at this exit, in deadline/FIFO order.
    pub interrupts: Vec<InterruptDelivery>,
    /// Full-state hash at the checkpoint interval and at terminal.
    pub state_hash: Option<[u8; 32]>,
}

/// Complete normalized run log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizedLog {
    /// Ordered exit records.
    pub events: Vec<NormalizedEvent>,
}

#[derive(Clone, Debug)]
struct PendingLiveEvent {
    raw: RawEvent,
    class: NormalizedEventClass,
    payload: Vec<u8>,
}

/// Full virtual_time trace captured by the production VMM run loop.
///
/// The trace is host-side evidence only: it is excluded from snapshots and
/// state hashes. It retains every raw exit for local diagnosis, every
/// normalized exit for cross-run comparison, and the immutable deadline
/// schedule (including cancellation events) for the independent placement
/// checker.
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
    /// Backend-local raw exits. Never compare this across substrates.
    pub fn raw_log(&self) -> &[RawEvent] {
        &self.raw
    }

    /// Complete guest-visible normalized exit log.
    pub fn normalized_log(&self) -> &NormalizedLog {
        &self.normalized
    }

    /// Immutable deadline schedule consumed by [`check_delivery_placement`].
    pub fn schedule(&self) -> &[ScheduledInterrupt] {
        &self.schedule
    }

    /// SHA-256 of the complete normalized log and deadline schedule in a fixed,
    /// domain-separated little-endian encoding.
    pub fn normalized_digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"consonance.live-virtual_time-log.v1\0");
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

    /// Retain a substrate-private exit in the raw diagnostic stream without
    /// assigning portable V-time or a normalized event ordinal.
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

    pub(crate) fn cancel_clockevent(&mut self) -> Result<(), &'static str> {
        let event = self.current_event_index()?;
        self.cancel_clockevent_at(event)
    }

    /// Close the active delivery-eligibility epoch at this masked exit and
    /// carry the same deadline into the next event. The immutable schedule
    /// therefore gives the independent placement checker explicit evidence
    /// that delivery was not legal at this boundary, without changing the
    /// frozen normalized-event surface.
    pub(crate) fn defer_clockevent(&mut self) -> Result<(), &'static str> {
        // Substrate-private raw exits do not consume a portable event ordinal,
        // so they cannot create an eligibility epoch in the normalized schedule.
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
}

/// State supplied to the full-state checkpoint callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualTimeCheckpoint {
    /// Post-advance V-time; the underlying clock is queried at work zero.
    pub vns: u64,
    /// Number of deadlines not yet delivered.
    pub pending_interrupts: u64,
    /// Current event index.
    pub event_index: u64,
}

/// Failure from the virtual_time run loop.
#[derive(Debug, thiserror::Error)]
pub enum VirtualTimeError {
    /// Backend operation failed.
    #[error(transparent)]
    Backend(#[from] vmm_backend::BackendError),
    /// Clock configuration failed.
    #[error(transparent)]
    Clock(#[from] vtime::VtimeError),
    /// A zero checkpoint interval would make the oracle vacuous.
    #[error("checkpoint interval must be at least one event")]
    ZeroCheckpointInterval,
    /// WFI cannot make progress without a scheduled wakeup.
    #[error("idle exit has no scheduled interrupt deadline")]
    IdleWithoutDeadline,
    /// No more unique event or schedule indices can be represented.
    #[error("{counter} index exhausted")]
    IndexExhausted {
        /// Name of the exhausted counter.
        counter: &'static str,
    },
    /// The caller tried to continue after a terminal event.
    #[error("cannot run after a terminal event")]
    AlreadyTerminal,
    /// Internal timer metadata and `TimerQueue` disagreed.
    #[error("timer queue returned unknown token {token}")]
    UnknownTimerToken {
        /// Token returned by the queue.
        token: u64,
    },
    /// Vendor classification or completion failed closed.
    #[error("exit classification failed: {0}")]
    Classification(String),
}

/// Run-loop state for virtual_time V-time.
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
    /// Construct a run loop over an already configured backend.
    ///
    /// `clock_config.vns_base` is the initial V-time.  Work remains zero for
    /// the lifetime of this loop.
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

    /// Current V-time.  This is always `VClock::vns(0)`.
    pub fn vns(&self) -> u64 {
        self.clock.vns()
    }

    /// Immutable access to the backend, primarily for reports and tests.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend-local raw exit log.
    pub fn raw_log(&self) -> &[RawEvent] {
        &self.raw
    }

    /// The guest-visible normalized log.
    pub fn normalized_log(&self) -> &NormalizedLog {
        &self.normalized
    }

    /// The immutable deadline schedule consumed by the placement checker.
    pub fn schedule(&self) -> &[ScheduledInterrupt] {
        &self.schedule
    }

    /// Schedule a one-shot interrupt and return its immutable schedule record.
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

    /// Run the backend to its next exit, classify/service that exit, advance
    /// V-time, raise every due interrupt, and append both logs.
    ///
    /// `classify` is the vendor dispatch seam. It may complete a read-style
    /// backend exit before returning the normalized classification. `deliver`
    /// raises every due identity into the userspace interrupt fabric before the
    /// next entry. `hash` must return the canonical hash of all observable
    /// state; it is called at every configured checkpoint and unconditionally
    /// for a terminal event.
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
    hasher.update(b"consonance.virtual_time-event.v1\0");
    hasher.update([class.tag()]);
    hasher.update(
        u64::try_from(payload.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(payload);
    hasher.finalize().into()
}

/// Which normalized-log field first diverged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogField {
    /// One log ended before the other.
    Length,
    /// Event indices differ.
    EventIndex,
    /// Event classes differ.
    Class,
    /// Payload digests differ.
    PayloadDigest,
    /// Post-advance V-time differs.
    VnsAfter,
    /// Interrupt placement or order differs.
    Interrupts,
    /// Full-state checkpoint hashes differ.
    StateHash,
}

/// Exact first divergence between normalized logs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("normalized logs diverged at event {event_index} in {field:?}")]
pub struct LogDivergence {
    /// Exact first divergent event (or the shorter length for a length mismatch).
    pub event_index: u64,
    /// First field that differs at that event.
    pub field: LogField,
}

/// Compare complete normalized logs and report their exact first divergence.
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

/// A delivery-placement contract violation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlacementViolation {
    /// Log event indices are not a contiguous sequence from zero.
    #[error("normalized log event index {actual} appeared at position {position}")]
    BadEventIndex {
        /// Vector position.
        position: u64,
        /// Recorded index.
        actual: u64,
    },
    /// V-time moved backwards.
    #[error("normalized log V-time moved backwards at event {event_index}: {before} -> {after}")]
    VtimeRegressed {
        /// Event carrying the regression.
        event_index: u64,
        /// Previous post-advance V-time.
        before: u64,
        /// Regressed post-advance V-time.
        after: u64,
    },
    /// The set/order delivered at an event differs from the independent schedule oracle.
    #[error("interrupt placement differs at event {event_index}")]
    WrongDelivery {
        /// Exact divergent event.
        event_index: u64,
        /// Deadlines that must be delivered here.
        expected: Vec<InterruptDelivery>,
        /// Deadlines the run actually delivered here.
        actual: Vec<InterruptDelivery>,
    },
    /// A scheduled deadline was never reached/delivered by the complete log.
    #[error("scheduled interrupt {schedule_index} at {deadline_vns} vns was not delivered")]
    Undelivered {
        /// FIFO schedule identity.
        schedule_index: u64,
        /// Deadline left outstanding.
        deadline_vns: u64,
    },
}

/// Independently verify the §2.1 delivery contract against one complete log.
///
/// This deliberately does not reuse [`TimerQueue`].  It sorts the immutable
/// effective schedule by `(deadline, insertion sequence)` and derives the
/// expected deliveries from only the log's post-advance V-time values. Masked
/// exits appear as closed schedule epochs, so the checker sees exactly where
/// delivery was ineligible without reading backend state. Therefore a run loop
/// that is consistently one exit late cannot make this oracle agree with itself.
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

    // A milestone log is a finite prefix ending at its observation marker
    // (`/init` for M1), not necessarily a terminal VM state. A still-armed
    // deadline strictly beyond the prefix's final V-time is not late and must
    // remain in the schedule so the state/checkpoint is honest. Reject only a
    // live deadline that had become eligible within the observed prefix.
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
        assert_eq!(trace.raw[1].event_index, 1);
        assert_eq!(trace.normalized.events.len(), 1);
        assert_eq!(trace.normalized.events[0].event_index, 0);
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

        // Deadline reached, but the architectural IRQ mask is set. Production
        // records this eligibility-epoch boundary before returning without a
        // delivery.
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
    }
}
