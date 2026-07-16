//! The Differential Dataflow program under test, and its revision-stepped
//! driver.
//!
//! Doctrine (ruled; violations blocking):
//! - **Branch is a key.** Rollout identity appears only in keys and data
//!   columns of one dataflow. Forks add rows, never dataflows or timestamps.
//! - **Revision is time; `Moment`/ordinal is data.** The one differential
//!   timestamp is the `u64` campaign revision at which an input update
//!   commits. Every V-time coordinate rides in the data.
//! - **No custom lattice.** The outer timestamp is `u64`; the only nested
//!   timestamp is the standard `Product<u64, u64>` inside `iterate`.
//!
//! Two formulations of point observation are built side by side:
//!
//! - **naive** — each evaluation point joins every ancestor-segment event and
//!   reduces; per-point cost is proportional to the full lineage prefix (the
//!   honest recompute-shaped baseline).
//! - **shared** — per-segment partial aggregates at boundary granularity,
//!   cumulative per rollout, ancestor contributions composed through the
//!   lineage (all combines commutative/associative); per-branch cost is
//!   proportional to its own segment plus depth, not prefix length. The two
//!   must agree exactly (and match the genesis-replay referee).
//!
//! Read discipline: every view is consolidated in-graph, captured with its
//! `(data, revision, diff)` updates, probed, and only read after the probe
//! has passed the submitted revision; readers consolidate and canonically
//! sort (`Captured::net`/`Captured::flat`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use differential_dataflow::input::{Input, InputSession};
use differential_dataflow::operators::iterate::Iterate;
use timely::dataflow::operators::probe::Handle as ProbeHandle;

use crate::data::{
    AbsRow, Agg, CellKey, CellRow, CfgId, Dim, Fixture, ObsOut, ObsRow, OccRow, OrderScope,
    Payload, PointId, Pos, PrefixEv, PrefixRow, PropRow, Revision, RolloutId, ScrapeRow,
    SeqPairRow, SeqRejRow, SiteRow, Species, Transition, TransRow, WorkRow, cell_fn,
};
use crate::generate::SplitMix64;

/// Which formulations to build (tests run both; the benchmark isolates them).
#[derive(Clone, Copy, Debug)]
pub struct BuildOpts {
    /// Build the naive per-point prefix-join formulation.
    pub naive: bool,
    /// Build the shared segment-aggregate formulation.
    pub shared: bool,
    /// Build the lineage-composed seal-prefix event view (family 1).
    pub prefix: bool,
}

impl Default for BuildOpts {
    fn default() -> Self {
        BuildOpts { naive: true, shared: true, prefix: true }
    }
}

/// Everything the run captured: per-view `(row, revision, diff)` updates plus
/// per-stage per-revision delta counts (the incrementality measure).
#[derive(Clone, Debug, Default)]
pub struct Captured {
    /// Family 1: lineage-complete prefixes at candidate seals.
    pub seal_prefix: Vec<(PrefixRow, Revision, isize)>,
    /// Observations, naive formulation.
    pub obs_naive: Vec<(ObsRow, Revision, isize)>,
    /// Observations, shared formulation.
    pub obs_shared: Vec<(ObsRow, Revision, isize)>,
    /// Cells at every evaluation point.
    pub cells: Vec<(CellRow, Revision, isize)>,
    /// Provisional transitions (replay nominations).
    pub transitions: Vec<(TransRow, Revision, isize)>,
    /// Archive occupancy.
    pub occupancy: Vec<(OccRow, Revision, isize)>,
    /// Property-level assertion aggregation.
    pub property_results: Vec<(PropRow, Revision, isize)>,
    /// Site coverage.
    pub site_coverage: Vec<(SiteRow, Revision, isize)>,
    /// Finalized absence expectations.
    pub absence: Vec<(AbsRow, Revision, isize)>,
    /// Bounded working-set species counts.
    pub working_species: Vec<(WorkRow, Revision, isize)>,
    /// Cross-source sequence pairs.
    pub seq_pairs: Vec<(SeqPairRow, Revision, isize)>,
    /// Cross-source sequence rejections.
    pub seq_rejections: Vec<(SeqRejRow, Revision, isize)>,
    /// Terminal scrape evidence.
    pub scrape_terminal: Vec<(ScrapeRow, Revision, isize)>,
    /// `(stage, revision) -> update count` for every metered stage.
    pub deltas: BTreeMap<(String, Revision), u64>,
}

impl Captured {
    /// Consolidate a captured view as of `rev`: sum diffs for updates with
    /// time `<= rev`, drop zeros, canonically sort.
    pub fn net<T: Ord + Clone>(rows: &[(T, Revision, isize)], rev: Revision) -> Vec<(T, isize)> {
        let mut acc: BTreeMap<T, isize> = BTreeMap::new();
        for (d, t, r) in rows {
            if *t <= rev {
                *acc.entry(d.clone()).or_default() += *r;
            }
        }
        acc.into_iter().filter(|(_, r)| *r != 0).collect()
    }

    /// As `net`, asserting every surviving row has multiplicity exactly one
    /// (the canonical-read discipline for set-like views).
    pub fn flat<T: Ord + Clone + std::fmt::Debug>(
        rows: &[(T, Revision, isize)],
        rev: Revision,
    ) -> Vec<T> {
        Self::net(rows, rev)
            .into_iter()
            .map(|(d, r)| {
                assert_eq!(r, 1, "non-unit multiplicity for {d:?}");
                d
            })
            .collect()
    }

    /// Total update count across revisions for stages whose name starts with
    /// `prefix`.
    pub fn delta_total(&self, prefix: &str) -> u64 {
        self.deltas
            .iter()
            .filter(|((n, _), _)| n.starts_with(prefix))
            .map(|(_, c)| *c)
            .sum()
    }

    /// Update count at one revision for stages whose name starts with
    /// `prefix`.
    pub fn delta_at(&self, prefix: &str, rev: Revision) -> u64 {
        self.deltas
            .iter()
            .filter(|((n, t), _)| n.starts_with(prefix) && *t == rev)
            .map(|(_, c)| *c)
            .sum()
    }
}

/// Greatest present cumulative entry strictly below `below` (partials are
/// keyed by their interval's lower boundary; a point's count is itself a
/// boundary, which is what makes this lookup exact).
fn lookup(cum: &[(Pos, Agg)], below: Pos) -> Option<&Agg> {
    cum.iter().rev().find(|(b, _)| *b < below).map(|(_, a)| a)
}

/// Fold a reduce input slice of `Agg` units into one aggregate. The reduce
/// contract guarantees a non-empty slice, so `None` never escapes.
fn fold_units(input: &[(&Agg, isize)]) -> Option<Agg> {
    let mut acc: Option<Agg> = None;
    for (agg, w) in input {
        let scaled = agg.scaled(*w as i64);
        acc = Some(match acc {
            Some(a) => a.combine(&scaled),
            None => scaled,
        });
    }
    acc
}

/// Run one fixture through the dataflow: one process, one worker, one
/// dataflow, time = revision. Input batches within a revision are fed in an
/// `order_seed`-shuffled order (net views must be invariant to it).
pub fn run(fixture: &Fixture, opts: BuildOpts, order_seed: u64) -> Captured {
    assert!(opts.naive || opts.shared, "at least one formulation must be built");
    let acc = Arc::new(Mutex::new(Captured::default()));
    let acc_in = Arc::clone(&acc);
    let fx = fixture.clone();

    timely::execute_directly(move |worker| {
        let probe = ProbeHandle::new();
        let probe_in = probe.clone();

        let mut inputs = worker.dataflow::<u64, _, _>(move |scope| {
            let probe = probe_in;

            macro_rules! cap {
                ($coll:expr, $name:expr, $field:ident) => {{
                    let a = Arc::clone(&acc_in);
                    $coll
                        .consolidate()
                        .inspect_batch(move |_t, batch| {
                            let mut g = a.lock().unwrap();
                            for (d, t, r) in batch {
                                *g.deltas.entry(($name.to_owned(), *t)).or_default() += 1;
                                g.$field.push((d.clone(), *t, *r));
                            }
                        })
                        .probe_with(&probe)
                }};
            }
            macro_rules! meter {
                ($coll:expr, $name:expr) => {{
                    let a = Arc::clone(&acc_in);
                    $coll.inspect_batch(move |_t, batch| {
                        let mut g = a.lock().unwrap();
                        for (_d, t, _r) in batch {
                            *g.deltas.entry(($name.to_owned(), *t)).or_default() += 1;
                        }
                    })
                }};
            }

            let (events_in, events) = scope.new_collection::<crate::data::SdkEventRec, isize>();
            let (scrape_in, scrape) = scope.new_collection::<crate::data::ScrapeLineRec, isize>();
            let (registers_in, registers) =
                scope.new_collection::<crate::data::RegisterDecl, isize>();
            let (sources_in, sources) = scope.new_collection::<crate::data::SourceDecl, isize>();
            let (properties_in, properties) =
                scope.new_collection::<crate::data::PropertyDecl, isize>();
            let (lineage_in, lineage) = scope.new_collection::<crate::data::LineageRec, isize>();
            let (obs_cuts_in, obs_cuts) = scope.new_collection::<crate::data::ObsCutRec, isize>();
            let (seals_in, seals) = scope.new_collection::<crate::data::SealRec, isize>();
            let (entry_commits_in, entry_commits) =
                scope.new_collection::<crate::data::EntryCommitRec, isize>();
            let (working_in, working) =
                scope.new_collection::<(CfgId, RolloutId, Pos), isize>();
            let (seq_queries_in, seq_queries) =
                scope.new_collection::<crate::data::SeqQueryRec, isize>();

            // -- Shared base arrangements (the in-process shared-arrangement
            // story: one arranged evidence index, many consumers). ----------
            let ev = events.clone().map(|e| ((e.config, e.rollout), (e.source, e.pos, e.moment, e.payload)));
            let ev_arr = ev.arrange_by_key_named("evidence-by-rollout");

            let reg_ev = events.clone().flat_map(|e| match e.payload {
                Payload::Register { reg, value } => {
                    Some(((e.config, reg), (e.rollout, e.pos, e.moment, value)))
                }
                _ => None,
            });
            let reg_ops = registers.map(|d| ((d.config, d.reg), d.op));
            let reg_measures =
                reg_ev.join_map(reg_ops, |&(cfg, reg), &(rollout, pos, moment, value), &op| {
                    let dim = Dim::Reg(reg, op);
                    ((cfg, rollout), (dim, pos, moment, value))
                });
            let note_measures = events.clone().flat_map(|e| match e.payload {
                Payload::Note { tag } => {
                    Some(((e.config, e.rollout), (Dim::Tag(tag), e.pos, e.moment, 0i64)))
                }
                _ => None,
            });
            let measures = meter!(reg_measures.concat(note_measures), "base.measures");
            let measures_arr = measures.arrange_by_key_named("measures-by-rollout");

            // Evaluation points: configured cuts, branch points, seals.
            let cut_points = obs_cuts
                .map(|c| ((c.config, c.rollout), (PointId::Cut(c.cut.count), c.cut.count)))
                .distinct();
            let fork_points = lineage.clone()
                .map(|l| ((l.config, l.parent), (PointId::Fork(l.cut.count), l.cut.count)))
                .distinct();
            let seal_points =
                seals.clone().map(|s| ((s.config, s.rollout), (PointId::Seal(s.seal), s.cut.count)));
            let points = cut_points.clone().concat(fork_points.clone()).concat(seal_points.clone());
            let points_arr = points.clone().arrange_by_key_named("points-by-rollout");

            // Ancestors: ((cfg, rollout), (ancestor, upper cut count, depth)),
            // by iteration over the lineage relation. The inner timestamp is
            // the standard Product<u64, u64> — no custom lattice.
            let edges = lineage.clone().map(|l| ((l.config, l.child), (l.parent, l.cut.count)));
            let anc = {
                let edges_c = edges.clone();
                let base = edges.map(|((cfg, r), (p, u))| ((cfg, r), (p, u, 1u32)));
                let base_c = base.clone();
                base.iterate(move |scope, inner| {
                    let edges = edges_c.enter(scope);
                    let base = base_c.enter(scope);
                    let step = inner
                        .clone()
                        .map(|((cfg, r), (a, _u, d))| ((cfg, a), (r, d)))
                        .join_map(edges, |&(cfg, _a), &(r, d), &(b, u2)| {
                            ((cfg, r), (b, u2, d + 1))
                        });
                    base.concat(step).distinct()
                })
            };
            let anc = meter!(anc, "lineage.anc");
            let anc_arr = anc.clone().arrange_by_key_named("ancestors-by-rollout");

            // -- Naive formulation ------------------------------------------
            let obs_naive = if opts.naive {
                let own = points_arr.clone().join_core(
                    measures_arr.clone(),
                    |&(cfg, r), &(point, count), &(dim, pos, moment, value)| {
                        (pos < count)
                            .then(|| ((cfg, r, point, dim), Agg::unit(&dim, pos, moment, value)))
                    },
                );
                let point_anc = points_arr.clone().join_core(
                    anc_arr.clone(),
                    |&(cfg, r), &(point, _count), &(a, u, _d)| Some(((cfg, a), (r, point, u))),
                );
                let anc_side = point_anc.join_core(
                    measures_arr.clone(),
                    |&(cfg, _a), &(r, point, u), &(dim, pos, moment, value)| {
                        (pos < u)
                            .then(|| ((cfg, r, point, dim), Agg::unit(&dim, pos, moment, value)))
                    },
                );
                let contrib = meter!(own.concat(anc_side), "naive.contrib");
                let obs = contrib.reduce(|_k, input, output| {
                    if let Some(agg) = fold_units(input) {
                        output.push((ObsOut::from_agg(&agg), 1isize));
                    }
                });
                Some(cap!(obs, "naive.obs", obs_naive))
            } else {
                None
            };

            // -- Shared formulation ------------------------------------------
            let obs_shared = if opts.shared {
                let breq = cut_points
                    .map(|((cfg, r), (_p, count))| ((cfg, r), count))
                    .concat(fork_points.map(|((cfg, r), (_p, count))| ((cfg, r), count)))
                    .concat(seal_points.map(|((cfg, r), (_p, count))| ((cfg, r), count)))
                    .distinct();
                let bounds = breq.reduce(|_k, input, output| {
                    // Values arrive sorted and distinct: the boundary vector.
                    let v: Vec<Pos> = input.iter().map(|(b, _)| **b).collect();
                    output.push((v, 1isize));
                });
                let bounds_arr = bounds.arrange_by_key_named("bounds-by-rollout");

                // Assign each measure to its interval's lower boundary.
                let units = measures_arr.clone().join_core(
                    bounds_arr.clone(),
                    |&(cfg, r), &(dim, pos, moment, value), bounds: &Vec<Pos>| {
                        let idx = bounds.partition_point(|b| *b <= pos);
                        let b_lo = if idx == 0 { 0 } else { bounds[idx - 1] };
                        Some(((cfg, r, dim, b_lo), Agg::unit(&dim, pos, moment, value)))
                    },
                );
                let partials = meter!(
                    units.reduce(|_k, input, output| {
                        if let Some(agg) = fold_units(input) {
                            output.push((agg, 1isize));
                        }
                    }),
                    "shared.partials"
                );
                let cum = partials
                    .map(|((cfg, r, dim, b_lo), agg)| ((cfg, r, dim), (b_lo, agg)))
                    .reduce(|_k, input, output| {
                        let mut acc: Option<Agg> = None;
                        let mut vec: Vec<(Pos, Agg)> = Vec::with_capacity(input.len());
                        for ((b_lo, agg), w) in
                            input.iter().map(|(v, w)| ((v.0, &v.1), *w))
                        {
                            let scaled = agg.scaled(w as i64);
                            let next = match acc {
                                Some(a) => a.combine(&scaled),
                                None => scaled,
                            };
                            vec.push((b_lo, next.clone()));
                            acc = Some(next);
                        }
                        output.push((vec, 1isize));
                    });
                let cum = meter!(cum, "shared.cum");
                let cum_by_rollout = cum.map(|((cfg, r, dim), vec)| ((cfg, r), (dim, vec)));
                let cum_arr = cum_by_rollout.arrange_by_key_named("cum-by-rollout");

                // Inherited start state: ancestor cumulative aggregates at
                // the lineage fork bounds, composed per rollout.
                let anc_by_a = anc.map(|((cfg, r), (a, u, d))| ((cfg, a), (r, u, d)));
                let start_contrib = anc_by_a.join_core(
                    cum_arr.clone(),
                    |&(cfg, _a), &(r, u, d), dv: &(Dim, Vec<(Pos, Agg)>)| {
                        lookup(&dv.1, u).map(|agg| ((cfg, r, dv.0), (d, agg.clone())))
                    },
                );
                let start = start_contrib.reduce(|_k, input, output| {
                    let mut acc: Option<Agg> = None;
                    for ((_d, agg), w) in input.iter().map(|(v, w)| ((v.0, &v.1), *w)) {
                        let scaled = agg.scaled(w as i64);
                        acc = Some(match acc {
                            Some(a) => a.combine(&scaled),
                            None => scaled,
                        });
                    }
                    if let Some(agg) = acc {
                        output.push((agg, 1isize));
                    }
                });
                let start = meter!(start, "shared.start");
                let start_by_rollout = start.map(|((cfg, r, dim), agg)| ((cfg, r), (dim, agg)));
                let start_arr = start_by_rollout.arrange_by_key_named("start-by-rollout");

                let inherited = points_arr.clone().join_core(
                    start_arr,
                    |&(cfg, r), &(point, _count), da: &(Dim, Agg)| {
                        Some(((cfg, r, point, da.0), da.1.clone()))
                    },
                );
                let own = points_arr.clone().join_core(
                    cum_arr.clone(),
                    |&(cfg, r), &(point, count), dv: &(Dim, Vec<(Pos, Agg)>)| {
                        lookup(&dv.1, count).map(|agg| ((cfg, r, point, dv.0), agg.clone()))
                    },
                );
                let obs = inherited.concat(own).reduce(|_k, input, output| {
                    if let Some(agg) = fold_units(input) {
                        output.push((ObsOut::from_agg(&agg), 1isize));
                    }
                });
                Some(cap!(obs, "shared.obs", obs_shared))
            } else {
                None
            };

            // -- Family 1: lineage-composed seal prefixes --------------------
            if opts.prefix {
                let seal_pts = seals.clone().map(|s| ((s.config, s.rollout), (s.seal, s.cut.count)));
                let own_pref = seal_pts.clone().join_core(
                    ev_arr.clone(),
                    |&(cfg, r), &(seal, count), ev: &(u32, Pos, u64, Payload)| {
                        (ev.1 < count).then(|| {
                            (
                                (cfg, r, seal),
                                PrefixEv {
                                    owner: r,
                                    source: ev.0,
                                    pos: ev.1,
                                    moment: ev.2,
                                    payload: ev.3.clone(),
                                },
                            )
                        })
                    },
                );
                let seal_anc = seal_pts.join_core(
                    anc_arr.clone(),
                    |&(cfg, r), &(seal, _count), &(a, u, _d)| Some(((cfg, a), (r, seal, u))),
                );
                let anc_pref = seal_anc.join_core(
                    ev_arr.clone(),
                    |&(cfg, a), &(r, seal, u), ev: &(u32, Pos, u64, Payload)| {
                        (ev.1 < u).then(|| {
                            (
                                (cfg, r, seal),
                                PrefixEv {
                                    owner: a,
                                    source: ev.0,
                                    pos: ev.1,
                                    moment: ev.2,
                                    payload: ev.3.clone(),
                                },
                            )
                        })
                    },
                );
                cap!(own_pref.concat(anc_pref), "prefix.events", seal_prefix);
            }

            // -- Cells, transitions, occupancy --------------------------------
            let obs_pref = obs_shared
                .clone()
                .or_else(|| obs_naive.clone())
                .expect("at least one formulation is built");
            let cell_seed =
                points.map(|((cfg, r), (point, _count))| ((cfg, r, point), None::<(Dim, ObsOut)>));
            let cell_obs =
                obs_pref.map(|((cfg, r, point, dim), out)| ((cfg, r, point), Some((dim, out))));
            let cells = cell_seed.concat(cell_obs).reduce(|_k, input, output| {
                let obs: Vec<(Dim, ObsOut)> =
                    input.iter().filter_map(|(v, _)| (*v).clone()).collect();
                output.push((cell_fn(&obs), 1isize));
            });
            let cells = cap!(cells, "cells", cells);

            let baseline = lineage
                .map(|l| ((l.config, l.parent, PointId::Fork(l.cut.count)), (l.child, l.cut.count)))
                .join_map(cells.clone(), |&(cfg, _parent, _point), &(child, count), cell| {
                    ((cfg, child), (0u8, count, cell.clone()))
                });
            let own_cut_cells = cells.clone().flat_map(|((cfg, r, point), cell)| match point {
                PointId::Cut(count) => Some(((cfg, r), (1u8, count, cell))),
                _ => None,
            });
            let transitions = baseline.concat(own_cut_cells).reduce(|_k, input, output| {
                // Sorted input: the (unique) baseline first, then cuts by
                // ascending count.
                let mut prev: Option<CellKey> = None;
                for (v, _w) in input {
                    let (tag, count, cell) = (v.0, v.1, &v.2);
                    if tag == 0 {
                        prev = Some(cell.clone());
                        continue;
                    }
                    if prev.as_ref() != Some(cell) {
                        output.push((
                            Transition { at_count: count, from: prev.clone(), to: cell.clone() },
                            1isize,
                        ));
                    }
                    prev = Some(cell.clone());
                }
            });
            cap!(transitions, "transitions", transitions);

            // Occupancy reduces committed entries at derived seal cells only;
            // provisional transitions are not an input by construction.
            let seal_cells = cells.flat_map(|((cfg, r, point), cell)| match point {
                PointId::Seal(seal) => Some(((cfg, r, seal), cell)),
                _ => None,
            });
            let commits =
                entry_commits.map(|c| ((c.config, c.rollout, c.seal), (c.quality, c.entry)));
            let occ_in = commits.join_map(seal_cells, |&(cfg, _r, _s), &(quality, entry), cell| {
                ((cfg, cell.clone()), (quality, entry))
            });
            let occupancy = occ_in.reduce(|_k, input, output| {
                let mut best: Option<(i64, u32)> = None;
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
            cap!(occupancy, "occupancy", occupancy);

            // -- Property-level aggregation (immutable ledger only) ----------
            let prop_ev = events.clone().flat_map(|e| match e.payload {
                Payload::Assertion { property, passed, .. } => {
                    Some(((e.config, property), passed))
                }
                _ => None,
            });
            let property_results = prop_ev.clone().reduce(|_k, input, output| {
                let mut pass = 0i64;
                let mut fail = 0i64;
                for (p, w) in input {
                    if **p {
                        pass += *w as i64;
                    } else {
                        fail += *w as i64;
                    }
                }
                output.push(((pass, fail), 1isize));
            });
            cap!(property_results, "property_results", property_results);

            let site_coverage = events.clone()
                .flat_map(|e| match e.payload {
                    Payload::Assertion { site, property, .. } => {
                        Some((e.config, property, site))
                    }
                    _ => None,
                })
                .count()
                .map(|(k, n)| (k, n as i64));
            cap!(site_coverage, "site_coverage", site_coverage);

            let satisfied = prop_ev.flat_map(|(k, passed)| passed.then_some(k)).distinct();
            let declared = properties.flat_map(|p| p.must_hit.then_some(((p.config, p.property), ())));
            let absence = declared.antijoin(satisfied).map(|(k, ())| k);
            cap!(absence, "absence", absence);

            // -- Bounded working membership -----------------------------------
            let ev_coord = events.clone().map(|e| ((e.config, e.rollout, e.pos), Species::of(&e.payload)));
            let working_species = working
                .map(|k| (k, ()))
                .join_map(ev_coord, |&(cfg, _r, _p), &(), &species| (cfg, species))
                .count()
                .map(|(k, n)| (k, n as i64));
            cap!(working_species, "working_species", working_species);

            // -- Cross-source sequences ---------------------------------------
            let scopes = sources.map(|s| ((s.config, s.source), s.scope));
            let q_scoped = seq_queries
                .map(|q| ((q.config, q.src_a), (q.query, q.src_b)))
                .join_map(scopes.clone(), |&(cfg, src_a), &(query, src_b), &scope_a| {
                    ((cfg, src_b), (query, src_a, scope_a))
                })
                .join_map(scopes, |&(cfg, src_b), &(query, src_a, scope_a), &scope_b| {
                    ((cfg, query), (src_a, scope_a, src_b, scope_b))
                });
            let eligible = q_scoped.clone().filter(|&(_, (_, sa, _, sb))| {
                sa == OrderScope::RolloutGlobal && sb == OrderScope::RolloutGlobal
            });
            let seq_rejections = q_scoped
                .flat_map(|((cfg, query), (src_a, scope_a, src_b, scope_b))| {
                    let mut out = Vec::new();
                    if scope_a != OrderScope::RolloutGlobal {
                        out.push(((cfg, query), src_a));
                    }
                    if scope_b != OrderScope::RolloutGlobal {
                        out.push(((cfg, query), src_b));
                    }
                    out
                })
                .distinct();
            cap!(seq_rejections, "seq_rejections", seq_rejections);

            let notes_by_src = events.flat_map(|e| match e.payload {
                Payload::Note { tag } => {
                    Some(((e.config, e.source), (e.rollout, e.pos, e.moment, tag)))
                }
                _ => None,
            });
            let seq_pairs = eligible
                .map(|((cfg, query), (src_a, _sa, src_b, _sb))| ((cfg, src_a), (query, src_b)))
                .join_map(
                    notes_by_src.clone(),
                    |&(cfg, _sa), &(query, src_b), &(rollout, pos, moment, tag)| {
                        ((cfg, src_b), (query, rollout, pos, moment, tag))
                    },
                )
                .join(notes_by_src)
                .flat_map(|((cfg, _sb), ((query, ra, pa, ma, ta), (rb, pb, mb, tb)))| {
                    (ra == rb && pa < pb)
                        .then_some(((cfg, query, ra), ((pa, ma, ta), (pb, mb, tb))))
                });
            cap!(seq_pairs, "seq_pairs", seq_pairs);

            // -- Terminal scrape evidence -------------------------------------
            let scrape_terminal = scrape.map(|s| ((s.config, s.rollout), (s.local_ord, s.tag)));
            cap!(scrape_terminal, "scrape_terminal", scrape_terminal);

            Inputs {
                events: events_in,
                scrape: scrape_in,
                registers: registers_in,
                sources: sources_in,
                properties: properties_in,
                lineage: lineage_in,
                obs_cuts: obs_cuts_in,
                seals: seals_in,
                entry_commits: entry_commits_in,
                working: working_in,
                seq_queries: seq_queries_in,
            }
        });

        // Feed revision by revision; within a revision, each record class is
        // shuffled by the order seed (net views must not care).
        let mut rng = SplitMix64(order_seed ^ 0xC0FF_EE11_D00D_5EED);
        let max_rev = fx.max_rev();
        for rev in 0..=max_rev {
            feed_rev(&mut inputs, &fx, rev, &mut rng);
            inputs.advance_flush(rev + 1);
            worker.step_while(|| probe.less_than(&(rev + 1)));
        }
    });

    match Arc::try_unwrap(acc) {
        Ok(m) => m.into_inner().expect("no poisoned lock: single-threaded run"),
        Err(arc) => arc.lock().expect("no poisoned lock: single-threaded run").clone(),
    }
}

struct Inputs {
    events: InputSession<u64, crate::data::SdkEventRec, isize>,
    scrape: InputSession<u64, crate::data::ScrapeLineRec, isize>,
    registers: InputSession<u64, crate::data::RegisterDecl, isize>,
    sources: InputSession<u64, crate::data::SourceDecl, isize>,
    properties: InputSession<u64, crate::data::PropertyDecl, isize>,
    lineage: InputSession<u64, crate::data::LineageRec, isize>,
    obs_cuts: InputSession<u64, crate::data::ObsCutRec, isize>,
    seals: InputSession<u64, crate::data::SealRec, isize>,
    entry_commits: InputSession<u64, crate::data::EntryCommitRec, isize>,
    working: InputSession<u64, (CfgId, RolloutId, Pos), isize>,
    seq_queries: InputSession<u64, crate::data::SeqQueryRec, isize>,
}

impl Inputs {
    fn advance_flush(&mut self, to: Revision) {
        self.events.advance_to(to);
        self.scrape.advance_to(to);
        self.registers.advance_to(to);
        self.sources.advance_to(to);
        self.properties.advance_to(to);
        self.lineage.advance_to(to);
        self.obs_cuts.advance_to(to);
        self.seals.advance_to(to);
        self.entry_commits.advance_to(to);
        self.working.advance_to(to);
        self.seq_queries.advance_to(to);
        self.events.flush();
        self.scrape.flush();
        self.registers.flush();
        self.sources.flush();
        self.properties.flush();
        self.lineage.flush();
        self.obs_cuts.flush();
        self.seals.flush();
        self.entry_commits.flush();
        self.working.flush();
        self.seq_queries.flush();
    }
}

fn shuffled<T: Clone>(items: impl Iterator<Item = T>, rng: &mut SplitMix64) -> Vec<T> {
    let mut v: Vec<T> = items.collect();
    let n = v.len();
    for i in (1..n).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        v.swap(i, j);
    }
    v
}

fn feed_rev(inputs: &mut Inputs, fx: &Fixture, rev: Revision, rng: &mut SplitMix64) {
    for r in shuffled(fx.events.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.events.update_at(r, rev, 1);
    }
    for r in shuffled(fx.scrape.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.scrape.update_at(r, rev, 1);
    }
    for r in shuffled(fx.registers.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.registers.update_at(r, rev, 1);
    }
    for r in shuffled(fx.sources.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.sources.update_at(r, rev, 1);
    }
    for r in shuffled(fx.properties.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.properties.update_at(r, rev, 1);
    }
    for r in shuffled(fx.lineage.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.lineage.update_at(r, rev, 1);
    }
    for r in shuffled(fx.obs_cuts.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.obs_cuts.update_at(r, rev, 1);
    }
    for r in shuffled(fx.seals.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.seals.update_at(r, rev, 1);
    }
    for r in shuffled(fx.entry_commits.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.entry_commits.update_at(r, rev, 1);
    }
    for r in shuffled(fx.working.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.working.update_at((r.config, r.rollout, r.pos), rev, r.delta as isize);
    }
    for r in shuffled(fx.seq_queries.iter().filter(|r| r.rev == rev).cloned(), rng) {
        inputs.seq_queries.update_at(r, rev, 1);
    }
}
