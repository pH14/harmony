// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::event::{Event, EventKind};
use crate::observer::Observer;

pub const DEFAULT_CAPACITY: usize = 8192;

#[derive(Debug)]
struct Ring {
    queue: VecDeque<Event>,
    capacity: usize,
    dropped: u64,
    last_stamp: (u64, u64, u64),
}

#[derive(Clone, Debug)]
pub struct LiveSink {
    ring: Arc<Mutex<Ring>>,
}

impl LiveSink {
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

    pub fn with_default_capacity() -> LiveSink {
        LiveSink::new(DEFAULT_CAPACITY)
    }

    pub fn drain(&self) -> Vec<Event> {
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

    pub fn len(&self) -> usize {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).capacity
    }

    pub fn pending_dropped(&self) -> u64 {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).dropped
    }
}

impl Observer for LiveSink {
    fn emit(&mut self, ev: &Event) {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        ring.last_stamp = (ev.seq, ev.exit_count, ev.vns);
        if ring.queue.len() >= ring.capacity {
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
        for i in 0..100 {
            sink.emit(&ev(i));
        }
        assert_eq!(sink.len(), 4, "queue is capped at capacity");
        assert_eq!(sink.pending_dropped(), 96);

        let drained = sink.drain();
        assert_eq!(drained.len(), 5);
        let last = drained.last().expect("non-empty");
        assert_eq!(last.kind, EventKind::Dropped { count: 96 });
        assert_eq!(last.seq, 99);
        assert_eq!(last.vns, 99);

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
