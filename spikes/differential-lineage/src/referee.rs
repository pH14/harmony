// SPDX-License-Identifier: AGPL-3.0-or-later
//! The direct-recompute referee: every view computed in plain Rust from the
//! genesis-complete replay vectors (the semantic oracle the strategy names —
//! "replaying the genesis-complete reproducer is the semantic oracle; cached
//! ancestor materializations are acceleration and must produce the same
//! consolidated multiset and the same canonically sorted projection").
//!
//! Every function takes an explicit revision and sees only records committed
//! at or before it, mirroring what the dataflow exposes once its probe has
//! passed that revision.

use std::collections::BTreeMap;

use crate::data::{
    AbsRow, Agg, CellKey, CellRow, CfgId, Dim, Fixture, ObsOut, ObsRow, OccRow, OrderScope,
    Payload, PointId, Pos, PrefixEv, PrefixRow, PropRow, ReduceOp, RegId, Replay, Revision,
    RolloutId, ScrapeRow, SdkEventRec, SeqPairRow, SeqRejRow, SiteRow, Species, TransRow,
    Transition, ValidationError, WorkRow, cell_fn,
};

/// The referee over one fixture and its replay authority.
pub struct Referee<'a> {
    fixture: &'a Fixture,
    replay: &'a Replay,
}

impl<'a> Referee<'a> {
    /// New referee. Refuses (with a typed error, never a panic) a fixture
    /// that fails [`Fixture::validate`] or a replay whose vectors do not
    /// cover the fixture's cuts — every slicing operation below is guarded
    /// by these checks.
    pub fn new(fixture: &'a Fixture, replay: &'a Replay) -> Result<Referee<'a>, ValidationError> {
        fixture.validate()?;
        let covered =
            |config: CfgId, rollout: RolloutId, count: Pos| -> Result<(), ValidationError> {
                if count as usize <= replay.vector(config, rollout).len() {
                    Ok(())
                } else {
                    Err(ValidationError::ReplayTooShort { rollout, count })
                }
            };
        for s in &fixture.seals {
            covered(s.config, s.rollout, s.cut.count)?;
        }
        for c in &fixture.obs_cuts {
            covered(c.config, c.rollout, c.cut.count)?;
        }
        for l in &fixture.lineage {
            // Both sides of the branch point are sliced: the child's vector
            // (its inherited prefix) and the PARENT's vector (the referee
            // evaluates Fork points on the parent at the fork count — r3:
            // this side was sliced before being validated).
            covered(l.config, l.child, l.cut.count)?;
            covered(l.config, l.parent, l.cut.count)?;
        }
        Ok(Referee { fixture, replay })
    }

    /// The replay evidence covered by a cut at `count`, as of `rev`: the
    /// half-open position prefix, filtered to records already committed. A
    /// covered event whose record commits later (legal in a fixture, though
    /// the production durable-append-before-submit rule forbids it) is not
    /// yet evidence at earlier revisions — exactly what the dataflow sees.
    fn covered_prefix(
        &self,
        config: CfgId,
        rollout: RolloutId,
        count: Pos,
        rev: Revision,
    ) -> Vec<&SdkEventRec> {
        self.replay.vector(config, rollout)[..count as usize]
            .iter()
            .filter(|e| e.rev <= rev)
            .collect()
    }

    fn reg_ops(&self, rev: Revision) -> BTreeMap<(CfgId, RegId), ReduceOp> {
        self.fixture
            .registers
            .iter()
            .filter(|d| d.rev <= rev)
            .map(|d| ((d.config, d.reg), d.op))
            .collect()
    }

    /// All evaluation points committed by `rev`:
    /// `(config, rollout, point, count)`, deduplicated.
    fn points(&self, rev: Revision) -> Vec<(CfgId, RolloutId, PointId, Pos)> {
        let mut pts = Vec::new();
        for c in self.fixture.obs_cuts.iter().filter(|c| c.rev <= rev) {
            pts.push((c.config, c.rollout, PointId::Cut(c.cut.count), c.cut.count));
        }
        for l in self.fixture.lineage.iter().filter(|l| l.rev <= rev) {
            pts.push((l.config, l.parent, PointId::Fork(l.cut.count), l.cut.count));
        }
        for s in self.fixture.seals.iter().filter(|s| s.rev <= rev) {
            pts.push((s.config, s.rollout, PointId::Seal(s.seal), s.cut.count));
        }
        pts.sort_unstable();
        pts.dedup();
        pts
    }

    /// Family 1: lineage-complete prefixes at candidate seals, straight from
    /// the genesis replay vector.
    pub fn seal_prefix(&self, rev: Revision) -> Vec<PrefixRow> {
        let mut rows = Vec::new();
        for s in self.fixture.seals.iter().filter(|s| s.rev <= rev) {
            for e in self.covered_prefix(s.config, s.rollout, s.cut.count, rev) {
                rows.push((
                    (s.config, s.rollout, s.seal),
                    PrefixEv {
                        owner: e.rollout,
                        source: e.source,
                        pos: e.pos,
                        moment: e.moment,
                        payload: e.payload.clone(),
                    },
                ));
            }
        }
        rows.sort_unstable();
        rows
    }

    fn fold_obs(
        &self,
        ops: &BTreeMap<(CfgId, RegId), ReduceOp>,
        config: CfgId,
        prefix: &[&SdkEventRec],
    ) -> Vec<(Dim, ObsOut)> {
        let mut aggs: BTreeMap<Dim, Agg> = BTreeMap::new();
        for e in prefix {
            let (dim, unit) = match &e.payload {
                Payload::Register { reg, value } => {
                    let Some(op) = ops.get(&(config, *reg)) else {
                        // Undeclared register: evidence, not reducible state.
                        continue;
                    };
                    let dim = Dim::Reg(*reg, *op);
                    (dim, Agg::unit(&dim, e.pos, e.moment, *value))
                }
                Payload::Note { tag } => {
                    let dim = Dim::Tag(*tag);
                    (dim, Agg::unit(&dim, e.pos, e.moment, 0))
                }
                Payload::Assertion { .. } => continue,
            };
            aggs.entry(dim)
                .and_modify(|a| *a = a.combine(&unit))
                .or_insert(unit);
        }
        aggs.iter()
            .map(|(d, a)| (*d, ObsOut::from_agg(a)))
            .collect()
    }

    /// Family 6: reduced and derived observations at every evaluation point.
    pub fn obs(&self, rev: Revision) -> Vec<ObsRow> {
        let ops = self.reg_ops(rev);
        let mut rows = Vec::new();
        for (config, rollout, point, count) in self.points(rev) {
            let prefix = self.covered_prefix(config, rollout, count, rev);
            for (dim, out) in self.fold_obs(&ops, config, &prefix) {
                rows.push(((config, rollout, point, dim), out));
            }
        }
        rows.sort_unstable();
        rows
    }

    /// Cells at every evaluation point (empty prefixes yield the empty cell).
    pub fn cells(&self, rev: Revision) -> Vec<CellRow> {
        let ops = self.reg_ops(rev);
        let mut rows = Vec::new();
        for (config, rollout, point, count) in self.points(rev) {
            let prefix = self.covered_prefix(config, rollout, count, rev);
            let obs = self.fold_obs(&ops, config, &prefix);
            rows.push(((config, rollout, point), cell_fn(&obs)));
        }
        rows.sort_unstable();
        rows
    }

    /// Family 2 (first pass): provisional transitions at configured unsealed
    /// cuts, baselined at the inherited branch-point cell.
    pub fn transitions(&self, rev: Revision) -> Vec<TransRow> {
        let ops = self.reg_ops(rev);
        let mut per_rollout: BTreeMap<(CfgId, RolloutId), Vec<Pos>> = BTreeMap::new();
        for c in self.fixture.obs_cuts.iter().filter(|c| c.rev <= rev) {
            per_rollout
                .entry((c.config, c.rollout))
                .or_default()
                .push(c.cut.count);
        }
        let mut rows = Vec::new();
        for ((config, rollout), mut counts) in per_rollout {
            counts.sort_unstable();
            counts.dedup();
            let baseline = self
                .fixture
                .lineage
                .iter()
                .filter(|l| l.rev <= rev)
                .find(|l| l.config == config && l.child == rollout)
                .map(|l| {
                    let prefix = self.covered_prefix(config, rollout, l.cut.count, rev);
                    cell_fn(&self.fold_obs(&ops, config, &prefix))
                });
            let mut prev = baseline;
            for count in counts {
                let prefix = self.covered_prefix(config, rollout, count, rev);
                let cell = cell_fn(&self.fold_obs(&ops, config, &prefix));
                if prev.as_ref() != Some(&cell) {
                    rows.push((
                        (config, rollout),
                        Transition {
                            at_count: count,
                            from: prev.clone(),
                            to: cell.clone(),
                        },
                    ));
                }
                prev = Some(cell);
            }
        }
        rows.sort_unstable();
        rows
    }

    /// Archive occupancy: deterministic best entry per `(config, cell)` over
    /// committed entries at candidate seals — quality descending, entry id
    /// ascending as the stable tie-break. Provisional transitions are not an
    /// input by construction.
    pub fn occupancy(&self, rev: Revision) -> Vec<OccRow> {
        let cells: BTreeMap<(CfgId, RolloutId, PointId), CellKey> =
            self.cells(rev).into_iter().collect();
        let mut best: BTreeMap<(CfgId, CellKey), (i64, EntryOrd)> = BTreeMap::new();
        for c in self.fixture.entry_commits.iter().filter(|c| c.rev <= rev) {
            let Some(cell) = cells.get(&(c.config, c.rollout, PointId::Seal(c.seal))) else {
                // Seal not committed yet: the commit is not yet joinable.
                continue;
            };
            let cand = (c.quality, EntryOrd(c.entry));
            best.entry((c.config, cell.clone()))
                .and_modify(|b| {
                    if cand > *b {
                        *b = cand;
                    }
                })
                .or_insert(cand);
        }
        best.into_iter()
            .map(|((config, cell), (_, e))| ((config, cell), e.0))
            .collect()
    }

    /// Family 7: property-level aggregation over the immutable ledger.
    pub fn property_results(&self, rev: Revision) -> Vec<PropRow> {
        let mut counts: BTreeMap<(CfgId, u32), (i64, i64)> = BTreeMap::new();
        for e in self.fixture.events.iter().filter(|e| e.rev <= rev) {
            if let Payload::Assertion {
                property, passed, ..
            } = &e.payload
            {
                let c = counts.entry((e.config, *property)).or_default();
                if *passed {
                    c.0 += 1;
                } else {
                    c.1 += 1;
                }
            }
        }
        counts.into_iter().collect()
    }

    /// Site coverage, separate from property verdicts.
    pub fn site_coverage(&self, rev: Revision) -> Vec<SiteRow> {
        let mut counts: BTreeMap<(CfgId, u32, u32), i64> = BTreeMap::new();
        for e in self.fixture.events.iter().filter(|e| e.rev <= rev) {
            if let Payload::Assertion { site, property, .. } = &e.payload {
                *counts.entry((e.config, *property, *site)).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }

    /// Finalized absence expectations: declared `must_hit` properties with no
    /// satisfying evaluation, derivable only once the campaign has explicitly
    /// finalized. Reads the immutable ledger only.
    pub fn absence(&self, rev: Revision) -> Vec<AbsRow> {
        let finalized: std::collections::BTreeSet<CfgId> = self
            .fixture
            .finalizations
            .iter()
            .filter(|f| f.rev <= rev)
            .map(|f| f.config)
            .collect();
        let satisfied: Vec<(CfgId, u32)> = self
            .fixture
            .events
            .iter()
            .filter(|e| e.rev <= rev)
            .filter_map(|e| match &e.payload {
                Payload::Assertion {
                    property,
                    passed: true,
                    ..
                } => Some((e.config, *property)),
                _ => None,
            })
            .collect();
        let mut rows: Vec<AbsRow> = self
            .fixture
            .properties
            .iter()
            .filter(|p| p.rev <= rev && p.must_hit)
            .filter(|p| finalized.contains(&p.config))
            .filter(|p| !satisfied.contains(&(p.config, p.property)))
            .map(|p| (p.config, p.property))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// Family 8: bounded working-set species counts under net membership.
    pub fn working_species(&self, rev: Revision) -> Vec<WorkRow> {
        let mut net: BTreeMap<(CfgId, RolloutId, Pos), i64> = BTreeMap::new();
        for w in self.fixture.working.iter().filter(|w| w.rev <= rev) {
            *net.entry((w.config, w.rollout, w.pos)).or_default() += w.delta;
        }
        let mut counts: BTreeMap<(CfgId, Species), i64> = BTreeMap::new();
        for e in self.fixture.events.iter().filter(|e| e.rev <= rev) {
            let n = net.get(&(e.config, e.rollout, e.pos)).copied().unwrap_or(0);
            if n != 0 {
                *counts
                    .entry((e.config, Species::of(&e.payload)))
                    .or_default() += n;
            }
        }
        counts.into_iter().filter(|(_, n)| *n != 0).collect()
    }

    /// Cross-source sequence pairs for eligible (rollout-global × rollout-
    /// global) queries, per owning-rollout segment, over `Note` events.
    pub fn seq_pairs(&self, rev: Revision) -> Vec<SeqPairRow> {
        let scopes: BTreeMap<(CfgId, u32), OrderScope> = self
            .fixture
            .sources
            .iter()
            .filter(|s| s.rev <= rev)
            .map(|s| ((s.config, s.source), s.scope))
            .collect();
        let mut rows = Vec::new();
        for q in self.fixture.seq_queries.iter().filter(|q| q.rev <= rev) {
            let ok = |src: u32| scopes.get(&(q.config, src)) == Some(&OrderScope::RolloutGlobal);
            if !ok(q.src_a) || !ok(q.src_b) {
                continue;
            }
            let notes = |src: u32| -> Vec<(RolloutId, Pos, u64, u32)> {
                self.fixture
                    .events
                    .iter()
                    .filter(|e| e.rev <= rev && e.config == q.config && e.source == src)
                    .filter_map(|e| match &e.payload {
                        Payload::Note { tag } => Some((e.rollout, e.pos, e.moment, *tag)),
                        _ => None,
                    })
                    .collect()
            };
            for (ra, pa, ma, ta) in notes(q.src_a) {
                for (rb, pb, mb, tb) in notes(q.src_b) {
                    if ra == rb && pa < pb {
                        rows.push(((q.config, q.query, ra), ((pa, ma, ta), (pb, mb, tb))));
                    }
                }
            }
        }
        rows.sort_unstable();
        rows
    }

    /// Rejections: queries naming a source without rollout-global order.
    pub fn seq_rejections(&self, rev: Revision) -> Vec<SeqRejRow> {
        let scopes: BTreeMap<(CfgId, u32), OrderScope> = self
            .fixture
            .sources
            .iter()
            .filter(|s| s.rev <= rev)
            .map(|s| ((s.config, s.source), s.scope))
            .collect();
        let mut rows = Vec::new();
        for q in self.fixture.seq_queries.iter().filter(|q| q.rev <= rev) {
            for src in [q.src_a, q.src_b] {
                if scopes.get(&(q.config, src)) != Some(&OrderScope::RolloutGlobal) {
                    rows.push(((q.config, q.query), src));
                }
            }
        }
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// Terminal scrape evidence (source-local, stop-granular).
    pub fn scrape_terminal(&self, rev: Revision) -> Vec<ScrapeRow> {
        let mut rows: Vec<ScrapeRow> = self
            .fixture
            .scrape
            .iter()
            .filter(|s| s.rev <= rev)
            .map(|s| ((s.config, s.rollout), (s.local_ord, s.tag)))
            .collect();
        rows.sort_unstable();
        rows
    }
}

/// Entry ids tie-break ascending at equal quality: wrap so that the maximum
/// of `(quality, EntryOrd(entry))` picks the LOWEST entry id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EntryOrd(u32);

impl Ord for EntryOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.cmp(&self.0)
    }
}

impl PartialOrd for EntryOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
