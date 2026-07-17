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

/// Declares a property under one source schema and whether absence of a
/// satisfying evaluation is a finding (the source protocol's `must_hit`
/// semantics). Property identity is scoped by the declaring source-schema
/// instance (r6): two sources sharing a numeric `PropId` are distinct
/// properties — their evaluation counts never merge and each source's
/// `must_hit` expectation is judged against its own evidence only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PropertyDecl {
    /// Commit revision.
    pub rev: Revision,
    /// Campaign configuration.
    pub config: CfgId,
    /// Declaring source-schema instance.
    pub source: SourceId,
    /// Property identity within that source.
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

/// Campaign finalization for one configuration: the explicit closure record
/// after which finalized facts (the absence view) are derivable. Evidence
/// and property declarations must precede it; bounded working-set retention
/// deliberately continues after it (the retention contract finalized facts
/// must survive).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FinalizeRec {
    /// Commit revision (the campaign-closure revision).
    pub rev: Revision,
    /// Finalized campaign configuration.
    pub config: CfgId,
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
    /// Campaign finalizations.
    pub finalizations: Vec<FinalizeRec>,
}

/// A structural-contract violation in a decoded fixture. Returned (never
/// panicked) through the public APIs: `dataflow::run` and `Referee::new`
/// refuse the fixture instead of hanging, overflowing, or slicing out of
/// bounds on it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// A record commits at `Revision::MAX`, which the driver cannot advance
    /// past.
    #[error("{what} commits at Revision::MAX")]
    RevisionMax {
        /// Record class.
        what: &'static str,
    },
    /// A rollout is declared as its own parent.
    #[error("rollout {rollout} is its own parent")]
    SelfParent {
        /// Offending rollout.
        rollout: RolloutId,
    },
    /// A rollout has more than one lineage parent.
    #[error("rollout {rollout} has two parents")]
    TwoParents {
        /// Offending rollout.
        rollout: RolloutId,
    },
    /// The lineage relation contains a cycle — the ancestry iteration would
    /// never reach a fixed point.
    #[error("lineage cycle through rollout {rollout} (config {config})")]
    LineageCycle {
        /// Campaign configuration.
        config: CfgId,
        /// A rollout on the cycle.
        rollout: RolloutId,
    },
    /// A rollout's persisted positions are not the contiguous range from its
    /// branch-point count.
    #[error("rollout {rollout} (config {config}) persists non-contiguous positions")]
    NonContiguousPositions {
        /// Campaign configuration.
        config: CfgId,
        /// Offending rollout.
        rollout: RolloutId,
    },
    /// Position arithmetic overflows `u64` (start + own event count).
    #[error("rollout {rollout} (config {config}) position range overflows")]
    PositionOverflow {
        /// Campaign configuration.
        config: CfgId,
        /// Offending rollout.
        rollout: RolloutId,
    },
    /// Moments decrease along a rollout's own persisted positions, or a
    /// child's first own event precedes the last moment it inherits — either
    /// breaks canonical `(Moment, pos)` order reconstruction.
    #[error(
        "moment {moment} at position {pos} of rollout {rollout} (config {config}) \
         precedes moment {prev}"
    )]
    DecreasingMoments {
        /// Campaign configuration.
        config: CfgId,
        /// Offending rollout.
        rollout: RolloutId,
        /// Offending position.
        pos: Pos,
        /// Offending moment.
        moment: Moment,
        /// The moment it must not precede.
        prev: Moment,
    },
    /// A cut (fork, configured, or seal) precedes its rollout's branch point
    /// or exceeds its persisted evidence — the physical cut contract.
    #[error("{kind} cut {count} on rollout {rollout} outside [{lo}, {hi}]")]
    CutOutOfBounds {
        /// Which cut kind ("fork", "obs", "seal").
        kind: &'static str,
        /// Offending rollout (the parent, for fork cuts).
        rollout: RolloutId,
        /// Offending count.
        count: Pos,
        /// Branch-point lower bound.
        lo: Pos,
        /// Persisted-extent upper bound.
        hi: Pos,
    },
    /// More than one declaration for the same identity (register, source, or
    /// property). Duplicate declarations make the dataflow's declaration
    /// joins fan out and disagree with the referee's last-wins map.
    #[error("duplicate {what} declaration for id {id} (config {config})")]
    DuplicateDeclaration {
        /// Declaration class ("register", "source", "property").
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// Declared identity.
        id: u32,
    },
    /// A sequence query names an undeclared source — the dataflow would
    /// silently drop it while the referee rejects it.
    #[error("sequence query {query} (config {config}) names undeclared source {src}")]
    UndeclaredQuerySource {
        /// Campaign configuration.
        config: CfgId,
        /// Offending query.
        query: QueryId,
        /// Undeclared source (named `src`: thiserror reserves `source`).
        src: SourceId,
    },
    /// Two records share one documented record identity (seal ids per
    /// config; obs-cut counts per rollout; scrape ordinals per rollout;
    /// query ids per config; entry ids per config). Duplicates would emit
    /// multiplicity-2 view rows and break the canonical unit-multiplicity
    /// read.
    #[error("duplicate {what} record ({detail}) in config {config}")]
    DuplicateRecord {
        /// Record class.
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// The colliding identity, rendered.
        detail: String,
    },
    /// A record that depends on a rollout's lineage commits before the
    /// lineage record itself — the dataflow could not compose the inherited
    /// prefix the referee already knows about. Production writes lineage at
    /// branch creation, before any dependent record.
    #[error(
        "{what} on rollout {child} (config {config}) commits at revision {rev}, \
         before its lineage record at revision {lineage_rev}"
    )]
    RecordBeforeLineage {
        /// Dependent record class ("event", "obs cut", "seal",
        /// "descendant lineage").
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// The forked rollout whose lineage is depended upon.
        child: RolloutId,
        /// The dependent record's revision.
        rev: Revision,
        /// The lineage record's revision.
        lineage_rev: Revision,
    },
    /// An entry commit references a seal that exists at no revision.
    #[error(
        "entry commit {entry} (config {config}) references missing seal {seal} \
         on rollout {rollout}"
    )]
    DanglingEntryCommit {
        /// Campaign configuration.
        config: CfgId,
        /// Committing entry.
        entry: EntryId,
        /// Referenced rollout.
        rollout: RolloutId,
        /// Referenced (missing) seal.
        seal: SealId,
    },
    /// A record uses a declaration that commits only later — the dataflow
    /// evaluates the use once the declaration arrives, while a
    /// revision-filtered reader would already judge it, so the orders must
    /// agree by contract (declarations precede uses).
    #[error(
        "{what} (config {config}) uses declaration {id} at revision {use_rev}, \
         but it commits at revision {decl_rev}"
    )]
    DeclarationAfterUse {
        /// Using record class ("sequence query").
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// Declared identity.
        id: u32,
        /// Declaration revision.
        decl_rev: Revision,
        /// Use revision.
        use_rev: Revision,
    },
    /// A working-set update references an evidence coordinate that exists at
    /// no revision.
    #[error(
        "working update for rollout {rollout} pos {pos} (config {config}) \
         references no persisted event"
    )]
    DanglingWorkingRef {
        /// Campaign configuration.
        config: CfgId,
        /// Referenced rollout.
        rollout: RolloutId,
        /// Referenced position.
        pos: Pos,
    },
    /// A working-set update's delta is not +1/-1. Bounded membership admits
    /// or expires exactly one coordinate at a time; an unconstrained delta
    /// is not a membership update (and could overflow net accumulation).
    #[error(
        "working update for rollout {rollout} pos {pos} (config {config}) at \
         revision {rev} carries delta {delta}; membership updates are +1/-1"
    )]
    WorkingDeltaOutOfRange {
        /// Campaign configuration.
        config: CfgId,
        /// Coordinate rollout.
        rollout: RolloutId,
        /// Coordinate position.
        pos: Pos,
        /// Offending record's revision.
        rev: Revision,
        /// The offending delta.
        delta: i64,
    },
    /// Working membership leaves {0, 1} for one coordinate after some
    /// revision's updates — bounded membership admits at most once and never
    /// expires what was not admitted.
    #[error(
        "working membership for rollout {rollout} pos {pos} (config {config}) \
         nets {net} after revision {rev}"
    )]
    WorkingNetOutOfRange {
        /// Campaign configuration.
        config: CfgId,
        /// Coordinate rollout.
        rollout: RolloutId,
        /// Coordinate position.
        pos: Pos,
        /// Revision after whose updates the net leaves {0, 1}.
        rev: Revision,
        /// The offending net.
        net: i64,
    },
    /// A cut's `Moment` contradicts the evidence it covers or the evidence
    /// that follows it: the last covered event's `Moment` exceeds the cut's,
    /// or a child's first own event precedes its fork cut's `Moment` (a
    /// machine exists only from its branch moment onward).
    #[error(
        "{kind} cut (moment {cut_moment}, count {count}) on rollout {rollout} \
         (config {config}) is incoherent with the event at moment {event_moment}"
    )]
    CutMomentIncoherent {
        /// Cut kind ("fork", "obs", "seal") — "fork" also covers a child's
        /// first own event preceding the fork moment.
        kind: &'static str,
        /// The cut's rollout (the child, for the child-event direction).
        rollout: RolloutId,
        /// Campaign configuration.
        config: CfgId,
        /// The cut's count.
        count: Pos,
        /// The cut's claimed Moment.
        cut_moment: Moment,
        /// The contradicting event's Moment.
        event_moment: Moment,
    },
    /// A cut on a non-genesis rollout claims a `Moment` before the rollout's
    /// branch (birth) `Moment` — the machine did not exist yet (J1).
    #[error(
        "{kind} cut (moment {cut_moment}, count {count}) on rollout {rollout} \
         (config {config}) precedes the rollout's birth at moment {birth_moment}"
    )]
    CutBeforeBirth {
        /// Cut kind ("fork", "obs", "seal").
        kind: &'static str,
        /// The cut's rollout.
        rollout: RolloutId,
        /// Campaign configuration.
        config: CfgId,
        /// The cut's count.
        count: Pos,
        /// The cut's claimed Moment.
        cut_moment: Moment,
        /// The rollout's fork-cut Moment.
        birth_moment: Moment,
    },
    /// An assertion event or property declaration names a source-schema
    /// instance that is never declared (J4) — property facts must not bind
    /// to schemas that do not exist.
    #[error("{what} (config {config}) names undeclared source {src}")]
    UndeclaredSource {
        /// Using record class ("assertion event", "property declaration").
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// The undeclared source (named `src`: thiserror reserves `source`).
        src: SourceId,
    },
    /// Evidence (or a property declaration) commits after its campaign's
    /// finalization record — finalized facts would emit-and-retract.
    #[error(
        "{what} (config {config}) commits at revision {rev}, after the \
         campaign finalization at revision {finalize_rev}"
    )]
    RecordAfterFinalization {
        /// Offending record class.
        what: &'static str,
        /// Campaign configuration.
        config: CfgId,
        /// The record's revision.
        rev: Revision,
        /// The finalization revision.
        finalize_rev: Revision,
    },
    /// The genesis replay vectors do not cover the fixture's cuts (referee
    /// construction only).
    #[error("replay vector for rollout {rollout} shorter than cut {count}")]
    ReplayTooShort {
        /// Offending rollout.
        rollout: RolloutId,
        /// Uncovered cut count.
        count: Pos,
    },
}

impl Fixture {
    /// Validate the structural contracts every consumer relies on. Returns
    /// the first violation found as a typed error. Checked by
    /// `dataflow::run` (which refuses malformed fixtures instead of hanging
    /// or overflowing) and by `Referee::new`.
    ///
    /// Contracts:
    /// - no record commits at `Revision::MAX` (the driver advances to
    ///   `rev + 1`);
    /// - lineage is a forest per config: no self-parent, at most one parent
    ///   per child, no cycles (a cycle would prevent the ancestry iteration
    ///   from ever reaching a fixed point);
    /// - each rollout's persisted positions are exactly the contiguous range
    ///   `[start, start + n)` (checked arithmetic) where `start` is its
    ///   branch-point count (the restored prefix is inherited, never
    ///   re-persisted);
    /// - moments are nondecreasing along each rollout's own positions AND
    ///   across every lineage boundary (a child's first own event does not
    ///   precede the last moment it inherits) — canonical `(Moment, pos)`
    ///   order rests on it;
    /// - no cut (fork, configured, or seal) precedes its rollout's branch
    ///   point or exceeds its persisted evidence (the physical cut contract:
    ///   a machine exists only from its branch moment onward, so cuts are
    ///   nondecreasing along every lineage path — lineage composition is
    ///   sound only under it);
    /// - at most one register/source/property declaration per identity
    ///   (duplicates make declaration joins fan out);
    /// - sequence queries name declared sources.
    pub fn validate(&self) -> Result<(), ValidationError> {
        use std::collections::{BTreeMap, BTreeSet};

        let max_rev_ok = |rev: Revision, what: &'static str| -> Result<(), ValidationError> {
            if rev == Revision::MAX {
                Err(ValidationError::RevisionMax { what })
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
        for r in &self.finalizations {
            max_rev_ok(r.rev, "finalization")?;
        }

        // Declarations are unique per identity.
        let mut reg_decls: BTreeMap<(CfgId, RegId), ()> = BTreeMap::new();
        for d in &self.registers {
            if reg_decls.insert((d.config, d.reg), ()).is_some() {
                return Err(ValidationError::DuplicateDeclaration {
                    what: "register",
                    config: d.config,
                    id: d.reg,
                });
            }
        }
        let mut src_decls: BTreeMap<(CfgId, SourceId), ()> = BTreeMap::new();
        for d in &self.sources {
            if src_decls.insert((d.config, d.source), ()).is_some() {
                return Err(ValidationError::DuplicateDeclaration {
                    what: "source",
                    config: d.config,
                    id: d.source,
                });
            }
        }
        let mut prop_decls: BTreeMap<(CfgId, SourceId, PropId), ()> = BTreeMap::new();
        for d in &self.properties {
            if prop_decls
                .insert((d.config, d.source, d.property), ())
                .is_some()
            {
                return Err(ValidationError::DuplicateDeclaration {
                    what: "property",
                    config: d.config,
                    id: d.property,
                });
            }
        }
        for q in &self.seq_queries {
            for source in [q.src_a, q.src_b] {
                if !src_decls.contains_key(&(q.config, source)) {
                    return Err(ValidationError::UndeclaredQuerySource {
                        config: q.config,
                        query: q.query,
                        src: source,
                    });
                }
            }
        }
        // Declarations precede uses: a committed query must not wait on its
        // sources' scope declarations (the dataflow's join would sit silent
        // while a revision-filtered reader already judges the query).
        let src_decl_rev: BTreeMap<(CfgId, SourceId), Revision> = self
            .sources
            .iter()
            .map(|d| ((d.config, d.source), d.rev))
            .collect();
        // Property facts bind to source schemas (J4): every assertion event
        // and every property declaration must name a DECLARED source, whose
        // declaration commits no later than the use — otherwise the
        // source-scoped property/absence views would mint finalized facts
        // for schemas that do not exist. Register/note event sources remain
        // unconstrained: no view consumes them except through validated
        // sequence queries.
        for e in &self.events {
            if matches!(e.payload, Payload::Assertion { .. }) {
                match src_decl_rev.get(&(e.config, e.source)) {
                    None => {
                        return Err(ValidationError::UndeclaredSource {
                            what: "assertion event",
                            config: e.config,
                            src: e.source,
                        });
                    }
                    Some(&decl_rev) if decl_rev > e.rev => {
                        return Err(ValidationError::DeclarationAfterUse {
                            what: "assertion event",
                            config: e.config,
                            id: e.source,
                            decl_rev,
                            use_rev: e.rev,
                        });
                    }
                    Some(_) => {}
                }
            }
        }
        for d in &self.properties {
            match src_decl_rev.get(&(d.config, d.source)) {
                None => {
                    return Err(ValidationError::UndeclaredSource {
                        what: "property declaration",
                        config: d.config,
                        src: d.source,
                    });
                }
                Some(&decl_rev) if decl_rev > d.rev => {
                    return Err(ValidationError::DeclarationAfterUse {
                        what: "property declaration",
                        config: d.config,
                        id: d.source,
                        decl_rev,
                        use_rev: d.rev,
                    });
                }
                Some(_) => {}
            }
        }
        for q in &self.seq_queries {
            for source in [q.src_a, q.src_b] {
                if let Some(&decl_rev) = src_decl_rev.get(&(q.config, source))
                    && decl_rev > q.rev
                {
                    return Err(ValidationError::DeclarationAfterUse {
                        what: "sequence query",
                        config: q.config,
                        id: source,
                        decl_rev,
                        use_rev: q.rev,
                    });
                }
            }
        }

        // Record identities are unique: duplicates would emit
        // multiplicity-2 view rows and break canonical unit-multiplicity
        // reads.
        let mut seal_ids: BTreeSet<(CfgId, SealId)> = BTreeSet::new();
        for sl in &self.seals {
            if !seal_ids.insert((sl.config, sl.seal)) {
                return Err(ValidationError::DuplicateRecord {
                    what: "seal",
                    config: sl.config,
                    detail: format!("seal {}", sl.seal),
                });
            }
        }
        let mut cut_ids: BTreeSet<(CfgId, RolloutId, Pos)> = BTreeSet::new();
        for c in &self.obs_cuts {
            if !cut_ids.insert((c.config, c.rollout, c.cut.count)) {
                return Err(ValidationError::DuplicateRecord {
                    what: "obs cut",
                    config: c.config,
                    detail: format!("rollout {} count {}", c.rollout, c.cut.count),
                });
            }
        }
        let mut scrape_ids: BTreeSet<(CfgId, RolloutId, u64)> = BTreeSet::new();
        for sc in &self.scrape {
            if !scrape_ids.insert((sc.config, sc.rollout, sc.local_ord)) {
                return Err(ValidationError::DuplicateRecord {
                    what: "scrape line",
                    config: sc.config,
                    detail: format!("rollout {} ordinal {}", sc.rollout, sc.local_ord),
                });
            }
        }
        let mut query_ids: BTreeSet<(CfgId, QueryId)> = BTreeSet::new();
        for q in &self.seq_queries {
            if !query_ids.insert((q.config, q.query)) {
                return Err(ValidationError::DuplicateRecord {
                    what: "sequence query",
                    config: q.config,
                    detail: format!("query {}", q.query),
                });
            }
        }
        let mut entry_ids: BTreeSet<(CfgId, EntryId)> = BTreeSet::new();
        for ec in &self.entry_commits {
            if !entry_ids.insert((ec.config, ec.entry)) {
                return Err(ValidationError::DuplicateRecord {
                    what: "entry commit",
                    config: ec.config,
                    detail: format!("entry {}", ec.entry),
                });
            }
        }
        let mut finalize_rev_by_cfg: BTreeMap<CfgId, Revision> = BTreeMap::new();
        for f in &self.finalizations {
            if finalize_rev_by_cfg.insert(f.config, f.rev).is_some() {
                return Err(ValidationError::DuplicateRecord {
                    what: "finalization",
                    config: f.config,
                    detail: "campaign closure".to_owned(),
                });
            }
        }
        // Finalization is the last word for the facts derived from it:
        // assertion evidence and property declarations must precede it (so a
        // finalized absence fact can never emit-and-retract). Working-set
        // retention deliberately continues afterwards.
        for e in &self.events {
            if let Some(&finalize_rev) = finalize_rev_by_cfg.get(&e.config)
                && e.rev > finalize_rev
            {
                return Err(ValidationError::RecordAfterFinalization {
                    what: "event",
                    config: e.config,
                    rev: e.rev,
                    finalize_rev,
                });
            }
        }
        for d in &self.properties {
            if let Some(&finalize_rev) = finalize_rev_by_cfg.get(&d.config)
                && d.rev > finalize_rev
            {
                return Err(ValidationError::RecordAfterFinalization {
                    what: "property declaration",
                    config: d.config,
                    rev: d.rev,
                    finalize_rev,
                });
            }
        }
        for sc in &self.scrape {
            // Scrape lines are evidence too (r6 ride-along): the closure
            // cutoff covers every evidence class, not just SDK events.
            if let Some(&finalize_rev) = finalize_rev_by_cfg.get(&sc.config)
                && sc.rev > finalize_rev
            {
                return Err(ValidationError::RecordAfterFinalization {
                    what: "scrape line",
                    config: sc.config,
                    rev: sc.rev,
                    finalize_rev,
                });
            }
        }

        // Cross-record references resolve: entry commits name a seal that
        // exists (at some revision — committing before the seal is a legal
        // ordering both the dataflow join and the revision-filtered referee
        // treat identically), and working updates name a persisted evidence
        // coordinate.
        let seal_refs: BTreeSet<(CfgId, RolloutId, SealId)> = self
            .seals
            .iter()
            .map(|sl| (sl.config, sl.rollout, sl.seal))
            .collect();
        for ec in &self.entry_commits {
            if !seal_refs.contains(&(ec.config, ec.rollout, ec.seal)) {
                return Err(ValidationError::DanglingEntryCommit {
                    config: ec.config,
                    entry: ec.entry,
                    rollout: ec.rollout,
                    seal: ec.seal,
                });
            }
        }
        let event_coords: BTreeSet<(CfgId, RolloutId, Pos)> = self
            .events
            .iter()
            .map(|e| (e.config, e.rollout, e.pos))
            .collect();
        for w in &self.working {
            // Deltas are exactly +1/-1: anything else is not a membership
            // update, and rejecting it here also keeps the net accumulation
            // below trivially in range (r4: a decoded i64::MAX delta must
            // reach a typed error, not overflow inside validation).
            if w.delta != 1 && w.delta != -1 {
                return Err(ValidationError::WorkingDeltaOutOfRange {
                    config: w.config,
                    rollout: w.rollout,
                    pos: w.pos,
                    rev: w.rev,
                    delta: w.delta,
                });
            }
            if !event_coords.contains(&(w.config, w.rollout, w.pos)) {
                return Err(ValidationError::DanglingWorkingRef {
                    config: w.config,
                    rollout: w.rollout,
                    pos: w.pos,
                });
            }
        }
        // Bounded membership: per coordinate, the net stays in {0, 1} after
        // every revision's updates (admit at most once; never expire what
        // was not admitted).
        let mut per_coord: BTreeMap<(CfgId, RolloutId, Pos), Vec<(Revision, i64)>> =
            BTreeMap::new();
        for w in &self.working {
            per_coord
                .entry((w.config, w.rollout, w.pos))
                .or_default()
                .push((w.rev, w.delta));
        }
        for ((config, rollout, pos), mut updates) in per_coord {
            updates.sort_unstable();
            let mut net = 0i64;
            let mut idx = 0;
            while idx < updates.len() {
                let rev = updates[idx].0;
                while idx < updates.len() && updates[idx].0 == rev {
                    // Checked as a backstop: with deltas constrained to +1/-1
                    // above, |net| is bounded by the update count and cannot
                    // overflow, but the accumulation stays total regardless.
                    net = net.checked_add(updates[idx].1).ok_or(
                        ValidationError::WorkingNetOutOfRange {
                            config,
                            rollout,
                            pos,
                            rev,
                            net,
                        },
                    )?;
                    idx += 1;
                }
                if !(0..=1).contains(&net) {
                    return Err(ValidationError::WorkingNetOutOfRange {
                        config,
                        rollout,
                        pos,
                        rev,
                        net,
                    });
                }
            }
        }

        // Lineage: forest per config.
        let mut parent: BTreeMap<(CfgId, RolloutId), (RolloutId, Pos)> = BTreeMap::new();
        for l in &self.lineage {
            if l.child == l.parent {
                return Err(ValidationError::SelfParent { rollout: l.child });
            }
            if parent
                .insert((l.config, l.child), (l.parent, l.cut.count))
                .is_some()
            {
                return Err(ValidationError::TwoParents { rollout: l.child });
            }
        }
        for &(config, child) in parent.keys() {
            let mut seen = BTreeSet::new();
            let mut cur = child;
            while let Some(&(p, _)) = parent.get(&(config, cur)) {
                if !seen.insert(cur) {
                    return Err(ValidationError::LineageCycle {
                        config,
                        rollout: cur,
                    });
                }
                cur = p;
            }
        }

        // Lineage precedes its dependents: everything recorded against a
        // forked rollout — its events, cuts, seals, and any fork off it —
        // commits at or after its lineage record, or the dataflow could not
        // yet compose the inherited prefix the referee already knows about.
        // (Production writes lineage at branch creation, before any child
        // record exists.)
        let lineage_rev: BTreeMap<(CfgId, RolloutId), Revision> = self
            .lineage
            .iter()
            .map(|l| ((l.config, l.child), l.rev))
            .collect();
        let before = |config: CfgId, rollout: RolloutId, rev: Revision| -> Option<Revision> {
            lineage_rev
                .get(&(config, rollout))
                .copied()
                .filter(|&lr| rev < lr)
        };
        for e in &self.events {
            if let Some(lineage_rev) = before(e.config, e.rollout, e.rev) {
                return Err(ValidationError::RecordBeforeLineage {
                    what: "event",
                    config: e.config,
                    child: e.rollout,
                    rev: e.rev,
                    lineage_rev,
                });
            }
        }
        for c in &self.obs_cuts {
            if let Some(lineage_rev) = before(c.config, c.rollout, c.rev) {
                return Err(ValidationError::RecordBeforeLineage {
                    what: "obs cut",
                    config: c.config,
                    child: c.rollout,
                    rev: c.rev,
                    lineage_rev,
                });
            }
        }
        for sl in &self.seals {
            if let Some(lineage_rev) = before(sl.config, sl.rollout, sl.rev) {
                return Err(ValidationError::RecordBeforeLineage {
                    what: "seal",
                    config: sl.config,
                    child: sl.rollout,
                    rev: sl.rev,
                    lineage_rev,
                });
            }
        }
        for l in &self.lineage {
            // A fork OFF a forked rollout depends on that rollout's own
            // lineage (revisions are then nondecreasing along every chain).
            if let Some(lineage_rev) = before(l.config, l.parent, l.rev) {
                return Err(ValidationError::RecordBeforeLineage {
                    what: "descendant lineage",
                    config: l.config,
                    child: l.parent,
                    rev: l.rev,
                    lineage_rev,
                });
            }
        }

        // Per-rollout persisted extent: positions are contiguous from the
        // branch-point count (checked arithmetic — a hostile cut count near
        // u64::MAX must fail the bound check, not overflow before it), and
        // moments are nondecreasing along them.
        let mut own: BTreeMap<(CfgId, RolloutId), Vec<(Pos, Moment)>> = BTreeMap::new();
        for e in &self.events {
            own.entry((e.config, e.rollout))
                .or_default()
                .push((e.pos, e.moment));
        }
        let start_of = |config: CfgId, rollout: RolloutId| -> Pos {
            parent.get(&(config, rollout)).map(|&(_, c)| c).unwrap_or(0)
        };
        let mut extent: BTreeMap<(CfgId, RolloutId), Pos> = BTreeMap::new();
        // (config, rollout) -> (first own moment, last own moment).
        let mut own_moments: BTreeMap<(CfgId, RolloutId), (Moment, Moment)> = BTreeMap::new();
        for ((config, rollout), mut evs) in own {
            evs.sort_unstable();
            let start = start_of(config, rollout);
            let end = start
                .checked_add(evs.len() as Pos)
                .ok_or(ValidationError::PositionOverflow { config, rollout })?;
            let contiguous = evs
                .iter()
                .zip(start..end)
                .all(|(&(pos, _), expect)| pos == expect);
            if !contiguous {
                return Err(ValidationError::NonContiguousPositions { config, rollout });
            }
            let mut prev = evs[0].1;
            for &(pos, moment) in &evs {
                if moment < prev {
                    return Err(ValidationError::DecreasingMoments {
                        config,
                        rollout,
                        pos,
                        moment,
                        prev,
                    });
                }
                prev = moment;
            }
            own_moments.insert((config, rollout), (evs[0].1, prev));
            extent.insert((config, rollout), end);
        }
        let extent_of = |config: CfgId, rollout: RolloutId| -> Pos {
            extent
                .get(&(config, rollout))
                .copied()
                .unwrap_or(start_of(config, rollout))
        };

        // Cross-boundary moment monotonicity: a child's first own event must
        // not precede the last moment covered by its fork cut. The covered
        // last event at position `count - 1` is found by walking up the
        // (already cycle-checked) chain to whichever ancestor owns it; by
        // induction with the per-rollout check above, the full replay vector
        // is nondecreasing.
        let last_covered_moment =
            |config: CfgId, rollout: RolloutId, count: Pos| -> Option<Moment> {
                if count == 0 {
                    return None;
                }
                let target = count - 1;
                let mut cur = rollout;
                loop {
                    let start = start_of(config, cur);
                    if target >= start {
                        // Owned by `cur` iff persisted; positions are contiguous.
                        return own_moments.get(&(config, cur)).and_then(|_| {
                            self.events
                                .iter()
                                .find(|e| e.config == config && e.rollout == cur && e.pos == target)
                                .map(|e| e.moment)
                        });
                    }
                    let &(p, _) = parent.get(&(config, cur))?;
                    cur = p;
                }
            };
        for l in &self.lineage {
            if let (Some(&(first, _)), Some(covered)) = (
                own_moments.get(&(l.config, l.child)),
                last_covered_moment(l.config, l.parent, l.cut.count),
            ) && first < covered
            {
                return Err(ValidationError::DecreasingMoments {
                    config: l.config,
                    rollout: l.child,
                    pos: start_of(l.config, l.child),
                    moment: first,
                    prev: covered,
                });
            }
        }

        // Cut Moment/count coherence (r6 + J1): a cut's claimed Moment must
        // not precede the last event it covers (a seal at Moment 5 cannot
        // include an event from Moment 10); the FIRST EXCLUDED persisted
        // event of the cut's own rollout (position == count) must not
        // precede the cut's Moment either — a seal claiming Moment 100 over
        // a count-1 prefix, with excluded events from Moments 20 and 30,
        // would assert a sealed state that was not true at Moment 100.
        // Equality stays legal in both directions: excluding same-Moment
        // events at or past the count IS the half-open contract. A cut also
        // must not precede its rollout's birth (fork-cut) Moment, and a
        // child's first own event must not precede its fork cut's Moment
        // (the machine exists only from the branch moment onward).
        let birth_moment: BTreeMap<(CfgId, RolloutId), Moment> = self
            .lineage
            .iter()
            .map(|l| ((l.config, l.child), l.cut.moment))
            .collect();
        let event_moment_at: BTreeMap<(CfgId, RolloutId, Pos), Moment> = self
            .events
            .iter()
            .map(|e| ((e.config, e.rollout, e.pos), e.moment))
            .collect();
        let coherent = |kind: &'static str,
                        config: CfgId,
                        rollout: RolloutId,
                        cut: &Cut|
         -> Result<(), ValidationError> {
            if let Some(covered) = last_covered_moment(config, rollout, cut.count)
                && covered > cut.moment
            {
                return Err(ValidationError::CutMomentIncoherent {
                    kind,
                    rollout,
                    config,
                    count: cut.count,
                    cut_moment: cut.moment,
                    event_moment: covered,
                });
            }
            if let Some(&excluded) = event_moment_at.get(&(config, rollout, cut.count))
                && excluded < cut.moment
            {
                return Err(ValidationError::CutMomentIncoherent {
                    kind,
                    rollout,
                    config,
                    count: cut.count,
                    cut_moment: cut.moment,
                    event_moment: excluded,
                });
            }
            if let Some(&birth) = birth_moment.get(&(config, rollout))
                && cut.moment < birth
            {
                return Err(ValidationError::CutBeforeBirth {
                    kind,
                    rollout,
                    config,
                    count: cut.count,
                    cut_moment: cut.moment,
                    birth_moment: birth,
                });
            }
            Ok(())
        };
        for l in &self.lineage {
            coherent("fork", l.config, l.parent, &l.cut)?;
            if let Some(&(first, _)) = own_moments.get(&(l.config, l.child))
                && first < l.cut.moment
            {
                return Err(ValidationError::CutMomentIncoherent {
                    kind: "fork",
                    rollout: l.child,
                    config: l.config,
                    count: l.cut.count,
                    cut_moment: l.cut.moment,
                    event_moment: first,
                });
            }
        }
        for c in &self.obs_cuts {
            coherent("obs", c.config, c.rollout, &c.cut)?;
        }
        for sl in &self.seals {
            coherent("seal", sl.config, sl.rollout, &sl.cut)?;
        }

        // Cut bounds: start <= count <= extent, for every cut kind.
        for l in &self.lineage {
            let (lo, hi) = (start_of(l.config, l.parent), extent_of(l.config, l.parent));
            if l.cut.count < lo || l.cut.count > hi {
                return Err(ValidationError::CutOutOfBounds {
                    kind: "fork",
                    rollout: l.parent,
                    count: l.cut.count,
                    lo,
                    hi,
                });
            }
        }
        for c in &self.obs_cuts {
            let (lo, hi) = (
                start_of(c.config, c.rollout),
                extent_of(c.config, c.rollout),
            );
            if c.cut.count < lo || c.cut.count > hi {
                return Err(ValidationError::CutOutOfBounds {
                    kind: "obs",
                    rollout: c.rollout,
                    count: c.cut.count,
                    lo,
                    hi,
                });
            }
        }
        for sl in &self.seals {
            let (lo, hi) = (
                start_of(sl.config, sl.rollout),
                extent_of(sl.config, sl.rollout),
            );
            if sl.cut.count < lo || sl.cut.count > hi {
                return Err(ValidationError::CutOutOfBounds {
                    kind: "seal",
                    rollout: sl.rollout,
                    count: sl.cut.count,
                    lo,
                    hi,
                });
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
        self.finalizations.iter().for_each(|r| see(r.rev));
        m
    }
}

/// Genesis-complete replay vectors: for every rollout, the full SDK vector a
/// genesis-complete reproducer replay would observe (restored ancestor prefix,
/// with ancestor evidence identity preserved, plus the rollout's own suffix).
/// Produced by the generator's simulation; the referee's semantic authority.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    /// `((config, rollout), full vector)` pairs. Rollout identity is scoped
    /// by campaign configuration — two configs may reuse rollout ids without
    /// their replay vectors colliding (r5). Each vector's index equals its
    /// events' `pos`.
    pub full: Vec<((CfgId, RolloutId), Vec<SdkEventRec>)>,
}

impl Replay {
    /// The full vector for one rollout of one config.
    pub fn vector(&self, config: CfgId, rollout: RolloutId) -> &[SdkEventRec] {
        self.full
            .iter()
            .find(|(k, _)| *k == (config, rollout))
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
///
/// Field order is the contract (r6): the derived `Ord` sorts by the explicit
/// evidence coordinates `(Moment, pos)` FIRST, so every reader that orders
/// rows by `Ord` (`Captured::flat`, `Referee::seal_prefix`) reconstructs the
/// canonical sequence without any view-specific re-sort.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PrefixEv {
    /// V-time coordinate (first: canonical major sort key).
    pub moment: Moment,
    /// Vector position (second: the contractual same-`Moment` order).
    pub pos: Pos,
    /// The rollout that persisted this event.
    pub owner: RolloutId,
    /// Source identity.
    pub source: SourceId,
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
/// Property-level assertion aggregation, scoped by source-schema instance:
/// `(pass, fail)` evaluation counts.
pub type PropRow = ((CfgId, SourceId, PropId), (i64, i64));
/// Site coverage (provenance, separate from property verdicts), scoped by
/// source-schema instance.
pub type SiteRow = ((CfgId, SourceId, PropId, SiteId), i64);
/// A finalized absence finding: a `must_hit` property of one source with no
/// satisfying evaluation from that source.
pub type AbsRow = (CfgId, SourceId, PropId);
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
