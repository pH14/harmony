// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prescriptive V-time advancement, normalized logging, and delivery checking.
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

/// Device classes whose contract constants advance prescriptive V-time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceClass {
    /// Interrupt-controller distributor, redistributor, or CPU-interface access.
    InterruptController,
    /// Guest serial device access.
    Serial,
    /// A paravirtual device other than the doorbell transport itself.
    Paravirtual,
}

/// The per-exit constants used by prescriptive advancement.
///
/// The default contains deliberately named placeholders.  A production
/// composition must pass the normative values from its determinism contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrescriptiveTiming {
    /// V-ns assigned to interrupt-controller MMIO.
    pub interrupt_controller_mmio_vns: u64,
    /// V-ns assigned to serial MMIO.
    pub serial_mmio_vns: u64,
    /// V-ns assigned to paravirtual-device MMIO.
    pub paravirtual_device_mmio_vns: u64,
    /// V-ns assigned to a trapped time read.
    pub trapped_time_read_vns: u64,
}

impl Default for PrescriptiveTiming {
    fn default() -> Self {
        Self {
            interrupt_controller_mmio_vns: PLACEHOLDER_INTERRUPT_CONTROLLER_MMIO_VNS,
            serial_mmio_vns: PLACEHOLDER_SERIAL_MMIO_VNS,
            paravirtual_device_mmio_vns: PLACEHOLDER_PARAVIRTUAL_DEVICE_MMIO_VNS,
            trapped_time_read_vns: PLACEHOLDER_TRAPPED_TIME_READ_VNS,
        }
    }
}

impl PrescriptiveTiming {
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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdvanceRule {
    Doorbell(u64),
    DeviceMmio(DeviceClass),
    TimeRead,
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

/// State supplied to the full-state checkpoint callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrescriptiveCheckpoint {
    /// Post-advance V-time; the underlying clock is queried at work zero.
    pub vns: u64,
    /// Number of deadlines not yet delivered.
    pub pending_interrupts: u64,
    /// Current event index.
    pub event_index: u64,
}

/// Failure from the prescriptive run loop.
#[derive(Debug, thiserror::Error)]
pub enum PrescriptiveError {
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

/// Run-loop state for prescriptive V-time.
pub struct PrescriptiveRunLoop<B: Backend> {
    backend: B,
    timing: PrescriptiveTiming,
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

impl<B: Backend> PrescriptiveRunLoop<B> {
    /// Construct a run loop over an already configured backend.
    ///
    /// `clock_config.vns_base` is the initial V-time.  Work remains zero for
    /// the lifetime of this loop.
    pub fn new(
        backend: B,
        clock_config: VClockConfig,
        timing: PrescriptiveTiming,
        checkpoint_every: u64,
    ) -> Result<Self, PrescriptiveError> {
        if checkpoint_every == 0 {
            return Err(PrescriptiveError::ZeroCheckpointInterval);
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
        self.clock.vns(0)
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
    ) -> Result<ScheduledInterrupt, PrescriptiveError> {
        let schedule_index = self.next_schedule_index;
        self.next_schedule_index =
            self.next_schedule_index
                .checked_add(1)
                .ok_or(PrescriptiveError::IndexExhausted {
                    counter: "schedule",
                })?;
        let scheduled = ScheduledInterrupt {
            deadline_vns,
            schedule_index,
            armed_for_event: self.next_event_index,
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
    ) -> Result<&NormalizedEvent, PrescriptiveError>
    where
        C: FnOnce(&mut B, &vmm_backend::Exit<B::A>) -> Result<ClassifiedExit, PrescriptiveError>,
        D: FnMut(&mut B, InterruptDelivery) -> Result<(), PrescriptiveError>,
        H: FnOnce(&B, PrescriptiveCheckpoint) -> [u8; 32],
        B::A: std::fmt::Debug,
    {
        if self.terminal {
            return Err(PrescriptiveError::AlreadyTerminal);
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
    ) -> Result<&NormalizedEvent, PrescriptiveError>
    where
        D: FnMut(&mut B, InterruptDelivery) -> Result<(), PrescriptiveError>,
        H: FnOnce(&B, PrescriptiveCheckpoint) -> [u8; 32],
    {
        let event_index = self.next_event_index;
        self.next_event_index = self
            .next_event_index
            .checked_add(1)
            .ok_or(PrescriptiveError::IndexExhausted { counter: "event" })?;

        let now = self.vns();
        let advance_vns = match classified.advance {
            AdvanceRule::Doorbell(duration_vns) => duration_vns,
            AdvanceRule::DeviceMmio(class) => self.timing.mmio_vns(class),
            AdvanceRule::TimeRead => self.timing.trapped_time_read_vns,
            AdvanceRule::Idle => {
                let (deadline, _) = self
                    .timers
                    .peek_next()
                    .ok_or(PrescriptiveError::IdleWithoutDeadline)?;
                self.idle.plan(now, deadline).advance_vns
            }
            AdvanceRule::None => 0,
        };
        self.clock.advance_idle(advance_vns);
        let vns_after = self.vns();

        let mut interrupts = Vec::new();
        for (_, token) in self.timers.pop_due(vns_after) {
            let scheduled = self
                .pending
                .remove(&token)
                .ok_or(PrescriptiveError::UnknownTimerToken { token: token.0 })?;
            let delivery = scheduled.into();
            deliver(&mut self.backend, delivery)?;
            interrupts.push(delivery);
        }

        let checkpoint_due = (event_index + 1).is_multiple_of(self.checkpoint_every);
        let checkpoint = PrescriptiveCheckpoint {
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
            .ok_or(PrescriptiveError::Classification(
                "normalized event append produced no event".to_string(),
            ))
    }
}

fn digest_payload(class: NormalizedEventClass, payload: &[u8]) -> [u8; 32] {
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
/// schedule by `(deadline, insertion sequence)` and derives the expected
/// deliveries from only the log's post-advance V-time values.  Therefore a run
/// loop that is consistently one exit late cannot make this oracle agree with
/// itself.
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
        if position > 0 && event.vns_after < previous_vns {
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

    if let Some((_, missing)) = ordered
        .iter()
        .enumerate()
        .find(|(index, _)| !delivered[*index])
    {
        return Err(PlacementViolation::Undelivered {
            schedule_index: missing.schedule_index,
            deadline_vns: missing.deadline_vns,
        });
    }
    Ok(())
}
