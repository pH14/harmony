// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`LiveSink`] — the lossy, never-blocking live lane.
//!
//! The lossless [`crate::NdjsonRecorder`] is the authoritative record; the live
//! lane exists so a browser can watch a run **without** adding wall-clock pauses
//! to it. V-time is work-based, so even a blocking observer could not perturb the
//! run — but a live UI must never stall it either, so `LiveSink` **drops and
//! counts** on overflow instead of blocking. The drop count is surfaced as a
//! synthetic [`EventKind::Dropped`] event when the queue is next drained, so the
//! operator sees the gap rather than silently missing it.
//!
//! The sink is `Clone` (it shares one `Arc<Mutex<…>>` bounded ring): the VMM
//! driver holds one handle as its `&mut dyn Observer`, the web server holds
//! another to drain. `emit` and [`LiveSink::drain`] both lock the same ring.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::event::{Event, EventKind};
use crate::observer::Observer;

/// Default live-queue capacity if a caller does not pick one. Large enough to
/// ride out a browser stall of a few thousand exits, small enough to bound
/// memory.
pub const DEFAULT_CAPACITY: usize = 8192;

/// The shared, mutex-guarded ring behind every [`LiveSink`] clone.
#[derive(Debug)]
struct Ring {
    queue: VecDeque<Event>,
    capacity: usize,
    /// Events dropped since the last [`LiveSink::drain`] surfaced a notice.
    dropped: u64,
    /// `(seq, work, vns)` of the most recent `emit` (dropped or not), used to
    /// V-time-stamp a synthetic [`EventKind::Dropped`] so it lands on the
    /// timeline near where the loss happened.
    last_stamp: (u64, u64, u64),
}

/// A lossy, never-blocking [`Observer`] sink feeding the live web view.
///
/// Cloning shares the same bounded queue. `emit` pushes if there is room and
/// **drops + counts** if not — it never blocks and never allocates unboundedly.
#[derive(Clone, Debug)]
pub struct LiveSink {
    ring: Arc<Mutex<Ring>>,
}

impl LiveSink {
    /// Creates a sink with the given queue capacity. A capacity of `0` is valid
    /// and well-defined: every event drops and counts (the live view then sees
    /// only the synthetic [`EventKind::Dropped`] notices). `emit` never blocks at
    /// any capacity.
    pub fn new(capacity: usize) -> LiveSink {
        LiveSink {
            ring: Arc::new(Mutex::new(Ring {
                queue: VecDeque::new(),
                capacity,
                dropped: 0,
                last_stamp: (0, 0, 0),
            })),
        }
    }

    /// Creates a sink with [`DEFAULT_CAPACITY`].
    pub fn with_default_capacity() -> LiveSink {
        LiveSink::new(DEFAULT_CAPACITY)
    }

    /// Drains all buffered events, oldest first. If any were dropped since the
    /// last drain, a synthetic [`EventKind::Dropped`] is appended (stamped at the
    /// most recent `emit`'s V-time) and the drop counter is reset — so the gap is
    /// always surfaced exactly once.
    pub fn drain(&self) -> Vec<Event> {
        // A poisoned mutex means a panic while holding the lock; recover the
        // guard rather than propagate, so a drain can never panic the server.
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<Event> = ring.queue.drain(..).collect();
        if ring.dropped > 0 {
            let (seq, work, vns) = ring.last_stamp;
            out.push(Event::new(
                seq,
                work,
                vns,
                EventKind::Dropped {
                    count: ring.dropped,
                },
            ));
            ring.dropped = 0;
        }
        out
    }

    /// The number of events currently buffered (test/diagnostic use).
    pub fn len(&self) -> usize {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .len()
    }

    /// Whether the live queue is currently empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The configured queue capacity.
    pub fn capacity(&self) -> usize {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).capacity
    }

    /// The drop count not yet surfaced by a [`LiveSink::drain`] (test/diagnostic
    /// use).
    pub fn pending_dropped(&self) -> u64 {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).dropped
    }
}

impl Observer for LiveSink {
    fn emit(&mut self, ev: &Event) {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        ring.last_stamp = (ev.seq, ev.work, ev.vns);
        if ring.queue.len() >= ring.capacity {
            // Full: drop and count rather than block the run.
            ring.dropped = ring.dropped.saturating_add(1);
        } else {
            ring.queue.push_back(ev.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64) -> Event {
        Event::new(seq, seq, seq, EventKind::Inject { vector: 32 })
    }

    #[test]
    fn buffers_then_drains_in_order() {
        let mut sink = LiveSink::new(16);
        for i in 0..5 {
            sink.emit(&ev(i));
        }
        let drained = sink.drain();
        let seqs: Vec<u64> = drained.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
        assert!(sink.is_empty());
    }

    #[test]
    fn drops_dont_block_and_are_surfaced_once() {
        let mut sink = LiveSink::new(4);
        // Emit far more than capacity; emit must return promptly each time.
        for i in 0..100 {
            sink.emit(&ev(i));
        }
        assert_eq!(sink.len(), 4, "queue is capped at capacity");
        assert_eq!(sink.pending_dropped(), 96);

        let drained = sink.drain();
        // 4 buffered events + exactly one synthetic Dropped notice.
        assert_eq!(drained.len(), 5);
        let last = drained.last().expect("non-empty");
        assert_eq!(last.kind, EventKind::Dropped { count: 96 });
        // The notice is stamped at the most recent emit (seq 99).
        assert_eq!(last.seq, 99);
        assert_eq!(last.vns, 99);

        // Drained once: the counter resets, so a quiet period surfaces nothing.
        assert_eq!(sink.pending_dropped(), 0);
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn zero_capacity_drops_everything_without_deadlock() {
        let mut sink = LiveSink::new(0);
        for i in 0..10 {
            sink.emit(&ev(i));
        }
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, EventKind::Dropped { count: 10 });
    }

    #[test]
    fn is_empty_and_capacity_report_exact_values() {
        // capacity 7 is distinct from the 0/1 a mutated accessor would return.
        let mut sink = LiveSink::new(7);
        assert_eq!(sink.capacity(), 7, "capacity reports the configured value");
        assert!(sink.is_empty(), "a fresh sink is empty");

        sink.emit(&ev(1));
        assert!(!sink.is_empty(), "a sink holding an event is not empty");
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn clones_share_one_queue() {
        let mut producer = LiveSink::new(16);
        let consumer = producer.clone();
        producer.emit(&ev(7));
        assert_eq!(consumer.len(), 1);
        assert_eq!(consumer.drain()[0].seq, 7);
        assert!(producer.is_empty());
    }
}
