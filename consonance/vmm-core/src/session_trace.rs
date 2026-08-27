// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore-aware accumulation of production prescriptive traces.
//!
//! A control session replaces its live [`crate::vmm::Vmm`] on every branch or
//! replay. Each replacement starts a fresh [`crate::prescriptive::LivePrescriptiveTrace`]
//! whose event and schedule indices begin at zero and whose V-time may rewind.
//! This module preserves those traces as an ordered sequence of segments instead
//! of flattening them into a structurally invalid single run.

use std::io::{self, Write};

use sha2::{Digest, Sha256};

use crate::prescriptive::{
    LivePrescriptiveTrace, LogDivergence, NormalizedLog, PlacementViolation, ScheduledInterrupt,
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
    pub(crate) fn capture(start: SessionTraceStart, trace: &LivePrescriptiveTrace) -> Self {
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
pub struct SessionPrescriptiveTrace {
    segments: Vec<SessionTraceSegment>,
}

impl SessionPrescriptiveTrace {
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
    left: &SessionPrescriptiveTrace,
    right: &SessionPrescriptiveTrace,
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
    trace: &SessionPrescriptiveTrace,
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
    use crate::prescriptive::{LogField, NormalizedEvent, NormalizedEventClass};

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
        let trace = SessionPrescriptiveTrace::from_segments(vec![
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
        let left = SessionPrescriptiveTrace::from_segments(vec![
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
        let trace = SessionPrescriptiveTrace::from_segments(vec![
            segment(SessionTraceStart::InitialBoot, 100),
            segment(SessionTraceStart::Replay { snapshot: 1 }, 10),
        ]);
        assert_eq!(check_session_delivery_placement(&trace), Ok(()));
    }
}
