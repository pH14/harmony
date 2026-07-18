// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live Differential Dataflow host: one process, one Timely worker, one
//! dataflow, time = the `u64` campaign revision (the ruled doctrine — branch
//! is a key, `Revision` is the ONLY timestamp, no custom lattice; the one
//! nested timestamp is the standard `Product<u64, u64>` inside the ancestry
//! iteration).
//!
//! The committed-input relation is the coordinator's product: every
//! `(Revision, EvidenceBatchId)` pair the frontier machinery commits is
//! submitted here at its revision, consolidated in-graph, captured with its
//! `(data, revision, diff)` updates, and probed. Readers follow the spike
//! crate's read discipline: a view is read only after the probe has passed
//! the submitted revision, then consolidated and canonically ordered before
//! it can affect selection or serialized bytes.
//!
//! Alongside the committed-input relation, this host now runs the
//! **production observation/materialization relations** (task 132, `hm-e6q`
//! — the `spikes/differential-lineage` shapes, productionized): staged
//! [`EvidenceRows`] enter at their batch's committed revision, and the graph
//! materializes lineage-composed per-observation reductions at every
//! evaluation point (the shared segment-aggregate formulation the spike
//! proved equal to direct recomputation), the cell projection at each point,
//! and the deterministic best-entry-per-cell occupancy. Direct recomputation
//! remains the *oracle* (the explorer's pure functions assert parity in
//! tests); these relations are the production backend.
//!
//! The worker is built with `now: None` — timely runs entirely without a
//! wall-clock timer (no logging registry, no timer-based activations), so no
//! nondeterministic clock exists in the dataflow at all.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::iterate::Iterate;
use serde::{Deserialize, Serialize};
use timely::WorkerConfig;
use timely::communication::Allocator;
use timely::communication::allocator::thread::Thread;
use timely::dataflow::operators::probe::Handle as ProbeHandle;
use timely::worker::Worker;

use crate::relations::{
    Agg, CellProjection, CutRow, EntryKey, EvidenceRows, MaterializedViews, ObsKey, PointRow,
    ReduceOp, ReducedRow, RolloutKey, SealKey, StateEventRow,
};

/// The committed-input row: `(revision, batch digest)`. The revision rides
/// both as the DD timestamp and as a data column, exactly like the spike
/// crate's records carry their commit revision.
pub(crate) type Row = (u64, [u8; 32]);

/// Captured `(data, revision, diff)` updates from one consolidated view.
type Updates<T> = Arc<Mutex<Vec<(T, u64, isize)>>>;

/// One reduced-observation view row.
type ObsViewRow = ((RolloutKey, PointRow, ObsKey), ReducedRow);
/// One cell view row (the point's cut rides as data).
type CellViewRow = ((RolloutKey, PointRow, CutRow), Vec<u8>);
/// One occupancy view row.
type OccViewRow = (Vec<u8>, EntryKey);

/// One committed Entry offer input row.
type EntryInRow = ((RolloutKey, SealKey), (u64, EntryKey));

/// The value fed to the cell reduction: each point's seed (carrying its cut)
/// plus its reduced observations. The seed sorts first (`Ord` on the enum),
/// so the reduce sees it before any observation pair.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
enum CellIn {
    /// The point's existence + its cut (every point has exactly one).
    Seed(CutRow),
    /// One reduced observation at the point.
    Obs(ObsKey, ReducedRow),
}

/// The production relation input sessions, all timestamped by revision.
struct RelationInputs {
    /// `(obs, op)` declarations (deduplicated by the coordinator before
    /// feeding — a declaration is fed exactly once).
    declares: InputSession<u64, (ObsKey, ReduceOp), isize>,
    /// `(rollout, event)` state events (cumulative positions).
    events: InputSession<u64, (RolloutKey, StateEventRow), isize>,
    /// `(child, (parent, fork count))` lineage edges.
    lineage: InputSession<u64, (RolloutKey, (RolloutKey, u64)), isize>,
    /// `(rollout, (point, cut))` evaluation points (provisional cuts and
    /// candidate seals).
    points: InputSession<u64, (RolloutKey, (PointRow, CutRow)), isize>,
    /// `((rollout, seal), (quality, entry))` committed Entry offers.
    entries: InputSession<u64, EntryInRow, isize>,
}

impl RelationInputs {
    fn advance_flush(&mut self, to: u64) {
        self.declares.advance_to(to);
        self.events.advance_to(to);
        self.lineage.advance_to(to);
        self.points.advance_to(to);
        self.entries.advance_to(to);
        self.declares.flush();
        self.events.flush();
        self.lineage.flush();
        self.points.flush();
        self.entries.flush();
    }
}

/// The captured consolidated views (shared with the dataflow's inspectors).
#[derive(Default)]
struct CapturedViews {
    observations: Updates<ObsViewRow>,
    cells: Updates<CellViewRow>,
    occupancy: Updates<OccViewRow>,
}

/// Greatest present cumulative entry strictly below `below` (partials are
/// keyed by their interval's lower boundary; a point's count is itself a
/// boundary, which is what makes this lookup exact).
fn lookup(cum: &[(u64, Agg)], below: u64) -> Option<&Agg> {
    cum.iter().rev().find(|(b, _)| *b < below).map(|(_, a)| a)
}

/// Fold a reduce input slice of `Agg` units into one aggregate. Evidence
/// rows are unique by coordinate and fed exactly once, so multiplicities are
/// one and the aggregates are multiplicity-insensitive (no counting kind
/// exists here). The reduce contract guarantees a non-empty slice, so `None`
/// never escapes for one.
fn fold_units(input: &[(&Agg, isize)]) -> Option<Agg> {
    let mut acc: Option<Agg> = None;
    for (agg, _w) in input {
        acc = Some(match acc {
            Some(a) => a.combine(agg),
            None => (*agg).clone(),
        });
    }
    acc
}

/// One Timely worker driving the committed-input dataflow plus the
/// production observation/materialization relations.
pub(crate) struct ProbeHost {
    worker: Worker,
    input: InputSession<u64, Row, isize>,
    relations: RelationInputs,
    probe: ProbeHandle<u64>,
    captured: Updates<Row>,
    views: Arc<CapturedViews>,
    /// The input epoch we have advanced to (monotone).
    epoch: u64,
    /// The exclusive probe watermark `drive` has passed (monotone): every
    /// time `< driven` is complete, so views are readable at `driven - 1`.
    driven: u64,
}

impl ProbeHost {
    /// Build the worker and the dataflow under `proj` as the cell
    /// projection.
    pub(crate) fn new(proj: CellProjection) -> Self {
        let alloc = Allocator::Thread(Thread::default());
        let mut worker = Worker::new(WorkerConfig::default(), alloc, None);
        let captured: Updates<Row> = Arc::default();
        let views: Arc<CapturedViews> = Arc::default();
        let sink = Arc::clone(&captured);
        let views_in = Arc::clone(&views);
        let probe = ProbeHandle::new();
        let probe_in = probe.clone();

        let (input, relations) = worker.dataflow::<u64, _, _>(move |scope| {
            let probe = probe_in;

            // Capture one consolidated view and attach the probe to it.
            macro_rules! cap {
                ($coll:expr, $sink:expr) => {{
                    let a = Arc::clone(&$sink);
                    $coll
                        .consolidate()
                        .inspect_batch(move |_t, batch| {
                            // Statically infallible: one worker thread, and
                            // no code panics while the lock is held.
                            let mut rows = a.lock().expect("single-threaded capture lock");
                            for (d, t, r) in batch {
                                rows.push((d.clone(), *t, *r));
                            }
                        })
                        .probe_with(&probe)
                }};
            }

            // -- The committed-input relation (the coordination contract,
            // unchanged: DrainedView is read from exactly this capture). ----
            let (input, committed) = scope.new_collection::<Row, isize>();
            let _committed = cap!(committed, sink);

            // -- Production relation inputs. --------------------------------
            let (declares_in, declares) = scope.new_collection::<(ObsKey, ReduceOp), isize>();
            let (events_in, events) = scope.new_collection::<(RolloutKey, StateEventRow), isize>();
            let (lineage_in, lineage) =
                scope.new_collection::<(RolloutKey, (RolloutKey, u64)), isize>();
            let (points_in, points) =
                scope.new_collection::<(RolloutKey, (PointRow, CutRow)), isize>();
            let (entries_in, entries) =
                scope.new_collection::<((RolloutKey, SealKey), (u64, EntryKey)), isize>();

            // Measures: events joined with their declared base op, keyed by
            // rollout: ((rollout), (obs, op, pos, value)).
            let measures = events
                .map(|(r, e)| (e.obs, (r, e.pos, e.value)))
                .join_map(declares, |obs, &(r, pos, value), &op| {
                    (r, (obs.clone(), op, pos, value))
                });

            // Ancestors: ((rollout), (ancestor, upper cut count, depth)), by
            // iteration over the lineage relation. The inner timestamp is
            // the standard Product<u64, u64> — no custom lattice.
            let edges = lineage;
            let anc = {
                let edges_c = edges.clone();
                let base = edges.clone().map(|(r, (p, u))| (r, (p, u, 1u32)));
                let base_c = base.clone();
                base.iterate(move |scope, inner| {
                    let edges = edges_c.enter(scope);
                    let base = base_c.enter(scope);
                    let step = inner
                        .clone()
                        .map(|(r, (a, _u, d))| (a, (r, d)))
                        .join_map(edges, |&_a, &(r, d), &(b, u2)| (r, (b, u2, d + 1)));
                    base.concat(step).distinct()
                })
            };

            // -- Shared segment-aggregate formulation -----------------------
            // Boundary vector per rollout: every point count plus every fork
            // count of its children (a point's count is itself a boundary,
            // which makes the cumulative lookup exact).
            let breq = points
                .clone()
                .map(|(r, (_p, cut))| (r, cut.count))
                .concat(edges.map(|(_child, (parent, u))| (parent, u)))
                .distinct();
            let bounds = breq.reduce(|_k, input, output| {
                // Values arrive sorted and distinct: the boundary vector.
                let v: Vec<u64> = input.iter().map(|(b, _)| **b).collect();
                output.push((v, 1isize));
            });

            // Assign each measure to its interval's lower boundary.
            let units = measures.join_map(
                bounds,
                |&r, &(ref obs, op, pos, value), bounds: &Vec<u64>| {
                    let idx = bounds.partition_point(|b| *b <= pos);
                    let b_lo = if idx == 0 { 0 } else { bounds[idx - 1] };
                    ((r, obs.clone(), op, b_lo), Agg::unit(op, pos, value))
                },
            );
            let partials = units.reduce(|_k, input, output| {
                if let Some(agg) = fold_units(input) {
                    output.push((agg, 1isize));
                }
            });
            // Cumulative per (rollout, dim): the running combine over the
            // boundary-keyed partials, as one sorted vector.
            let cum = partials
                .map(|((r, obs, op, b_lo), agg)| ((r, obs, op), (b_lo, agg)))
                .reduce(|_k, input, output| {
                    let mut acc: Option<Agg> = None;
                    let mut vec: Vec<(u64, Agg)> = Vec::with_capacity(input.len());
                    for ((b_lo, agg), _w) in input.iter().map(|(v, w)| ((v.0, &v.1), *w)) {
                        let next = match acc {
                            Some(a) => a.combine(agg),
                            None => agg.clone(),
                        };
                        vec.push((b_lo, next.clone()));
                        acc = Some(next);
                    }
                    output.push((vec, 1isize));
                });
            let cum_by_rollout = cum.map(|((r, obs, op), vec)| (r, (obs, op, vec)));

            // Inherited start state: ancestor cumulative aggregates at the
            // lineage fork bounds, composed per rollout.
            let anc_by_a = anc.map(|(r, (a, u, d))| (a, (r, u, d)));
            let start_contrib = anc_by_a
                .join_map(
                    cum_by_rollout.clone(),
                    |&_a, &(r, u, _d), dv: &(ObsKey, ReduceOp, Vec<(u64, Agg)>)| {
                        ((r, dv.0.clone(), dv.1), lookup(&dv.2, u).cloned())
                    },
                )
                .flat_map(|(k, agg)| agg.map(|a| (k, a)));
            let start = start_contrib.reduce(|_k, input, output| {
                if let Some(agg) = fold_units(input) {
                    output.push((agg, 1isize));
                }
            });
            let start_by_rollout = start.map(|((r, obs, op), agg)| (r, (obs, op, agg)));

            let inherited = points.clone().join_map(
                start_by_rollout,
                |&r, &(point, _cut), da: &(ObsKey, ReduceOp, Agg)| {
                    ((r, point, da.0.clone()), da.2.clone())
                },
            );
            let own = points
                .clone()
                .join_map(
                    cum_by_rollout,
                    |&r, &(point, cut), dv: &(ObsKey, ReduceOp, Vec<(u64, Agg)>)| {
                        (
                            (r, point),
                            (dv.0.clone(), lookup(&dv.2, cut.count).cloned()),
                        )
                    },
                )
                .flat_map(|((r, point), (obs, agg))| agg.map(|a| ((r, point, obs), a)));
            let obs_view = inherited.concat(own).reduce(|_k, input, output| {
                if let Some(agg) = fold_units(input) {
                    output.push((agg.reduced(), 1isize));
                }
            });
            let obs_view = cap!(obs_view, views_in.observations);

            // -- Cells: the projection at every point (empty maps included
            // via the seed, which also carries the point's cut). ------------
            let cell_seed = points
                .clone()
                .map(|(r, (point, cut))| ((r, point), CellIn::Seed(cut)));
            let cell_obs =
                obs_view.map(|((r, point, obs), red)| ((r, point), CellIn::Obs(obs, red)));
            let cells = cell_seed
                .concat(cell_obs)
                .reduce(move |_k, input, output| {
                    // The seed sorts first; observation pairs follow in
                    // canonical (obs) order.
                    let mut cut = CutRow::default();
                    let mut pairs: Vec<(ObsKey, ReducedRow)> = Vec::new();
                    for (v, _w) in input {
                        match v {
                            CellIn::Seed(c) => cut = *c,
                            CellIn::Obs(obs, red) => pairs.push((obs.clone(), red.clone())),
                        }
                    }
                    output.push(((cut, proj(cut, &pairs)), 1isize));
                })
                .map(|((r, point), (cut, cell))| ((r, point, cut), cell));
            let cells = cap!(cells, views_in.cells);

            // -- Occupancy: the deterministic best-entry-per-cell reduction
            // over committed Entry offers at derived SEAL cells only
            // (provisional cuts are structurally unable to reach it). -------
            let seal_cells = cells.flat_map(|((r, point, _cut), cell)| match point {
                PointRow::Seal(seal) => Some(((r, seal), cell)),
                PointRow::Cut(_) => None,
            });
            let occ_in = entries.join_map(seal_cells, |&(_r, _s), &(quality, entry), cell| {
                (cell.clone(), (quality, entry))
            });
            let occupancy = occ_in.reduce(|_k, input, output| {
                let mut best: Option<(u64, EntryKey)> = None;
                for (v, _w) in input {
                    let (q, e) = (v.0, v.1);
                    best = Some(match best {
                        None => (q, e),
                        Some((bq, be)) => {
                            if q > bq || (q == bq && e < be) {
                                (q, e)
                            } else {
                                (bq, be)
                            }
                        }
                    });
                }
                if let Some((_q, e)) = best {
                    output.push((e, 1isize));
                }
            });
            let _occ = cap!(occupancy, views_in.occupancy);

            (
                input,
                RelationInputs {
                    declares: declares_in,
                    events: events_in,
                    lineage: lineage_in,
                    points: points_in,
                    entries: entries_in,
                },
            )
        });

        ProbeHost {
            worker,
            input,
            relations,
            probe,
            captured,
            views,
            epoch: 0,
            driven: 0,
        }
    }

    /// Submit one committed input at its revision. The caller (the
    /// coordinator's frontier machinery) guarantees `rev >= epoch` by only
    /// submitting the contiguous committed prefix in order.
    pub(crate) fn insert(&mut self, rev: u64, batch: [u8; 32]) {
        self.input.update_at((rev, batch), rev, 1);
    }

    /// Feed one committed batch's staged evidence rows at its revision, in
    /// deterministic field order. `declares` must already be deduplicated by
    /// the caller (fed exactly once per identity).
    pub(crate) fn feed_rows(&mut self, rev: u64, rows: &EvidenceRows) {
        for (obs, op) in &rows.declares {
            self.relations
                .declares
                .update_at((obs.clone(), *op), rev, 1);
        }
        if let Some(l) = &rows.lineage {
            self.relations
                .lineage
                .update_at((rows.rollout, (l.parent, l.cut.count)), rev, 1);
        }
        for e in &rows.events {
            self.relations
                .events
                .update_at((rows.rollout, e.clone()), rev, 1);
        }
        for cut in &rows.obs_cuts {
            self.relations.points.update_at(
                (rows.rollout, (PointRow::Cut(cut.count), *cut)),
                rev,
                1,
            );
        }
        if let Some(s) = &rows.seal {
            self.relations.points.update_at(
                (rows.rollout, (PointRow::Seal(s.seal), s.cut)),
                rev,
                1,
            );
            if let Some(e) = &rows.entry {
                self.relations.entries.update_at(
                    ((rows.rollout, s.seal), (e.quality, e.entry)),
                    rev,
                    1,
                );
            }
        }
    }

    /// Advance the input frontier to `to` (monotone; no-op if behind).
    pub(crate) fn advance(&mut self, to: u64) {
        if to > self.epoch {
            self.input.advance_to(to);
            self.input.flush();
            self.relations.advance_flush(to);
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
        if until > self.driven {
            self.driven = until;
        }
    }

    /// The exclusive watermark `drive` has passed: views are complete for
    /// every revision `< driven()`.
    pub(crate) fn driven(&self) -> u64 {
        self.driven
    }

    /// The consolidated, canonically ordered committed-input view at
    /// `visible` (inclusive): sum diffs for updates with time `<= visible`,
    /// drop zeros, sort. Only call after `drive` has passed `visible` — the
    /// probe-barrier read discipline.
    pub(crate) fn view(&self, visible: u64) -> Vec<Row> {
        // Statically infallible: one worker thread, no panic while held.
        let rows = self.captured.lock().expect("single-threaded capture lock");
        flat(&rows, visible)
    }

    /// The consolidated, canonically ordered materialized views at `visible`
    /// (inclusive). Only call after `drive` has passed `visible`.
    pub(crate) fn materialized(&self, visible: u64) -> MaterializedViews {
        // Statically infallible: one worker thread, no panic while held.
        let observations = {
            let rows = self
                .views
                .observations
                .lock()
                .expect("single-threaded capture lock");
            flat(&rows, visible)
        };
        let cells = {
            let rows = self
                .views
                .cells
                .lock()
                .expect("single-threaded capture lock");
            flat(&rows, visible)
        };
        let occupancy = {
            let rows = self
                .views
                .occupancy
                .lock()
                .expect("single-threaded capture lock");
            flat(&rows, visible)
        };
        MaterializedViews {
            observations,
            cells,
            occupancy,
        }
    }
}

/// Consolidate captured updates as of `visible` (sum diffs for updates with
/// time `<= visible`, drop zeros, canonically sort) and assert the canonical
/// unit-multiplicity read.
fn flat<T: Ord + Clone + std::fmt::Debug>(rows: &[(T, u64, isize)], visible: u64) -> Vec<T> {
    let mut net: BTreeMap<T, isize> = BTreeMap::new();
    for (data, time, diff) in rows {
        if *time <= visible {
            *net.entry(data.clone()).or_default() += *diff;
        }
    }
    net.into_iter()
        .filter(|(_, diff)| *diff != 0)
        .map(|(data, diff)| {
            debug_assert_eq!(diff, 1, "non-unit multiplicity for {data:?}");
            data
        })
        .collect()
}
