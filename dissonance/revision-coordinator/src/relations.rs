// SPDX-License-Identifier: AGPL-3.0-or-later
//! The production relation vocabulary (task 132, `hm-e6q`): the typed
//! evidence rows a campaign controller stages against a committed batch, and
//! the materialized views the in-process Differential dataflow derives from
//! them.
//!
//! The coordinator stays payload-blind (it never decodes SDK bytes — the
//! evidence ledger and its decoders live above, in the campaign controller):
//! observation identities arrive as **opaque canonical bytes** ([`ObsKey`])
//! and cells leave as **opaque canonical bytes** ([`CellBytes`]). What the
//! dataflow owns is the *temporal semantics*: lineage-composed evidence
//! prefixes, per-observation `set`/`max`/`min`/`accumulate` reduction at
//! half-open evidence cuts, the cell projection at each evaluation point, and
//! the deterministic best-entry-per-cell occupancy reduction — the
//! `spikes/differential-lineage` relations, productionized.
//!
//! Doctrine (ruled; violations blocking): branch/rollout identity is a key,
//! `Revision` is the ONLY timestamp, `Moment`/position are data columns, and
//! no custom lattice exists (the one nested timestamp is the standard
//! `Product<u64, u64>` inside the ancestry iteration).

use serde::{Deserialize, Serialize};

/// Opaque canonical observation-identity bytes. The campaign controller owns
/// the encoding (it is the same canonical encoding its cell projection
/// consumes); the dataflow only requires it to be stable and totally ordered
/// as bytes.
pub type ObsKey = Vec<u8>;

/// Opaque canonical cell-key bytes (the cell projection's output).
pub type CellBytes = Vec<u8>;

/// A deterministic rollout identity (the campaign-seeded issue index).
pub type RolloutKey = u64;

/// A candidate-seal identity (the campaign's issue index for the seal's
/// proposal).
pub type SealKey = u64;

/// A retained-entry identity (stable; lower id wins occupancy ties).
pub type EntryKey = u64;

/// The base update operation a state observation declares (the normalized
/// `SdkSchema` semantics, mirrored payload-blind — conventions rule 2).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum ReduceOp {
    /// The latest value at or before the queried cut is current.
    Set,
    /// The greatest value observed so far.
    Max,
    /// The least value observed so far.
    Min,
    /// The set of distinct values observed so far.
    Accumulate,
}

/// An evidence cut: **half-open by included count** — persisted positions
/// strictly less than `count` participate, including the exact subset emitted
/// at the cut's `moment`. `count` is the authority; `moment` rides as data.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct CutRow {
    /// The synchronized cut `Moment` (a V-time data column, never a
    /// timestamp).
    pub moment: u64,
    /// The included evidence count: the persisted vector's prefix length at
    /// the cut, **cumulative through restored ancestor prefixes**.
    pub count: u64,
}

/// One persisted state-observation event. Positions are **cumulative through
/// the lineage**: a child rollout persists only positions at or after its
/// parent cut's count; the restored ancestor prefix is inherited through
/// [`LineageRow`], never re-submitted as child evidence.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct StateEventRow {
    /// Persisted vector position (cumulative; unique per rollout).
    pub pos: u64,
    /// V-time coordinate (nondecreasing in `pos` within a rollout).
    pub moment: u64,
    /// The observation identity this event updates.
    pub obs: ObsKey,
    /// The update value (bounded integer; no floating point, rule 4).
    pub value: u64,
}

/// A rollout's branch lineage: it was branched from `parent` at `cut`. The
/// authority for evidence-prefix composition.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct LineageRow {
    /// The parent rollout.
    pub parent: RolloutKey,
    /// The branch-point cut on the parent's evidence vector.
    pub cut: CutRow,
}

/// A candidate seal: the second-pass, physically-held coordinate produced by
/// materialization replay, entering at a later revision than its rollout's
/// evidence (the two-barrier protocol).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct SealRow {
    /// The seal identity.
    pub seal: SealKey,
    /// The seal's server-stamped evidence cut.
    pub cut: CutRow,
}

/// A committed Entry offer at a seal: occupancy reduces these (and only
/// these — a provisional cut is structurally unable to reach occupancy) into
/// the deterministic best-entry-per-cell view.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct EntryCommitRow {
    /// The entry identity (stable tie-break: at equal quality the LOWER id
    /// wins, which is always the earlier offer — first-wins).
    pub entry: EntryKey,
    /// The versioned quality datum (greater dominates).
    pub quality: u64,
}

/// The typed evidence rows one committed batch contributes to the production
/// relations. Staged by the campaign controller
/// ([`Coordinator::stage_evidence`](crate::Coordinator::stage_evidence))
/// before its batch drains; fed to the dataflow at the batch's committed
/// revision, in deterministic field order.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct EvidenceRows {
    /// The owning rollout.
    pub rollout: RolloutKey,
    /// The rollout's branch lineage, on its first (rollout) batch.
    pub lineage: Option<LineageRow>,
    /// Observation declarations `(identity, base op)`. Idempotent per
    /// identity: re-declaring the same `(obs, op)` pair is absorbed;
    /// re-declaring an identity under a **different** op is refused at
    /// staging (a schema conflict is a determinism violation, surfaced).
    pub declares: Vec<(ObsKey, ReduceOp)>,
    /// The rollout's own persisted state events (child suffix only).
    pub events: Vec<StateEventRow>,
    /// Provisional (first-pass) evidence cuts to evaluate observations and
    /// cells at. Nomination coordinates only — never occupancy inputs.
    pub obs_cuts: Vec<CutRow>,
    /// The candidate seal this batch records, if it is a seal batch.
    pub seal: Option<SealRow>,
    /// The committed Entry offer riding the seal, if any.
    pub entry: Option<EntryCommitRow>,
}

/// An evaluation point on a rollout's evidence vector, as the views key it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum PointRow {
    /// A provisional (first-pass) cut, keyed by its included count.
    Cut(u64),
    /// A candidate seal (second pass; the only occupancy-eligible point).
    Seal(SealKey),
}

/// A reduced observation value at a point. `Scalar` covers `set`/`max`/`min`
/// (each collapses to one integer); `Accumulated` is the sorted distinct
/// value set of an `accumulate` observation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum ReducedRow {
    /// One reduced integer.
    Scalar(u64),
    /// The sorted, deduplicated set of observed values.
    Accumulated(Vec<u64>),
}

/// The consolidated, canonically ordered materialized views at a revision
/// frontier, read only after the probe barrier
/// ([`Coordinator::materialized`](crate::Coordinator::materialized)).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct MaterializedViews {
    /// Every reduced observation at every evaluation point:
    /// `((rollout, point, obs), reduced)`, canonically ordered.
    pub observations: Vec<((RolloutKey, PointRow, ObsKey), ReducedRow)>,
    /// The cell at every evaluation point: `((rollout, point, cut), cell)`,
    /// canonically ordered. The point's cut rides as data so readers keep
    /// the moment/count coordinate without a second join.
    pub cells: Vec<((RolloutKey, PointRow, CutRow), CellBytes)>,
    /// Archive occupancy: the best entry per cell, `(cell, entry)`,
    /// canonically ordered.
    pub occupancy: Vec<(CellBytes, EntryKey)>,
}

impl MaterializedViews {
    /// The cell at one evaluation point, if materialized.
    pub fn cell_at(&self, rollout: RolloutKey, point: PointRow) -> Option<&CellBytes> {
        self.cells
            .iter()
            .find(|((r, p, _), _)| *r == rollout && *p == point)
            .map(|(_, c)| c)
    }

    /// The occupant of one cell, if any.
    pub fn occupant(&self, cell: &[u8]) -> Option<EntryKey> {
        self.occupancy
            .iter()
            .find(|(c, _)| c.as_slice() == cell)
            .map(|(_, e)| *e)
    }
}

/// The pure cell projection the dataflow evaluates at each point: from the
/// point's cut and its complete reduced observation map (canonically ordered
/// `(identity, value)` pairs) to opaque cell bytes. Installed by the campaign
/// controller ([`Coordinator::set_cell_projection`](crate::Coordinator::set_cell_projection));
/// must be a pure function of its arguments (no interior state, wall clock,
/// or host entropy) — the determinism contract.
pub type CellProjection = std::rc::Rc<dyn Fn(CutRow, &[(ObsKey, ReducedRow)]) -> CellBytes>;

/// The default canonical cell projection: the byte concatenation of each
/// `(identity, reduced value)` pair in canonical order — identity bytes
/// verbatim (they are already canonically encoded by the controller), value
/// as a domain-tagged little-endian encoding. Moment-blind: the same reduced
/// state is the same cell wherever it occurs.
pub fn canonical_cell(_cut: CutRow, obs: &[(ObsKey, ReducedRow)]) -> CellBytes {
    let mut out = Vec::new();
    for (id, val) in obs {
        out.extend_from_slice(id);
        match val {
            ReducedRow::Scalar(v) => {
                out.push(0x01);
                out.extend_from_slice(&v.to_le_bytes());
            }
            ReducedRow::Accumulated(set) => {
                out.push(0x02);
                out.extend_from_slice(&(set.len() as u64).to_le_bytes());
                for v in set {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    out
}

/// The internal partial aggregate for one `(observation, op)` dimension. All
/// combines are commutative and associative, so segment aggregates compose in
/// any association — the property the shared formulation rests on (ported
/// from the spike's `Agg`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Agg {
    /// `set`: the latest `(pos, value)`; the greater `pos` wins.
    Last(u64, u64),
    /// `max`.
    Max(u64),
    /// `min`.
    Min(u64),
    /// `accumulate`: sorted distinct values.
    Distinct(Vec<u64>),
}

impl Agg {
    /// The aggregate a single event contributes under `op`.
    pub(crate) fn unit(op: ReduceOp, pos: u64, value: u64) -> Agg {
        match op {
            ReduceOp::Set => Agg::Last(pos, value),
            ReduceOp::Max => Agg::Max(value),
            ReduceOp::Min => Agg::Min(value),
            ReduceOp::Accumulate => Agg::Distinct(vec![value]),
        }
    }

    /// Combine two aggregates of one dimension. Kinds cannot differ for one
    /// dimension by construction (staging refuses op conflicts per identity,
    /// so the declaration join fixes the constructor); a mismatch is an
    /// internal invariant break and the merge keeps the left operand's kind
    /// deterministically (it cannot occur through the public API).
    pub(crate) fn combine(&self, other: &Agg) -> Agg {
        match (self, other) {
            (Agg::Last(p1, v1), Agg::Last(p2, v2)) => {
                if p2 >= p1 {
                    Agg::Last(*p2, *v2)
                } else {
                    Agg::Last(*p1, *v1)
                }
            }
            (Agg::Max(a), Agg::Max(b)) => Agg::Max(*a.max(b)),
            (Agg::Min(a), Agg::Min(b)) => Agg::Min(*a.min(b)),
            (Agg::Distinct(a), Agg::Distinct(b)) => {
                let mut v: Vec<u64> = a.iter().chain(b.iter()).copied().collect();
                v.sort_unstable();
                v.dedup();
                Agg::Distinct(v)
            }
            (left, _) => left.clone(),
        }
    }

    /// Project a complete aggregate into its reduced view value.
    pub(crate) fn reduced(&self) -> ReducedRow {
        match self {
            Agg::Last(_, v) | Agg::Max(v) | Agg::Min(v) => ReducedRow::Scalar(*v),
            Agg::Distinct(vs) => ReducedRow::Accumulated(vs.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aggregate combines are commutative and associative for every op — the
    /// property the shared segment formulation rests on.
    #[test]
    fn agg_combine_is_commutative_and_associative() {
        let cases = [
            (Agg::Last(1, 10), Agg::Last(5, 50), Agg::Last(3, 30)),
            (Agg::Max(4), Agg::Max(9), Agg::Max(1)),
            (Agg::Min(4), Agg::Min(9), Agg::Min(1)),
            (
                Agg::Distinct(vec![3]),
                Agg::Distinct(vec![1, 9]),
                Agg::Distinct(vec![3, 5]),
            ),
        ];
        for (a, b, c) in cases {
            assert_eq!(a.combine(&b), b.combine(&a), "commutative");
            assert_eq!(
                a.combine(&b).combine(&c),
                a.combine(&b.combine(&c)),
                "associative"
            );
        }
    }

    /// Units reduce to the declared semantics.
    #[test]
    fn agg_units_reduce_per_op() {
        let set = Agg::unit(ReduceOp::Set, 0, 3).combine(&Agg::unit(ReduceOp::Set, 1, 9));
        assert_eq!(set.reduced(), ReducedRow::Scalar(9), "latest pos wins");
        let max = Agg::unit(ReduceOp::Max, 0, 3).combine(&Agg::unit(ReduceOp::Max, 1, 9));
        assert_eq!(max.reduced(), ReducedRow::Scalar(9));
        let min = Agg::unit(ReduceOp::Min, 0, 3).combine(&Agg::unit(ReduceOp::Min, 1, 9));
        assert_eq!(min.reduced(), ReducedRow::Scalar(3));
        let acc = Agg::unit(ReduceOp::Accumulate, 0, 3)
            .combine(&Agg::unit(ReduceOp::Accumulate, 1, 9))
            .combine(&Agg::unit(ReduceOp::Accumulate, 2, 3));
        assert_eq!(acc.reduced(), ReducedRow::Accumulated(vec![3, 9]));
    }

    /// The canonical cell projection discriminates reduced states and is
    /// moment-blind.
    #[test]
    fn canonical_cell_discriminates_and_ignores_moment() {
        let a = vec![(vec![1u8], ReducedRow::Scalar(5))];
        let b = vec![(vec![1u8], ReducedRow::Scalar(6))];
        let cut1 = CutRow {
            moment: 1,
            count: 1,
        };
        let cut2 = CutRow {
            moment: 9,
            count: 1,
        };
        assert_eq!(canonical_cell(cut1, &a), canonical_cell(cut2, &a));
        assert_ne!(canonical_cell(cut1, &a), canonical_cell(cut1, &b));
        assert!(canonical_cell(cut1, &[]).is_empty());
    }
}
