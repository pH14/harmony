// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persisted record model and derived-value types.
//!
//! Every record an input collection carries is defined here, shaped after the
//! evidence-identity contract in `docs/DISSONANCE-STRATEGY.md` and the epic bead
//! (`hm-bbx`): ordered evidence is keyed by campaign/configuration, a
//! deterministic rollout identity, source identity, `Moment`, and an explicit
//! source ordinal (the persisted SDK vector position). Revision — the one
//! differential timestamp — appears on records only as the commit schedule
//! (`rev`); it is never part of evidence identity. `Moment` and ordinal are
//! plain data columns.

use serde::{Deserialize, Serialize};

/// Campaign configuration identity (stands in for the versioned, hashed
/// `CampaignConfig` reference).
pub type CfgId = u32;
/// Deterministic rollout identity.
pub type RolloutId = u32;
/// Candidate-seal identity within a campaign.
pub type SealId = u32;
/// Retained-entry identity.
pub type EntryId = u32;
/// State-register observation identity.
pub type RegId = u32;
/// Assertion site identity (provenance/coverage, not a property verdict).
pub type SiteId = u32;
/// Property identity (what assertion evidence aggregates by).
pub type PropId = u32;
/// Evidence-source identity.
pub type SourceId = u32;
/// Cross-source sequence-query identity.
pub type QueryId = u32;
/// A point on the deterministic V-time axis. Data, never a timestamp.
pub type Moment = u64;
/// Persisted SDK vector position: the contractual rollout-local source
/// ordinal, cumulative through restored ancestor prefixes.
pub type Pos = u64;
/// Campaign revision: the differential timestamp. Numbers a committed input
/// update, not a rollout.
pub type Revision = u64;

/// An evidence cut: half-open — persisted SDK vector positions strictly less
/// than `count` are included, including the exact subset emitted at the cut's
/// `Moment`. `count` is the authority; `moment` rides along as data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Cut {
    /// Synchronized seal `Moment`.
    pub moment: Moment,
    /// Included SDK-event count (full persisted vector prefix length).
    pub count: Pos,
}

/// Base update operation a state-bearing register declares (normalized
/// `SdkSchema` semantics; the fixture stand-in for the hm-bbx.1 contract).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ReduceOp {
    /// Latest value at or before the queried cut is current.
    Set,
    /// Greatest value observed so far.
    Max,
    /// Least value observed so far.
    Min,
    /// Set of distinct values observed so far.
    Accumulate,
}

/// Declared ordering scope of a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OrderScope {
    /// Stamped on the one deterministic machine-event stream: the ordinal is
    /// rollout-global across sources and may participate in cross-source
    /// sequence predicates.
    RolloutGlobal,
    /// Source-local order only (e.g. the current serial-console scrape):
    /// full-run evidence, rejected from cross-source sequence queries.
    SourceLocal,
}

/// Payload of one persisted SDK event.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Payload {
    /// A state-register update (the register's op lives in its declaration).
    Register {
        /// Observation identity.
        reg: RegId,
        /// Update value (bounded integer per the strategy's no-float rule).
        value: i64,
    },
    /// One assertion evaluation: an occurrence, not persistent state.
    Assertion {
        /// Site provenance (contributes coverage, never a verdict).
        site: SiteId,
        /// Property this evaluation aggregates into.
        property: PropId,
        /// Whether the evaluation satisfied the property.
        passed: bool,
    },
    /// A generic one-shot occurrence (input to history derivations).
    Note {
        /// Species tag.
        tag: u32,
    },
}

/// One persisted, normalized SDK event (the immutable evidence ledger).
/// Identity: `(config, rollout, source, moment, pos)`; `pos` is unique per
/// rollout and supplies same-`Moment` order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SdkEventRec {
    /// Revision at which this record commits (schedule, not identity).
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Owning rollout: a child rollout persists only positions after its
    /// parent cut; the restored prefix is inherited through lineage.
    pub rollout: RolloutId,
    /// Source identity (must be declared).
    pub source: SourceId,
    /// Persisted SDK vector position (cumulative through the lineage).
    pub pos: Pos,
    /// V-time coordinate. Nondecreasing in `pos` within a rollout.
    pub moment: Moment,
    /// Normalized payload.
    pub payload: Payload,
}

/// One decoded serial-scrape line: source-local, stop-granular evidence.
/// Deliberately has no `pos` in the SDK vector space and no capture-time
/// `Moment` — which is exactly why it is rejected from seal-relative and
/// cross-source sequence queries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScrapeLineRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Owning rollout.
    pub rollout: RolloutId,
    /// Source-local ordinal (line index at terminal stop).
    pub local_ord: u64,
    /// Decoded line template tag.
    pub tag: u32,
}

/// Declares a state register and its base update operation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RegisterDecl {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Register identity.
    pub reg: RegId,
    /// Declared base update operation.
    pub op: ReduceOp,
}

/// Declares a source and its ordering scope.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceDecl {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Source identity.
    pub source: SourceId,
    /// Declared ordering scope.
    pub scope: OrderScope,
}

/// Declares a property and whether absence of a satisfying evaluation is a
/// finding (the source protocol's `must_hit` semantics).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PropertyDecl {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Property identity.
    pub property: PropId,
    /// Whether a never-satisfied property is a finalized absence finding.
    pub must_hit: bool,
}

/// Entry lineage: `child` was branched from `parent` at `cut`. The authority
/// for prefix composition. Sibling children may share the same parent cut;
/// their own rollout identities keep their evidence coordinates disjoint.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LineageRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Child rollout.
    pub child: RolloutId,
    /// Parent rollout.
    pub parent: RolloutId,
    /// Branch-point evidence cut on the parent's vector.
    pub cut: Cut,
}

/// A configured unsealed evidence cut: first-pass observation point. Derived
/// transitions at these cuts are provisional — they nominate materialization
/// replay and can never enter archive occupancy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObsCutRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Rollout the cut addresses.
    pub rollout: RolloutId,
    /// The evidence cut.
    pub cut: Cut,
}

/// A candidate seal: the second-pass, physically-held coordinate produced by
/// materialization replay. Enters at a later revision than the rollout's
/// evidence (the two-revision barrier).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SealRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Sealed rollout.
    pub rollout: RolloutId,
    /// Seal identity.
    pub seal: SealId,
    /// The seal's evidence cut, bound at snapshot time.
    pub cut: Cut,
}

/// A committed Entry assignment: the controller retained `entry` at a seal
/// with versioned quality data. Occupancy is reduced from these records and
/// the seal's derived cell — never from provisional transitions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntryCommitRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Entry identity (the stable tie-break: lower id wins at equal quality).
    pub entry: EntryId,
    /// Sealed rollout.
    pub rollout: RolloutId,
    /// The seal this entry holds.
    pub seal: SealId,
    /// Versioned quality datum (greater is better).
    pub quality: i64,
}

/// Bounded working-set membership update for one evidence coordinate:
/// admission (`delta = +1`) and expiration (`delta = -1`) are ordinary
/// positive and negative differential updates.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkingRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Referenced evidence rollout.
    pub rollout: RolloutId,
    /// Referenced evidence position.
    pub pos: Pos,
    /// +1 admission / -1 expiration.
    pub delta: i64,
}

/// A cross-source sequence query: asks for ordered pairs of `Note` events,
/// the first from `src_a`, the second from `src_b`, within each rollout.
/// Sources without rollout-global order are rejected, not answered.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeqQueryRec {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Query identity.
    pub query: QueryId,
    /// First source.
    pub src_a: SourceId,
    /// Second source.
    pub src_b: SourceId,
}

/// One fixture: every persisted record with its commit revision. Vectors are
/// in authoring order; serialization is deterministic (no maps).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixture {
    /// Fixture name (for report labeling).
    pub name: String,
    /// Register declarations.
    pub registers: Vec<RegisterDecl>,
    /// Source declarations.
    pub sources: Vec<SourceDecl>,
    /// Property declarations.
    pub properties: Vec<PropertyDecl>,
    /// The immutable SDK evidence ledger.
    pub events: Vec<SdkEventRec>,
    /// Serial-scrape evidence (source-local).
    pub scrape: Vec<ScrapeLineRec>,
    /// Entry lineage.
    pub lineage: Vec<LineageRec>,
    /// Configured unsealed evidence cuts.
    pub obs_cuts: Vec<ObsCutRec>,
    /// Candidate seals.
    pub seals: Vec<SealRec>,
    /// Committed Entry assignments.
    pub entry_commits: Vec<EntryCommitRec>,
    /// Working-set membership updates.
    pub working: Vec<WorkingRec>,
    /// Cross-source sequence queries.
    pub seq_queries: Vec<SeqQueryRec>,
}

impl Fixture {
    /// Validate the structural contracts every consumer relies on. Returns a
    /// description of the first violation found. Checked by `dataflow::run`
    /// (which refuses malformed fixtures instead of hanging or panicking mid-
    /// dataflow) and by `Referee::new`.
    ///
    /// Contracts:
    /// - no record commits at `Revision::MAX` (the driver advances to
    ///   `rev + 1`);
    /// - lineage is a forest per config: no self-parent, at most one parent
    ///   per child, no cycles (a cycle would prevent the ancestry iteration
    ///   from ever reaching a fixed point);
    /// - each rollout's persisted positions are exactly the contiguous range
    ///   `[start, start + n)` where `start` is its branch-point count (the
    ///   restored prefix is inherited, never re-persisted);
    /// - no cut (fork, configured, or seal) precedes its rollout's branch
    ///   point or exceeds its persisted evidence (the physical cut contract:
    ///   a machine exists only from its branch moment onward, so cuts are
    ///   nondecreasing along every lineage path — lineage composition is
    ///   sound only under it).
    pub fn validate(&self) -> Result<(), String> {
        let max_rev_ok = |rev: Revision, what: &str| -> Result<(), String> {
            if rev == Revision::MAX {
                Err(format!("{what} commits at Revision::MAX"))
            } else {
                Ok(())
            }
        };
        for r in &self.registers {
            max_rev_ok(r.rev, "register declaration")?;
        }
        for r in &self.sources {
            max_rev_ok(r.rev, "source declaration")?;
        }
        for r in &self.properties {
            max_rev_ok(r.rev, "property declaration")?;
        }
        for r in &self.events {
            max_rev_ok(r.rev, "event")?;
        }
        for r in &self.scrape {
            max_rev_ok(r.rev, "scrape line")?;
        }
        for r in &self.lineage {
            max_rev_ok(r.rev, "lineage record")?;
        }
        for r in &self.obs_cuts {
            max_rev_ok(r.rev, "obs cut")?;
        }
        for r in &self.seals {
            max_rev_ok(r.rev, "seal")?;
        }
        for r in &self.entry_commits {
            max_rev_ok(r.rev, "entry commit")?;
        }
        for r in &self.working {
            max_rev_ok(r.rev, "working update")?;
        }
        for r in &self.seq_queries {
            max_rev_ok(r.rev, "sequence query")?;
        }

        // Lineage: forest per config.
        let mut parent: std::collections::BTreeMap<(CfgId, RolloutId), (RolloutId, Pos)> =
            std::collections::BTreeMap::new();
        for l in &self.lineage {
            if l.child == l.parent {
                return Err(format!("rollout {} is its own parent", l.child));
            }
            if parent
                .insert((l.config, l.child), (l.parent, l.cut.count))
                .is_some()
            {
                return Err(format!("rollout {} has two parents", l.child));
            }
        }
        for &(config, child) in parent.keys() {
            let mut seen = std::collections::BTreeSet::new();
            let mut cur = child;
            while let Some(&(p, _)) = parent.get(&(config, cur)) {
                if !seen.insert(cur) {
                    return Err(format!(
                        "lineage cycle through rollout {cur} (config {config})"
                    ));
                }
                cur = p;
            }
        }

        // Per-rollout persisted extent: positions are contiguous from the
        // branch-point count.
        let mut own: std::collections::BTreeMap<(CfgId, RolloutId), Vec<Pos>> =
            std::collections::BTreeMap::new();
        for e in &self.events {
            own.entry((e.config, e.rollout)).or_default().push(e.pos);
        }
        let start_of = |config: CfgId, rollout: RolloutId| -> Pos {
            parent.get(&(config, rollout)).map(|&(_, c)| c).unwrap_or(0)
        };
        let mut extent: std::collections::BTreeMap<(CfgId, RolloutId), Pos> =
            std::collections::BTreeMap::new();
        for ((config, rollout), mut positions) in own {
            positions.sort_unstable();
            let start = start_of(config, rollout);
            let expect: Vec<Pos> = (start..start + positions.len() as Pos).collect();
            if positions != expect {
                return Err(format!(
                    "rollout {rollout} (config {config}) persists non-contiguous                      positions {positions:?}; expected {expect:?}"
                ));
            }
            extent.insert((config, rollout), start + positions.len() as Pos);
        }
        let extent_of = |config: CfgId, rollout: RolloutId| -> Pos {
            extent
                .get(&(config, rollout))
                .copied()
                .unwrap_or(start_of(config, rollout))
        };

        // Cut bounds: start <= count <= extent, for every cut kind.
        for l in &self.lineage {
            let (lo, hi) = (start_of(l.config, l.parent), extent_of(l.config, l.parent));
            if l.cut.count < lo || l.cut.count > hi {
                return Err(format!(
                    "fork cut {} on rollout {} outside [{lo}, {hi}]",
                    l.cut.count, l.parent
                ));
            }
        }
        for c in &self.obs_cuts {
            let (lo, hi) = (
                start_of(c.config, c.rollout),
                extent_of(c.config, c.rollout),
            );
            if c.cut.count < lo || c.cut.count > hi {
                return Err(format!(
                    "obs cut {} on rollout {} outside [{lo}, {hi}]",
                    c.cut.count, c.rollout
                ));
            }
        }
        for sl in &self.seals {
            let (lo, hi) = (
                start_of(sl.config, sl.rollout),
                extent_of(sl.config, sl.rollout),
            );
            if sl.cut.count < lo || sl.cut.count > hi {
                return Err(format!(
                    "seal cut {} on rollout {} outside [{lo}, {hi}]",
                    sl.cut.count, sl.rollout
                ));
            }
        }
        Ok(())
    }

    /// Greatest revision any record commits at.
    pub fn max_rev(&self) -> Revision {
        let mut m = 0;
        let mut see = |r: Revision| {
            if r > m {
                m = r;
            }
        };
        self.registers.iter().for_each(|r| see(r.rev));
        self.sources.iter().for_each(|r| see(r.rev));
        self.properties.iter().for_each(|r| see(r.rev));
        self.events.iter().for_each(|r| see(r.rev));
        self.scrape.iter().for_each(|r| see(r.rev));
        self.lineage.iter().for_each(|r| see(r.rev));
        self.obs_cuts.iter().for_each(|r| see(r.rev));
        self.seals.iter().for_each(|r| see(r.rev));
        self.entry_commits.iter().for_each(|r| see(r.rev));
        self.working.iter().for_each(|r| see(r.rev));
        self.seq_queries.iter().for_each(|r| see(r.rev));
        m
    }
}

/// Genesis-complete replay vectors: for every rollout, the full SDK vector a
/// genesis-complete reproducer replay would observe (restored ancestor prefix,
/// with ancestor evidence identity preserved, plus the rollout's own suffix).
/// Produced by the generator's simulation; the referee's semantic authority.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    /// `(rollout, full vector)` pairs, ascending by rollout. Each vector's
    /// index equals its events' `pos`.
    pub full: Vec<(RolloutId, Vec<SdkEventRec>)>,
}

impl Replay {
    /// The full vector for one rollout.
    pub fn vector(&self, rollout: RolloutId) -> &[SdkEventRec] {
        self.full
            .iter()
            .find(|(r, _)| *r == rollout)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// Derived-value types (shared by the dataflow and the referee).
// ---------------------------------------------------------------------------

/// An evaluation point on a rollout's evidence vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PointId {
    /// A configured unsealed evidence cut (first pass; provisional).
    Cut(Pos),
    /// A branch point: some child forked here (baseline for its transitions).
    Fork(Pos),
    /// A candidate seal (second pass; authoritative for occupancy).
    Seal(SealId),
}

/// One reduced observation dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Dim {
    /// A declared state register under its declared operation.
    Reg(RegId, ReduceOp),
    /// A note tag's configured history derivation.
    Tag(u32),
}

/// A partial or complete aggregate for one dimension. All combines are
/// commutative and associative, so segment aggregates compose in any
/// association — the property the shared formulation rests on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Agg {
    /// `set`: the latest `(pos, value)`; the greater `pos` wins.
    Last(Pos, i64),
    /// `max`.
    Max(i64),
    /// `min`.
    Min(i64),
    /// `accumulate`: sorted distinct values.
    Distinct(Vec<i64>),
    /// History over occurrences: `count` and `latest (moment, pos)`.
    Hist(i64, (Moment, Pos)),
}

impl Agg {
    /// The aggregate a single event contributes.
    pub fn unit(dim: &Dim, pos: Pos, moment: Moment, value: i64) -> Agg {
        match dim {
            Dim::Reg(_, ReduceOp::Set) => Agg::Last(pos, value),
            Dim::Reg(_, ReduceOp::Max) => Agg::Max(value),
            Dim::Reg(_, ReduceOp::Min) => Agg::Min(value),
            Dim::Reg(_, ReduceOp::Accumulate) => Agg::Distinct(vec![value]),
            Dim::Tag(_) => Agg::Hist(1, (moment, pos)),
        }
    }

    /// Combine two aggregates of the same dimension kind. Kinds cannot differ
    /// for one `Dim` by construction (the dimension fixes the constructor);
    /// a mismatch is a generator/dataflow bug and panics loudly.
    pub fn combine(&self, other: &Agg) -> Agg {
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
                let mut v: Vec<i64> = a.iter().chain(b.iter()).copied().collect();
                v.sort_unstable();
                v.dedup();
                Agg::Distinct(v)
            }
            (Agg::Hist(c1, l1), Agg::Hist(c2, l2)) => Agg::Hist(c1 + c2, *l1.max(l2)),
            _ => unreachable!("aggregate kind mismatch within one dimension"),
        }
    }

    /// Scale by a multiplicity (evidence records are unique by coordinate, so
    /// only `Hist` counts are sensitive to it).
    pub fn scaled(&self, weight: i64) -> Agg {
        match self {
            Agg::Hist(c, l) => Agg::Hist(c * weight, *l),
            other => other.clone(),
        }
    }
}

/// A projected observation value at a point (what `CellFn` consumes and what
/// obs views expose).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObsOut {
    /// `set`/`max`/`min` current value.
    Scalar(i64),
    /// `accumulate` distinct-value set.
    Values(Vec<i64>),
    /// History derivations: `count`, `ever`, `latest`.
    Hist {
        /// Occurrence count through the cut.
        count: i64,
        /// Whether any occurrence exists (`count > 0`).
        ever: bool,
        /// Latest occurrence coordinate.
        latest: (Moment, Pos),
    },
}

impl ObsOut {
    /// Project a complete aggregate into its observation value.
    pub fn from_agg(agg: &Agg) -> ObsOut {
        match agg {
            Agg::Last(_, v) | Agg::Max(v) | Agg::Min(v) => ObsOut::Scalar(*v),
            Agg::Distinct(vs) => ObsOut::Values(vs.clone()),
            Agg::Hist(c, l) => ObsOut::Hist {
                count: *c,
                ever: *c > 0,
                latest: *l,
            },
        }
    }
}

/// One committed composite cell key: the spike's `CellFn` output. A sorted
/// vector of `(dim kind, id, projected scalar)` — exact, hash-free.
pub type CellKey = Vec<(u8, u32, i64)>;

/// The spike's versioned cell projection ("CellFnSpike1", deliberately not a
/// ratification of any production `CellFn`): register scalars pass through,
/// `accumulate` contributes its cardinality, tags contribute a saturating
/// occurrence bucket.
pub fn cell_fn(obs: &[(Dim, ObsOut)]) -> CellKey {
    let mut key: CellKey = obs
        .iter()
        .map(|(dim, out)| match (dim, out) {
            (Dim::Reg(reg, _), ObsOut::Scalar(v)) => (0u8, *reg, *v),
            (Dim::Reg(reg, _), ObsOut::Values(vs)) => (0u8, *reg, vs.len() as i64),
            (Dim::Tag(tag), ObsOut::Hist { count, .. }) => (1u8, *tag, (*count).min(3)),
            _ => unreachable!("dimension/value kind mismatch"),
        })
        .collect();
    key.sort_unstable();
    key
}

/// A provisional observation/cell transition at a configured unsealed cut.
/// The replay-nomination record: readable, ordered, and structurally unable
/// to reach occupancy (occupancy reduces committed entries at seals only).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Transition {
    /// The cut count at which the new cell was observed.
    pub at_count: Pos,
    /// Previous cell: the prior configured cut's cell, or the inherited
    /// branch-point baseline, or `None` at a genesis rollout's first cut.
    pub from: Option<CellKey>,
    /// Newly observed cell.
    pub to: CellKey,
}

/// Working-set species (what the bounded novelty view counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Species {
    /// Register-update evidence for one register.
    Register(RegId),
    /// Assertion evidence for one property.
    Assertion(PropId),
    /// Note evidence for one tag.
    Note(u32),
}

impl Species {
    /// The species of one payload.
    pub fn of(payload: &Payload) -> Species {
        match payload {
            Payload::Register { reg, .. } => Species::Register(*reg),
            Payload::Assertion { property, .. } => Species::Assertion(*property),
            Payload::Note { tag } => Species::Note(*tag),
        }
    }
}

// ---------------------------------------------------------------------------
// View rows: the exact shapes both the dataflow and the referee produce.
// ---------------------------------------------------------------------------

/// One event inside a lineage-composed seal prefix, with its owning rollout
/// identity preserved (ancestor evidence is inherited, never re-owned).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PrefixEv {
    /// The rollout that persisted this event.
    pub owner: RolloutId,
    /// Source identity.
    pub source: SourceId,
    /// Vector position.
    pub pos: Pos,
    /// V-time coordinate.
    pub moment: Moment,
    /// Payload.
    pub payload: Payload,
}

/// A lineage-complete prefix event at a candidate seal.
pub type PrefixRow = ((CfgId, RolloutId, SealId), PrefixEv);
/// A reduced/derived observation at an evaluation point.
pub type ObsRow = ((CfgId, RolloutId, PointId, Dim), ObsOut);
/// A committed cell at an evaluation point.
pub type CellRow = ((CfgId, RolloutId, PointId), CellKey);
/// A provisional transition (replay nomination) on a rollout.
pub type TransRow = ((CfgId, RolloutId), Transition);
/// Archive occupancy: best entry per cell.
pub type OccRow = ((CfgId, CellKey), EntryId);
/// Property-level assertion aggregation: `(pass, fail)` evaluation counts.
pub type PropRow = ((CfgId, PropId), (i64, i64));
/// Site coverage (provenance, separate from property verdicts).
pub type SiteRow = ((CfgId, PropId, SiteId), i64);
/// A finalized absence finding: a `must_hit` property with no satisfying
/// evaluation.
pub type AbsRow = (CfgId, PropId);
/// Bounded working-set species count.
pub type WorkRow = ((CfgId, Species), i64);
/// A note-event endpoint in a cross-source sequence pair.
pub type NoteRef = (Pos, Moment, u32);
/// One ordered cross-source pair within a rollout segment.
pub type SeqPairRow = ((CfgId, QueryId, RolloutId), (NoteRef, NoteRef));
/// A rejected cross-source sequence query participant.
pub type SeqRejRow = ((CfgId, QueryId), SourceId);
/// Terminal (stop-granular) scrape evidence, reportable but never
/// seal-relative.
pub type ScrapeRow = ((CfgId, RolloutId), (u64, u32));
