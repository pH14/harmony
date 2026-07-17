// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live Differential Dataflow host: one process, one Timely worker, one
//! dataflow, time = the `u64` campaign revision (the ruled doctrine — branch
//! is a key, `Revision` is the ONLY timestamp, no custom lattice).
//!
//! The committed-input relation is the coordinator's product: every
//! `(Revision, EvidenceBatchId)` pair the frontier machinery commits is
//! submitted here at its revision, consolidated in-graph, captured with its
//! `(data, revision, diff)` updates, and probed. Readers follow the spike
//! crate's read discipline: a view is read only after the probe has passed
//! the submitted revision, then consolidated and canonically ordered before
//! it can affect selection or serialized bytes.
//!
//! This is deliberately an ECHO program, not the production dataflow
//! (PR #124 F2 ruling): it proves the coordination contract — probe
//! barriers, frontier advancement, canonical reads — over the identity
//! relation. `hm-bbx.4` replaces the graph inside this seam with the spike
//! crate's proven relations; the coordinator's public surface and the
//! ledger protocol do not change when it does.
//!
//! The worker is built with `now: None` — timely runs entirely without a
//! wall-clock timer (no logging registry, no timer-based activations), so no
//! nondeterministic clock exists in the dataflow at all.

use std::sync::{Arc, Mutex};

use differential_dataflow::input::{Input, InputSession};
use timely::WorkerConfig;
use timely::communication::Allocator;
use timely::communication::allocator::thread::Thread;
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::worker::Worker;

/// The committed-input row: `(revision, batch digest)`. The revision rides
/// both as the DD timestamp and as a data column, exactly like the spike
/// crate's records carry their commit revision.
pub(crate) type Row = (u64, [u8; 32]);

/// Captured `(data, time, diff)` updates from the consolidated input view.
type CapturedUpdates = Arc<Mutex<Vec<(Row, u64, isize)>>>;

/// One Timely worker driving the committed-input dataflow.
pub(crate) struct ProbeHost {
    worker: Worker,
    input: InputSession<u64, Row, isize>,
    probe: ProbeHandle<u64>,
    captured: CapturedUpdates,
    /// The input epoch we have advanced to (monotone).
    epoch: u64,
}

impl ProbeHost {
    /// Build the worker and the committed-input dataflow.
    pub(crate) fn new() -> Self {
        let alloc = Allocator::Thread(Thread::default());
        let mut worker = Worker::new(WorkerConfig::default(), alloc, None);
        let captured: CapturedUpdates = Arc::default();
        let sink = Arc::clone(&captured);
        let (input, probe) = worker.dataflow::<u64, _, _>(move |scope| {
            let (input, committed) = scope.new_collection::<Row, isize>();
            let (probe, _) = committed
                .consolidate()
                .inspect_batch(move |_t, batch| {
                    // Statically infallible: one worker thread, and no code
                    // panics while the lock is held.
                    let mut rows = sink.lock().expect("single-threaded capture lock");
                    for (data, time, diff) in batch {
                        rows.push((*data, *time, *diff));
                    }
                })
                .probe();
            (input, probe)
        });
        ProbeHost {
            worker,
            input,
            probe,
            captured,
            epoch: 0,
        }
    }

    /// Submit one committed input at its revision. The caller (the
    /// coordinator's frontier machinery) guarantees `rev >= epoch` by only
    /// submitting the contiguous committed prefix in order.
    pub(crate) fn insert(&mut self, rev: u64, batch: [u8; 32]) {
        self.input.update_at((rev, batch), rev, 1);
    }

    /// Advance the input frontier to `to` (monotone; no-op if behind).
    pub(crate) fn advance(&mut self, to: u64) {
        if to > self.epoch {
            self.input.advance_to(to);
            self.input.flush();
            self.epoch = to;
        }
    }

    /// Step the worker until the probe frontier passes every time `< until`.
    /// The defensive break cannot fire while the dataflow holds an open
    /// input handle; it exists so a future wiring bug hangs a test assert
    /// instead of the process.
    pub(crate) fn drive(&mut self, until: u64) {
        while self.probe.less_than(&until) {
            if !self.worker.step() {
                break;
            }
        }
    }

    /// The consolidated, canonically ordered committed-input view at
    /// `visible` (inclusive): sum diffs for updates with time `<= visible`,
    /// drop zeros, sort. Only call after `drive` has passed `visible` — the
    /// probe-barrier read discipline.
    pub(crate) fn view(&self, visible: u64) -> Vec<Row> {
        let mut net: std::collections::BTreeMap<Row, isize> = std::collections::BTreeMap::new();
        // Statically infallible: one worker thread, no panic while held.
        let rows = self.captured.lock().expect("single-threaded capture lock");
        for (data, time, diff) in rows.iter() {
            if *time <= visible {
                *net.entry(*data).or_default() += *diff;
            }
        }
        net.into_iter()
            .filter(|(_, diff)| *diff != 0)
            .map(|(data, diff)| {
                // The coordinator submits each revision exactly once with
                // diff +1, so every surviving row is unit-multiplicity.
                debug_assert_eq!(diff, 1, "non-unit multiplicity for {data:?}");
                data
            })
            .collect()
    }
}
