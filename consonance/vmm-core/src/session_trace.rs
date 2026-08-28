// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore-aware accumulation of production virtual_time traces.
//!
//! A control session replaces its live [`crate::vmm::Vmm`] on every branch or
//! replay. Each replacement starts a fresh [`crate::virtual_time::LiveVirtualTimeTrace`]
//! whose event and schedule indices begin at zero and whose V-time may rewind.
//! This module preserves those traces as an ordered sequence of segments instead
//! of flattening them into a structurally invalid single run.

use std::io::{self, Write};

use sha2::{Digest, Sha256};

use crate::virtual_time::{
    LiveVirtualTimeTrace, LogDivergence, NormalizedLog, PlacementViolation, ScheduledInterrupt,
    check_delivery_placement, compare_normalized_logs,
};

/// How a restore-delimited trace segment began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTraceStart {
    /// The VM with which the control session was constructed.
    InitialBoot,
    /// A fresh boot kept after a recoverable restore rejection.
    RecoveryBoot,
    /// A successful entropy-forking branch from a session-local snapshot.
    Branch {
        /// Session-local snapshot handle.
        snapshot: u64,
    },
    /// A successful verbatim replay from a session-local snapshot.
    Replay {
        /// Session-local snapshot handle.
        snapshot: u64,
    },
}

impl SessionTraceStart {
    fn encode(self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::InitialBoot => write!(out, "initial"),
            Self::RecoveryBoot => write!(out, "recovery"),
            Self::Branch { snapshot } => write!(out, "branch:{snapshot}"),
            Self::Replay { snapshot } => write!(out, "replay:{snapshot}"),
        }
    }
}

/// One maximal normalized trace between control-session VM replacements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTraceSegment {
    start: SessionTraceStart,
    normalized: NormalizedLog,
    schedule: Vec<ScheduledInterrupt>,
}

impl SessionTraceSegment {
    pub(crate) fn capture(start: SessionTraceStart, trace: &LiveVirtualTimeTrace) -> Self {
        Self {
            start,
            normalized: trace.normalized_log().clone(),
            schedule: trace.schedule().to_vec(),
        }
    }

    /// Boundary that began this segment.
    pub fn start(&self) -> SessionTraceStart {
        self.start
    }

    /// Complete normalized event log for this segment.
    pub fn normalized_log(&self) -> &NormalizedLog {
        &self.normalized
    }

    /// Immutable interrupt schedule for this segment.
    pub fn schedule(&self) -> &[ScheduledInterrupt] {
        &self.schedule
    }
}

/// Complete restore-aware production trace for one control session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionVirtualTimeTrace {
    segments: Vec<SessionTraceSegment>,
}

impl SessionVirtualTimeTrace {
    pub(crate) fn from_segments(segments: Vec<SessionTraceSegment>) -> Self {
        Self { segments }
    }

    /// Ordered restore-delimited segments, including the current live VM.
    pub fn segments(&self) -> &[SessionTraceSegment] {
        &self.segments
    }

    /// Total portable events across every segment.
    pub fn event_count(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| segment.normalized.events.len())
            .sum()
    }

    /// Total immutable schedule records across every segment.
    pub fn schedule_count(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| segment.schedule.len())
            .sum()
    }

    /// Total full-state checkpoints across every segment.
    pub fn checkpoint_count(&self) -> usize {
        self.segments
            .iter()
            .flat_map(|segment| &segment.normalized.events)
            .filter(|event| event.state_hash.is_some())
            .count()
    }

    /// SHA-256 over the complete fixed text encoding, domain-separated from a
    /// single live-VMM trace digest.
    pub fn digest(&self) -> [u8; 32] {
        let mut body = Vec::new();
        // Writing to Vec is infallible.
        self.write_body(&mut body)
            .expect("writing session trace to Vec cannot fail");
        let mut hasher = Sha256::new();
        // Frozen v1 log-domain identifier: changing it would invalidate N1 byte fixtures.
        hasher.update(b"consonance.session-prescriptive-log.v1\0");
        hasher.update(body);
        hasher.finalize().into()
    }

    /// Write the complete, stable, host-neutral trace encoding.
    ///
    /// Raw backend diagnostics are deliberately absent: only portable events,
    /// checkpoint hashes, schedules, and restore boundaries enter this file.
    pub fn write_text(&self, mut out: impl Write) -> io::Result<()> {
        writeln!(out, "format consonance.session-prescriptive-log.v1")?;
        writeln!(out, "digest {}", hex(&self.digest()))?;
        self.write_body(&mut out)
    }

    fn write_body(&self, mut out: impl Write) -> io::Result<()> {
        writeln!(out, "segments {}", self.segments.len())?;
        for (segment_index, segment) in self.segments.iter().enumerate() {
            write!(out, "segment {segment_index} start=")?;
            segment.start.encode(&mut out)?;
            writeln!(
                out,
                " events={} schedules={}",
                segment.normalized.events.len(),
                segment.schedule.len()
            )?;
            for event in &segment.normalized.events {
                writeln!(
                    out,
                    "event {segment_index}:{} class={:?} payload={} vns={} interrupts={:?} state_hash={}",
                    event.event_index,
                    event.class,
                    hex(&event.payload_digest),
                    event.vns_after,
                    event.interrupts,
                    event
                        .state_hash
                        .as_ref()
                        .map_or_else(|| "-".to_string(), |hash| hex(hash)),
                )?;
            }
            for scheduled in &segment.schedule {
                writeln!(
                    out,
                    "schedule {segment_index}:{} deadline_vns={} armed_for_event={} canceled_at_event={:?} interrupt_id={}",
                    scheduled.schedule_index,
                    scheduled.deadline_vns,
                    scheduled.armed_for_event,
                    scheduled.canceled_at_event,
                    scheduled.interrupt_id,
                )?;
            }
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Exact first divergence between two restore-aware session traces.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionTraceDivergence {
    /// One session replaced its VM a different number of times.
    #[error("session trace segment count differs: left {left}, right {right}")]
    SegmentCount {
        /// Left segment count.
        left: usize,
        /// Right segment count.
        right: usize,
    },
    /// A segment began from a different control transition.
    #[error("session trace segment {segment_index} start differs")]
    Start {
        /// Exact divergent segment.
        segment_index: usize,
    },
    /// A segment's normalized log diverged.
    #[error("session trace segment {segment_index}: {divergence}")]
    Log {
        /// Exact divergent segment.
        segment_index: usize,
        /// Exact event and field within the segment.
        divergence: LogDivergence,
    },
    /// A segment's immutable interrupt schedule diverged.
    #[error("session trace segment {segment_index} schedule differs")]
    Schedule {
        /// Exact divergent segment.
        segment_index: usize,
    },
}

/// Compare complete restore-aware traces and report their first divergence.
pub fn compare_session_traces(
    left: &SessionVirtualTimeTrace,
    right: &SessionVirtualTimeTrace,
) -> Result<(), SessionTraceDivergence> {
    for (segment_index, (a, b)) in left.segments.iter().zip(&right.segments).enumerate() {
        if a.start != b.start {
            return Err(SessionTraceDivergence::Start { segment_index });
        }
        compare_normalized_logs(&a.normalized, &b.normalized).map_err(|divergence| {
            SessionTraceDivergence::Log {
                segment_index,
                divergence,
            }
        })?;
        if a.schedule != b.schedule {
            return Err(SessionTraceDivergence::Schedule { segment_index });
        }
    }
    if left.segments.len() != right.segments.len() {
        return Err(SessionTraceDivergence::SegmentCount {
            left: left.segments.len(),
            right: right.segments.len(),
        });
    }
    Ok(())
}

/// Field within a continuation schedule record that differs after rebasing the
/// source snapshot cut to destination event/schedule zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationScheduleField {
    /// Absolute V-time deadline.
    DeadlineVns,
    /// FIFO schedule identity.
    ScheduleIndex,
    /// First event at which the deadline is eligible.
    ArmedForEvent,
    /// Optional cancellation event.
    CanceledAtEvent,
    /// Vendor-neutral interrupt identity.
    InterruptId,
}

/// Exact first mismatch in a portable midpoint continuation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContinuationDivergence {
    /// A source cut lies beyond the source segment it names.
    #[error(
        "portable continuation cut is out of range: events {event_cut}/{source_events}, schedules {schedule_cut}/{source_schedules}"
    )]
    CutOutOfRange {
        /// Requested source event prefix length.
        event_cut: usize,
        /// Complete source event count.
        source_events: usize,
        /// Requested source schedule prefix length.
        schedule_cut: usize,
        /// Complete source schedule count.
        source_schedules: usize,
    },
    /// The rebased normalized event streams differ.
    #[error("portable continuation {divergence}")]
    Log {
        /// Exact relative event and field.
        divergence: LogDivergence,
    },
    /// The rebased schedule suffixes have different lengths.
    #[error(
        "portable continuation schedule count differs: source {source_count}, restored {restored_count}"
    )]
    ScheduleCount {
        /// Number of source schedules after the cut.
        source_count: usize,
        /// Number of schedules in the restored continuation.
        restored_count: usize,
    },
    /// One rebased schedule record differs.
    #[error("portable continuation schedule {schedule_index} differs in {field:?}")]
    Schedule {
        /// Relative schedule index.
        schedule_index: u64,
        /// Exact differing schedule field.
        field: ContinuationScheduleField,
    },
    /// The independently sampled whole-state boundary sequences differ in length.
    #[error(
        "portable continuation boundary-hash count differs: source {source_count}, restored {restored_count}"
    )]
    BoundaryHashCount {
        /// Source boundary count.
        source_count: usize,
        /// Restored boundary count.
        restored_count: usize,
    },
    /// An independently sampled whole-state boundary hash differs.
    #[error("portable continuation state hash differs at boundary {boundary_index}")]
    BoundaryStateHash {
        /// Zero-based boundary after restore; zero is the immediate restore hash.
        boundary_index: usize,
    },
}

/// Compare a source segment after a portable snapshot cut with the complete
/// restored segment and an independently sampled whole-state hash sequence.
///
/// Source event and schedule identities are rebased by their respective cut
/// prefix lengths. The comparison then covers every normalized event field,
/// embedded checkpoint hash, interrupt delivery, schedule record, and supplied
/// boundary hash. The restored segment must begin at event/schedule zero. A
/// delivery or schedule that refers to an identity before the cut fails closed;
/// callers must choose a seal boundary with no pre-cut deadline still live.
pub fn compare_portable_continuation(
    source: &SessionTraceSegment,
    source_event_cut: usize,
    source_schedule_cut: usize,
    source_boundary_hashes: &[[u8; 32]],
    restored: &SessionTraceSegment,
    restored_boundary_hashes: &[[u8; 32]],
) -> Result<(), ContinuationDivergence> {
    if source_event_cut > source.normalized.events.len()
        || source_schedule_cut > source.schedule.len()
    {
        return Err(ContinuationDivergence::CutOutOfRange {
            event_cut: source_event_cut,
            source_events: source.normalized.events.len(),
            schedule_cut: source_schedule_cut,
            source_schedules: source.schedule.len(),
        });
    }

    let event_base = u64::try_from(source_event_cut).unwrap_or(u64::MAX);
    let schedule_base = u64::try_from(source_schedule_cut).unwrap_or(u64::MAX);
    let source_events = &source.normalized.events[source_event_cut..];
    for (offset, (a, b)) in source_events
        .iter()
        .zip(&restored.normalized.events)
        .enumerate()
    {
        let relative = u64::try_from(offset).unwrap_or(u64::MAX);
        let field = if a.event_index.checked_sub(event_base) != Some(relative)
            || b.event_index != relative
        {
            Some(crate::virtual_time::LogField::EventIndex)
        } else if a.class != b.class {
            Some(crate::virtual_time::LogField::Class)
        } else if a.payload_digest != b.payload_digest {
            Some(crate::virtual_time::LogField::PayloadDigest)
        } else if a.vns_after != b.vns_after {
            Some(crate::virtual_time::LogField::VnsAfter)
        } else if !interrupts_equal_rebased(&a.interrupts, &b.interrupts, schedule_base) {
            Some(crate::virtual_time::LogField::Interrupts)
        } else if a.state_hash != b.state_hash {
            Some(crate::virtual_time::LogField::StateHash)
        } else {
            None
        };
        if let Some(field) = field {
            return Err(ContinuationDivergence::Log {
                divergence: LogDivergence {
                    event_index: relative,
                    field,
                },
            });
        }
    }
    if source_events.len() != restored.normalized.events.len() {
        return Err(ContinuationDivergence::Log {
            divergence: LogDivergence {
                event_index: u64::try_from(
                    source_events.len().min(restored.normalized.events.len()),
                )
                .unwrap_or(u64::MAX),
                field: crate::virtual_time::LogField::Length,
            },
        });
    }

    let source_schedules = &source.schedule[source_schedule_cut..];
    for (offset, (a, b)) in source_schedules.iter().zip(&restored.schedule).enumerate() {
        let relative = u64::try_from(offset).unwrap_or(u64::MAX);
        let field = if a.deadline_vns != b.deadline_vns {
            Some(ContinuationScheduleField::DeadlineVns)
        } else if a.schedule_index.checked_sub(schedule_base) != Some(relative)
            || b.schedule_index != relative
        {
            Some(ContinuationScheduleField::ScheduleIndex)
        } else if a.armed_for_event.checked_sub(event_base) != Some(b.armed_for_event) {
            Some(ContinuationScheduleField::ArmedForEvent)
        } else if rebase_optional(a.canceled_at_event, event_base) != Some(b.canceled_at_event) {
            Some(ContinuationScheduleField::CanceledAtEvent)
        } else if a.interrupt_id != b.interrupt_id {
            Some(ContinuationScheduleField::InterruptId)
        } else {
            None
        };
        if let Some(field) = field {
            return Err(ContinuationDivergence::Schedule {
                schedule_index: relative,
                field,
            });
        }
    }
    if source_schedules.len() != restored.schedule.len() {
        return Err(ContinuationDivergence::ScheduleCount {
            source_count: source_schedules.len(),
            restored_count: restored.schedule.len(),
        });
    }

    for (boundary_index, (source, restored)) in source_boundary_hashes
        .iter()
        .zip(restored_boundary_hashes)
        .enumerate()
    {
        if source != restored {
            return Err(ContinuationDivergence::BoundaryStateHash { boundary_index });
        }
    }
    if source_boundary_hashes.len() != restored_boundary_hashes.len() {
        return Err(ContinuationDivergence::BoundaryHashCount {
            source_count: source_boundary_hashes.len(),
            restored_count: restored_boundary_hashes.len(),
        });
    }
    Ok(())
}

fn interrupts_equal_rebased(
    source: &[crate::virtual_time::InterruptDelivery],
    restored: &[crate::virtual_time::InterruptDelivery],
    schedule_base: u64,
) -> bool {
    source.len() == restored.len()
        && source.iter().zip(restored).all(|(a, b)| {
            a.deadline_vns == b.deadline_vns
                && a.schedule_index.checked_sub(schedule_base) == Some(b.schedule_index)
                && a.interrupt_id == b.interrupt_id
        })
}

fn rebase_optional(value: Option<u64>, base: u64) -> Option<Option<u64>> {
    match value {
        Some(value) => value.checked_sub(base).map(Some),
        None => Some(None),
    }
}

/// Independent delivery-placement failure localized to a session segment.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("session trace segment {segment_index}: {violation}")]
pub struct SessionPlacementViolation {
    /// Exact failing segment.
    pub segment_index: usize,
    /// Independent per-segment schedule violation.
    pub violation: PlacementViolation,
}

/// Independently check every restore-delimited segment's delivery placement.
pub fn check_session_delivery_placement(
    trace: &SessionVirtualTimeTrace,
) -> Result<(), SessionPlacementViolation> {
    for (segment_index, segment) in trace.segments.iter().enumerate() {
        check_delivery_placement(&segment.schedule, &segment.normalized).map_err(|violation| {
            SessionPlacementViolation {
                segment_index,
                violation,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_time::{InterruptDelivery, LogField, NormalizedEvent, NormalizedEventClass};

    fn segment(start: SessionTraceStart, vns: u64) -> SessionTraceSegment {
        SessionTraceSegment {
            start,
            normalized: NormalizedLog {
                events: vec![NormalizedEvent {
                    event_index: 0,
                    class: NormalizedEventClass::TimeRead,
                    payload_digest: [3; 32],
                    vns_after: vns,
                    interrupts: Vec::new(),
                    state_hash: Some([5; 32]),
                }],
            },
            schedule: Vec::new(),
        }
    }

    #[test]
    fn comparator_accepts_an_identical_restore_aware_sequence() {
        let trace = SessionVirtualTimeTrace::from_segments(vec![
            segment(SessionTraceStart::InitialBoot, 10),
            segment(SessionTraceStart::Replay { snapshot: 1 }, 4),
            segment(SessionTraceStart::Branch { snapshot: 2 }, 7),
        ]);
        assert_eq!(compare_session_traces(&trace, &trace.clone()), Ok(()));
        assert_eq!(check_session_delivery_placement(&trace), Ok(()));
        assert_eq!(trace.event_count(), 3);
        assert_eq!(trace.checkpoint_count(), 3);
    }

    #[test]
    fn comparator_rejects_a_planted_middle_segment_increment() {
        let left = SessionVirtualTimeTrace::from_segments(vec![
            segment(SessionTraceStart::InitialBoot, 10),
            segment(SessionTraceStart::Replay { snapshot: 1 }, 4),
            segment(SessionTraceStart::Branch { snapshot: 2 }, 7),
        ]);
        let mut right = left.clone();
        right.segments[1].normalized.events[0].vns_after += 1;
        assert_eq!(
            compare_session_traces(&left, &right),
            Err(SessionTraceDivergence::Log {
                segment_index: 1,
                divergence: LogDivergence {
                    event_index: 0,
                    field: LogField::VnsAfter,
                },
            })
        );
        assert_ne!(left.digest(), right.digest());
    }

    #[test]
    fn segment_boundaries_permit_real_vtime_rewinds() {
        let trace = SessionVirtualTimeTrace::from_segments(vec![
            segment(SessionTraceStart::InitialBoot, 100),
            segment(SessionTraceStart::Replay { snapshot: 1 }, 10),
        ]);
        assert_eq!(check_session_delivery_placement(&trace), Ok(()));
    }

    fn portable_pair() -> (SessionTraceSegment, SessionTraceSegment) {
        let event =
            |event_index: u64, vns_after: u64, schedule_index: Option<u64>| NormalizedEvent {
                event_index,
                class: NormalizedEventClass::TimeRead,
                payload_digest: [event_index as u8; 32],
                vns_after,
                interrupts: schedule_index
                    .map(|schedule_index| {
                        vec![InterruptDelivery {
                            deadline_vns: 30,
                            schedule_index,
                            interrupt_id: 27,
                        }]
                    })
                    .unwrap_or_default(),
                state_hash: Some([event_index as u8; 32]),
            };
        let source = SessionTraceSegment {
            start: SessionTraceStart::Branch { snapshot: 8 },
            normalized: NormalizedLog {
                events: vec![
                    event(0, 10, None),
                    event(1, 20, None),
                    event(2, 30, Some(1)),
                ],
            },
            schedule: vec![
                ScheduledInterrupt {
                    deadline_vns: 9,
                    schedule_index: 0,
                    armed_for_event: 0,
                    canceled_at_event: Some(0),
                    interrupt_id: 27,
                },
                ScheduledInterrupt {
                    deadline_vns: 30,
                    schedule_index: 1,
                    armed_for_event: 1,
                    canceled_at_event: Some(2),
                    interrupt_id: 27,
                },
            ],
        };
        let mut restored_events = source.normalized.events[1..].to_vec();
        for (event_index, event) in restored_events.iter_mut().enumerate() {
            event.event_index = u64::try_from(event_index).unwrap();
            for delivery in &mut event.interrupts {
                delivery.schedule_index -= 1;
            }
        }
        let restored = SessionTraceSegment {
            start: SessionTraceStart::Replay { snapshot: 1 },
            normalized: NormalizedLog {
                events: restored_events,
            },
            schedule: vec![ScheduledInterrupt {
                deadline_vns: 30,
                schedule_index: 0,
                armed_for_event: 0,
                canceled_at_event: Some(1),
                interrupt_id: 27,
            }],
        };
        (source, restored)
    }

    #[test]
    fn portable_continuation_rebases_and_compares_every_retained_field() {
        let (source, restored) = portable_pair();
        let hashes = [[7; 32], [8; 32], [9; 32]];
        assert_eq!(
            compare_portable_continuation(&source, 1, 1, &hashes, &restored, &hashes),
            Ok(())
        );
    }

    #[test]
    fn portable_continuation_catches_a_planted_increment_at_the_exact_event() {
        let (source, mut restored) = portable_pair();
        restored.normalized.events[1].vns_after += 1;
        let hashes = [[7; 32], [8; 32], [9; 32]];
        assert_eq!(
            compare_portable_continuation(&source, 1, 1, &hashes, &restored, &hashes),
            Err(ContinuationDivergence::Log {
                divergence: LogDivergence {
                    event_index: 1,
                    field: LogField::VnsAfter,
                },
            })
        );
    }

    #[test]
    fn portable_continuation_catches_a_planted_boundary_hash() {
        let (source, restored) = portable_pair();
        let source_hashes = [[7; 32], [8; 32], [9; 32]];
        let mut restored_hashes = source_hashes;
        restored_hashes[1][17] ^= 1;
        assert_eq!(
            compare_portable_continuation(
                &source,
                1,
                1,
                &source_hashes,
                &restored,
                &restored_hashes,
            ),
            Err(ContinuationDivergence::BoundaryStateHash { boundary_index: 1 })
        );
    }
}
