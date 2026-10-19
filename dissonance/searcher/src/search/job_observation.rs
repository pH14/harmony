// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionStatus {
    Disabled,
    Unavailable,
    Collected,
    BeginPanicked,
    DrainPanicked,
}

#[derive(Clone, Copy, Debug)]
pub struct ObservationReceipt<O> {
    pub status: CollectionStatus,
    pub payload: Option<O>,
}

impl<O> Default for ObservationReceipt<O> {
    fn default() -> Self {
        Self {
            status: CollectionStatus::Disabled,
            payload: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservationCounts {
    pub eligible: u64,
    pub collected: u64,
    pub unavailable: u64,
    pub begin_panics: u64,
    pub drain_panics: u64,
    pub delivered: u64,
    pub sink_errors: u64,
    pub sink_panics: u64,
    pub discarded: u64,
    pub sink_attempts: u64,
    pub overflow: bool,
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Counter {
    Eligible,
    Collected,
    Unavailable,
    BeginPanics,
    DrainPanics,
    Delivered,
    SinkErrors,
    SinkPanics,
    Discarded,
    SinkAttempts,
}
const COUNTERS: usize = Counter::SinkAttempts as usize + 1;

#[derive(Default)]
struct Counters {
    values: [AtomicU64; COUNTERS],
    overflow: AtomicBool,
}

#[derive(Clone, Default)]
pub struct ObservationDiagnostics(Arc<Counters>);

impl ObservationDiagnostics {
    pub(crate) fn add(&self, slot: Counter) {
        if self.0.values[slot as usize]
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .is_err()
        {
            self.0.overflow.store(true, Ordering::Relaxed);
        }
    }
    pub fn snapshot(&self) -> ObservationCounts {
        let v = self.0.values.each_ref().map(|n| n.load(Ordering::Relaxed));
        ObservationCounts {
            eligible: v[Counter::Eligible as usize],
            collected: v[Counter::Collected as usize],
            unavailable: v[Counter::Unavailable as usize],
            begin_panics: v[Counter::BeginPanics as usize],
            drain_panics: v[Counter::DrainPanics as usize],
            delivered: v[Counter::Delivered as usize],
            sink_errors: v[Counter::SinkErrors as usize],
            sink_panics: v[Counter::SinkPanics as usize],
            discarded: v[Counter::Discarded as usize],
            sink_attempts: v[Counter::SinkAttempts as usize],
            overflow: self.0.overflow.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn discard<O>(&self, receipt: &ObservationReceipt<O>) {
        if receipt.payload.is_some() {
            self.add(crate::search::job_observation::Counter::Discarded);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CompletionObservation<O> {
    pub worker: u32,
    pub reservation: usize,
    pub planned_finish: u64,
    pub parent_id: u64,
    pub snapshot_id: u64,
    pub job_failed: bool,
    pub receipt: ObservationReceipt<O>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationSinkError;

pub struct JobObserver<'a, O> {
    pub diagnostics: ObservationDiagnostics,
    pub sink: &'a mut dyn FnMut(CompletionObservation<O>) -> Result<(), ObservationSinkError>,
}

pub(crate) fn bounded<O: Copy + Send + 'static>() {
    const {
        assert!(std::mem::size_of::<O>() <= 256);
    }
}

pub(crate) struct ObservationDelivery<O> {
    pub(crate) receipt: ObservationReceipt<O>,
    pub(crate) diagnostics: Option<ObservationDiagnostics>,
}
impl<O> Default for ObservationDelivery<O> {
    fn default() -> Self {
        Self {
            receipt: ObservationReceipt::default(),
            diagnostics: None,
        }
    }
}
impl<O> ObservationDelivery<O> {
    pub(crate) fn take(&mut self) -> ObservationReceipt<O> {
        ObservationReceipt {
            status: self.receipt.status,
            payload: self.receipt.payload.take(),
        }
    }
}
impl<O> Drop for ObservationDelivery<O> {
    fn drop(&mut self) {
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.discard(&self.receipt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropped_and_taken_delivery_have_exact_terminal_accounting() {
        let counts = ObservationDiagnostics::default();
        counts.add(crate::search::job_observation::Counter::Collected);
        drop(ObservationDelivery {
            receipt: ObservationReceipt {
                status: CollectionStatus::Collected,
                payload: Some([7u64; 32]),
            },
            diagnostics: Some(counts.clone()),
        });
        assert_eq!(counts.snapshot().discarded, 1);
        let mut delivery = ObservationDelivery {
            receipt: ObservationReceipt {
                status: CollectionStatus::Collected,
                payload: Some(9u64),
            },
            diagnostics: Some(counts.clone()),
        };
        assert_eq!(delivery.take().payload, Some(9));
        assert_eq!(delivery.take().payload, None);
        drop(delivery);
        assert_eq!(counts.snapshot().discarded, 1);
        bounded::<[u64; 32]>();
    }
    #[test]
    fn counter_overflow_is_visible_without_wrapping_or_panicking() {
        let counts = ObservationDiagnostics::default();
        counts.0.values[Counter::Discarded as usize].store(u64::MAX, Ordering::Relaxed);
        counts.add(crate::search::job_observation::Counter::Discarded);
        assert_eq!(counts.snapshot().discarded, u64::MAX);
        assert!(counts.snapshot().overflow);
    }
    #[test]
    fn snapshot_fields_follow_counter_variants_not_their_ordinal_position() {
        let counts = ObservationDiagnostics::default();
        for _ in 0..2 {
            counts.add(Counter::Delivered);
        }
        for _ in 0..3 {
            counts.add(Counter::Discarded);
        }
        counts.add(Counter::BeginPanics);
        counts.add(Counter::SinkErrors);
        let snapshot = counts.snapshot();
        assert_eq!(snapshot.delivered, 2);
        assert_eq!(snapshot.discarded, 3);
        assert_eq!(snapshot.begin_panics, 1);
        assert_eq!(snapshot.sink_errors, 1);
        assert_eq!(snapshot.eligible, 0);
        assert_eq!(snapshot.collected, 0);
    }
}
