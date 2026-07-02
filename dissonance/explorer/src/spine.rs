// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **search-plane trait spine** (task 64) — the Wave-5 keystone contract.
//!
//! Every later search/scoring task implements a trait defined here, in the
//! consumer (conventions rule 2): signals (tasks 65/67) implement [`Sensor`] and
//! [`CellFn`], the matcher DSL (task 66) adapts record types via [`Matchable`],
//! search policies (task 70) implement [`Selector`], entropy tactics (tasks
//! 71/72) implement [`Tactic`], and oracles (task 75) implement [`Oracle`]. The
//! engine composes them; the behavior-equivalence defaults live in
//! [`crate::defaults`].
//!
//! ## The organizing split (docs/EXPLORATION.md)
//!
//! - **Live plane** — touches the guest at branch speed: the
//!   [`Machine`](crate::Machine) and the [`Tactic`] (the inner loop's
//!   decision-answering policy). Only these cost VM time.
//! - **Replay plane** — pure or folded functions of a serialized run:
//!   [`Sensor`], [`CellFn`], [`Oracle`] (pure per run) and [`Archive`],
//!   [`Selector`] (stateful folds over the run sequence).
//!
//! ## The load-bearing invariants
//!
//! 1. **Open-loop `Tactic`.** [`Tactic::decide`] receives only the tactic's own
//!    state, the [`DecisionPoint`], and its PRNG — structurally, there is no
//!    parameter through which live `Sensor`/`Archive` output could reach a
//!    decision. Intra-run steering is recovered by checkpointing (seal, then
//!    fuzz from the snapshot), never by live feedback.
//! 2. **Timeline admission.** [`Archive::admit`] walks the run's sealable
//!    timeline and admits a [`VirtualExemplar`] at every novel `(cell, Moment)`;
//!    one run contributes many exemplars, and the archive is bounded by
//!    **distinct cells**, not runs.
//! 3. **Parent-rooted exemplars.** A [`VirtualExemplar`] is `(parent SnapId,
//!    seed, tail-complete suffix, at)`; materialization is `branch(parent)` +
//!    replay the suffix + seal — never a genesis replay. The genesis-complete
//!    [`Bug::env`] folds the suffix chain via
//!    [`EnvCodec::compose`](crate::EnvCodec::compose) (the task-93 ruling).
//! 4. **Eviction is reproducibility-safe.** Dropping any seal/snapshot never
//!    changes what a later run reproduces (an evicted state re-materializes from
//!    genesis, identically); retention is a pure performance knob.
//! 5. **Progression blindness.** [`Selector`] and [`Archive`] see opaque
//!    [`CellKey`]s and [`Reward`]s — no fault types, no signal channels, no
//!    `CellFn` meaning. Later faults and signals grow the seams and never touch
//!    the search policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::prng::Prng;
use crate::{Answer, Environment, SnapId, StopReason};

// ---------------------------------------------------------------------------
// Vocabulary (serializable, `serde`)
// ---------------------------------------------------------------------------

/// A moment on the single monotonic deterministic axis, mirroring
/// `environment::Moment`/`control-proto::Moment` (conventions rule 2 — defined
/// locally, not imported). The spine keys the replay-plane vocabulary on
/// `Moment`; which physical counter backs the axis at integration (the
/// retired-instruction count vs. the retired-branch V-time it is derived from)
/// is the unit ruling escalated to the foreman per task 65 — nothing in this
/// crate depends on the choice. The in-crate toy machine stamps its stop
/// V-times onto this axis one-for-one.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Moment(pub u64);

/// The shape-and-content view of a coverage map (instrument tier): AFL-style
/// edge counts, snapshotted from the backend's map. Only a view — the explorer
/// never interprets its layout beyond generic novelty (Progression blindness);
/// in production the bytes come from the negotiated shmem geometry
/// (`control-proto::CoverageGeometry`), in tests from the toy machine.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CoverageView {
    /// The raw coverage map bytes (edge-indexed hit counts).
    pub map: Vec<u8>,
}

/// A stable channel identifier: which signal tier/plugin a [`Feature`] came
/// from (scrape / link / instrument, then per-plugin). Channel numbering is a
/// campaign convention; the spine only requires stability.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct ChannelId(pub u16);

/// A stable feature identifier within a channel. Fixed-vocabulary sensors emit
/// stable ids directly; open-vocabulary signals (log templates, LSH) are
/// clustered by a codebook **internal to their plugin** — the codebook never
/// leaks into this crate.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct FeatureId(pub u64);

/// One observed signal: a stable `(channel, id)` pair. What a feature *means*
/// is defined by the plugin that emitted it; the Progression never learns.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Feature {
    /// The signal channel this feature belongs to.
    pub channel: ChannelId,
    /// The stable feature identity within the channel.
    pub id: FeatureId,
}

/// The features live at a given [`Moment`] — the point-in-time slice a
/// [`CellFn`] keys. Deterministically ordered (a `BTreeSet` underneath), so no
/// iteration order can reach a [`CellKey`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct FeatureSet {
    features: std::collections::BTreeSet<Feature>,
}

impl FeatureSet {
    /// An empty slice.
    pub fn new() -> Self {
        Self::default()
    }

    /// The slice holding exactly one feature.
    pub fn singleton(f: Feature) -> Self {
        let mut features = std::collections::BTreeSet::new();
        features.insert(f);
        Self { features }
    }

    /// Insert a feature; returns whether it was newly present.
    pub fn insert(&mut self, f: Feature) -> bool {
        self.features.insert(f)
    }

    /// Whether the slice holds `f`.
    pub fn contains(&self, f: &Feature) -> bool {
        self.features.contains(f)
    }

    /// The features, in their canonical (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = &Feature> {
        self.features.iter()
    }

    /// The number of features in the slice.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }
}

impl FromIterator<Feature> for FeatureSet {
    fn from_iter<I: IntoIterator<Item = Feature>>(iter: I) -> Self {
        Self {
            features: iter.into_iter().collect(),
        }
    }
}

/// A cell key: opaque bytes to the Progression, `Ord` for BTree keying. What a
/// cell *means* is the [`CellFn`]'s business alone (invariant 5).
pub type CellKey = Vec<u8>;

/// An attribute value a [`Matchable`] record exposes to the matcher DSL (task
/// 66). Deliberately **no floating-point variant**: anything that can reach
/// state-affecting math stays integer/rational (conventions rule 4).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Value {
    /// A boolean attribute.
    Bool(bool),
    /// A signed integer attribute.
    Int(i64),
    /// An unsigned integer attribute.
    UInt(u64),
    /// A string attribute.
    Str(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
}

/// A decoded link-tier guest event (SDK assertions, state registers, buggify
/// results). The stream stays **empty until task 73** wires the guest SDK; the
/// slot exists so [`RunTrace`]'s shape is fixed now. `kind` + sorted `attrs`
/// make any event adaptable to [`Matchable`] by the channel plugin.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GuestEvent {
    /// The event kind (an SDK-defined discriminator, e.g. `"assert_sometimes"`).
    pub kind: String,
    /// The event attributes, deterministically ordered.
    pub attrs: BTreeMap<String, Value>,
}

/// A decoded scrape-tier record (log lines, OTel spans, k8s events). The stream
/// stays **empty until task 65** builds the RunTrace recorder; the slot exists
/// so [`RunTrace`]'s shape is fixed now.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    /// The record kind (a scrape-defined discriminator, e.g. `"log"`, `"span"`).
    pub kind: String,
    /// The record attributes, deterministically ordered.
    pub attrs: BTreeMap<String, Value>,
}

/// One run, decoded and serializable — the unit the replay plane operates on.
/// Because a run is a pure function of its `env`, the full trace is
/// *regenerable* by replay on demand; persisting the tiny `env` is always
/// enough.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RunTrace {
    /// The terminal stop the run ended at.
    pub terminal: StopReason,
    /// The **genesis-complete** reproducer: `branch(genesis, env)` + re-run
    /// reproduces the run bit-for-bit. A run branched below a non-genesis
    /// exemplar is rebased through the exemplar's suffix chain
    /// ([`EnvCodec::compose`](crate::EnvCodec::compose), the task-93 ruling)
    /// before it is recorded here.
    pub env: Environment,
    /// The instrument-tier coverage map, snapshotted at run end. Coverage is an
    /// accumulated bitmap available (in production) only at run end, so it is a
    /// **terminal** signal — never blended into along-timeline cell keys.
    pub coverage: Option<CoverageView>,
    /// The link-tier event stream (decoded SDK) — **empty until task 73**.
    pub events: Vec<(Moment, GuestEvent)>,
    /// The scrape-tier record stream (decoded logs/spans/events) — **empty
    /// until task 65**.
    pub records: Vec<(Moment, Record)>,
}

/// A frontier entry's exemplar: **parent-rooted**, so materialization replays
/// only the suffix (never genesis). Kilobytes, not a snapshot: `parent` is a
/// provenance handle to an already-sealed ancestor, and dropping that ancestor
/// is always safe — the state re-materializes from genesis via the folded
/// suffix chain, identically (invariant 4).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VirtualExemplar {
    /// An already-sealed ancestor (or genesis). Ephemeral pool handle: cheap
    /// materialization when live, never part of the portable artifact.
    pub parent: SnapId,
    /// The campaign draw that minted this run's environment (the fresh seed of
    /// an explore step, the mutation salt of an exploit step) — provenance for
    /// the schema-blind engine; the authoritative reproducer is `suffix`.
    pub seed: u64,
    /// The tail-complete delta since `parent` (the compose contract,
    /// `docs/DISSONANCE.md` task 93): replaying it from `parent` reaches `at`.
    pub suffix: Environment,
    /// Where to seal within the branch (the sealable point this exemplar
    /// addresses).
    pub at: Moment,
}

/// A reportable bug: a genesis-complete reproducer, the stop that defines it,
/// and a stable fingerprint for dedup.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bug {
    /// The genesis-complete reproducer: `branch(genesis, env)` + re-run
    /// reproduces `stop` bit-for-bit.
    pub env: Environment,
    /// The bug's stop reason (a `Crash` or `Assertion`).
    pub stop: StopReason,
    /// A stable digest of the stop reason, for dedup across the many
    /// environments that reach the same bug.
    pub fingerprint: [u8; 32],
}

/// One sealable point a run passed, captured **live** by the engine: the
/// parent-rooted exemplar material plus the signal view as of that point. The
/// replay plane cannot mint these itself — slicing the run's `env` at a moment
/// is schema-aware ([`EnvCodec`](crate::EnvCodec) territory) and the suffix is
/// emitted by the machine at the fork — so the engine hands them to
/// [`Archive::admit`] alongside the [`RunTrace`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Fork {
    /// The parent-rooted exemplar for this sealable point.
    pub exemplar: VirtualExemplar,
    /// The **genesis-complete** environment reaching `exemplar.at` (the suffix
    /// chain already folded via `compose`). Opaque to the archive; stored so a
    /// later mutation or bug rebase keys from the right origin, and so an
    /// evicted seal re-materializes from genesis.
    pub env: Environment,
    /// The coverage view as of this point, when the backing exposes one (the
    /// toy machine does; a shmem-backed production map may only be terminal).
    pub coverage: Option<CoverageView>,
}

/// The outer-loop score signal: what a run's admission was worth. Opaque to the
/// [`Selector`] beyond magnitude comparison (invariant 5); integer-only
/// (conventions rule 4).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Reward {
    /// How many previously-unoccupied cells the run's exemplars claimed.
    pub new_cells: u64,
}

/// A surfaced decision, as the inner loop hands it to a [`Tactic`]: the moment,
/// the decision identity, and the opaque service↔policy context bytes. This is
/// deliberately the **whole** live input surface of a tactic — no coverage, no
/// sensor output, no archive state can reach a decision (invariant 1).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecisionPoint {
    /// The moment the decision surfaced.
    pub at: Moment,
    /// The decision identity (opaque; the toy machine uses the absolute index).
    pub id: u64,
    /// Opaque service↔policy context bytes.
    pub ctx: Vec<u8>,
}

// ---------------------------------------------------------------------------
// The frontier
// ---------------------------------------------------------------------------

/// A stable reference to a frontier entry (its admission-order index). Opaque
/// enough for a [`Selector`] — it carries no cell meaning — while staying
/// `Copy`/`Ord` for deterministic bookkeeping.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ExemplarRef(pub usize);

/// One admitted frontier entry: the exemplar, its genesis-complete environment
/// (memoized suffix-chain fold), and the reward its admission earned.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrontierEntry {
    /// The parent-rooted exemplar.
    pub exemplar: VirtualExemplar,
    /// The genesis-complete environment reaching `exemplar.at` (opaque bytes;
    /// the compose base for children and the re-materialization reproducer).
    pub env: Environment,
    /// The admission reward (how many cells this entry claimed when admitted).
    pub reward: Reward,
}

/// The Go-Explore/MAP-Elites frontier: admitted exemplars in admission order,
/// plus the cell index mapping each occupied [`CellKey`] to its (best)
/// occupant. Deterministic by construction — a `Vec` and a `BTreeMap`, no
/// iteration-order surface. Dumb indexed storage: *which* exemplar occupies a
/// cell (domination) is the [`Archive`]'s policy, *which* to branch from next
/// is the [`Selector`]'s.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Frontier {
    entries: Vec<FrontierEntry>,
    // Serialized as a pair sequence: a byte-vector map key is fine for the
    // BTree but not for string-keyed formats like JSON.
    #[serde(
        serialize_with = "serialize_cells",
        deserialize_with = "deserialize_cells"
    )]
    cells: BTreeMap<CellKey, ExemplarRef>,
}

/// Serialize the cell index as an ordered pair sequence (JSON-compatible).
fn serialize_cells<S: serde::Serializer>(
    cells: &BTreeMap<CellKey, ExemplarRef>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_seq(cells.iter())
}

/// Deserialize the cell index from its pair-sequence form.
fn deserialize_cells<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<CellKey, ExemplarRef>, D::Error> {
    let pairs: Vec<(CellKey, ExemplarRef)> = Vec::deserialize(d)?;
    Ok(pairs.into_iter().collect())
}

impl Frontier {
    /// An empty frontier.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of admitted entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been admitted.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of occupied cells. Admission claims at least one fresh cell
    /// per entry, so `len() <= occupied_cells()` — the archive is bounded by
    /// distinct cells, not runs (invariant 2).
    pub fn occupied_cells(&self) -> usize {
        self.cells.len()
    }

    /// The entry behind `r`, if it exists.
    pub fn get(&self, r: ExemplarRef) -> Option<&FrontierEntry> {
        self.entries.get(r.0)
    }

    /// The `i % len`-th entry in admission order — the deterministic pick a
    /// salt-indexed selector uses. `None` on an empty frontier.
    pub fn nth(&self, i: u64) -> Option<ExemplarRef> {
        if self.entries.is_empty() {
            return None;
        }
        Some(ExemplarRef((i % self.entries.len() as u64) as usize))
    }

    /// Every entry with its reference, in admission order.
    pub fn iter(&self) -> impl Iterator<Item = (ExemplarRef, &FrontierEntry)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (ExemplarRef(i), e))
    }

    /// The current occupant of `cell`, if any.
    pub fn occupant(&self, cell: &CellKey) -> Option<ExemplarRef> {
        self.cells.get(cell).copied()
    }

    /// Append an entry (admission order) and return its reference. An archive
    /// pairs this with [`claim`](Frontier::claim) / [`occupy`](Frontier::occupy)
    /// per its domination policy.
    pub fn insert(&mut self, entry: FrontierEntry) -> ExemplarRef {
        let r = ExemplarRef(self.entries.len());
        self.entries.push(entry);
        r
    }

    /// Claim `cell` for `r` **iff unoccupied** (first-wins); returns whether the
    /// cell was newly claimed.
    pub fn claim(&mut self, cell: CellKey, r: ExemplarRef) -> bool {
        match self.cells.get(&cell) {
            Some(_) => false,
            None => {
                self.cells.insert(cell, r);
                true
            }
        }
    }

    /// Set `cell`'s occupant to `r` unconditionally — the best-per-cell
    /// **domination** primitive (replace when a dominating exemplar arrives).
    /// Returns the previous occupant, if any.
    pub fn occupy(&mut self, cell: CellKey, r: ExemplarRef) -> Option<ExemplarRef> {
        self.cells.insert(cell, r)
    }
}

// ---------------------------------------------------------------------------
// Replay-plane traits — pure per run
// ---------------------------------------------------------------------------

/// Raw-observable → timestamped features. One [`RunTrace`] yields a **stream**,
/// not a terminal set: a run passes through many interesting states, and each
/// feature is stamped with the moment it was observed. Pure: same trace, same
/// stream.
pub trait Sensor {
    /// Derive the timestamped feature stream of one run.
    fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)>;
}

/// Point-in-time feature slice → cell key. **The one campaign-defined stage**;
/// everything downstream ([`Archive`], [`Selector`]) is generic and never
/// learns what a cell means. Iterated hardest of the whole pipeline (the cell
/// abstraction is the hard problem), which is why it is isolated to one pure
/// trait.
pub trait CellFn {
    /// Key the slice of features live at `at` into an opaque cell.
    fn key(&self, at: Moment, feats: &FeatureSet) -> CellKey;
}

/// A trace oracle: a pure verdict over a finished run (crash,
/// always-assertion, Elle-over-history). Re-running a **new** oracle over
/// recorded runs finds real bugs — the strong offline property. Probe oracles
/// (liveness, `eventually`) are *not* this trait: they run forward from a
/// state on a throwaway branch and belong to the live plane (task 75).
pub trait Oracle {
    /// Judge a finished run; `Some` exactly when it exhibits a bug.
    fn judge(&self, t: &RunTrace) -> Option<Bug>;
}

// ---------------------------------------------------------------------------
// Replay-plane folds — stateful across the run sequence
// ---------------------------------------------------------------------------

/// The Go-Explore/MAP-Elites frontier fold: cells, best-per-cell exemplars,
/// reproducibility-safe eviction. Generic — sees opaque [`CellKey`]s and
/// [`Environment`]s, never fault types or signal meaning (invariant 5).
pub trait Archive {
    /// Admit along the **whole timeline**: walk the run's sealable points and
    /// admit a [`VirtualExemplar`] at every novel `(cell, Moment)` — one run
    /// contributes many exemplars. Admission consults
    /// [`admissible`](Archive::admissible); best-per-cell domination decides
    /// replacement. Returns the run's [`Reward`].
    ///
    /// `forks` is the live-captured sealable-point material (the spec's
    /// parameter lists may vary where the semantics hold): the suffix at a
    /// moment is emitted by the machine at the fork and the chain fold is
    /// schema-aware, so the replay plane cannot reconstruct either from `t`
    /// alone. `cells` keys the per-moment feature slices; `sensors` derive
    /// timeline features from the trace's (task 65+/73+) event and record
    /// streams.
    fn admit(
        &mut self,
        t: &RunTrace,
        forks: &[Fork],
        cells: &dyn CellFn,
        sensors: &[Box<dyn Sensor>],
    ) -> Reward;

    /// Whether a sealable point at `at` may be admitted. Injected at
    /// construction; **default always-true**. If task 63 rules RESTRICTED, its
    /// `sealable(Moment)` plugs in here — with zero spine change.
    fn admissible(&self, at: Moment) -> bool;

    /// Enforce the retention policy (best-per-cell trimming, seal-cost GC).
    /// Must be **reproducibility-safe** (invariant 4): evicting never changes
    /// what a later run reproduces, only what it costs.
    fn evict(&mut self);

    /// The current frontier.
    fn frontier(&self) -> &Frontier;
}

/// The outer-loop policy: which exemplar to branch from next. Generic — never
/// sees cell meaning, only the opaque frontier and rewards (invariant 5).
pub trait Selector {
    /// Choose the exemplar to branch from next, or `None` to explore fresh
    /// from genesis. `rng` is the caller-seeded campaign stream (mirroring
    /// [`Tactic::decide`] — a stochastic policy draws from it, deterministic
    /// given the stream state).
    fn choose(&mut self, frontier: &Frontier, rng: &mut Prng) -> Option<ExemplarRef>;

    /// Feed back the reward the chosen exemplar's run earned (the bandit hook,
    /// task 70). Called once per exploit step, after admission.
    fn reward(&mut self, chosen: ExemplarRef, r: Reward);
}

// ---------------------------------------------------------------------------
// Live-plane trait — the inner loop's answering policy
// ---------------------------------------------------------------------------

/// A **stateful distribution** over surfaced decisions; **open-loop** — never
/// reads `Sensor`/`CellFn`/`Archive` output mid-run (invariant 1, enforced
/// structurally: these are the entire inputs). Identical `(state, point, rng)`
/// yields identical answers, whatever concurrent runs do; all feedback-driven
/// adaptation happens *between* runs, in the outer loop.
pub trait Tactic {
    /// Answer one surfaced decision from the tactic's own state, the point, and
    /// the caller-seeded PRNG.
    fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer;
}

// ---------------------------------------------------------------------------
// For the matcher DSL (task 66) to adapt any record type
// ---------------------------------------------------------------------------

/// Adapts any record type to the matcher DSL (task 66): a kind discriminator,
/// attribute lookup, and the moment the record was observed. Channel plugins
/// implement this for their decoded record types; the generic
/// `MatchSensor`/`MatchOracle` operate over `dyn Matchable`.
pub trait Matchable {
    /// The record's kind discriminator (e.g. `"log"`, `"span"`, an SDK event
    /// name).
    fn kind(&self) -> &str;

    /// Look up an attribute by key; `None` when absent.
    fn attr(&self, k: &str) -> Option<Value>;

    /// The moment the record was observed.
    fn moment(&self) -> Moment;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VTime;

    fn env(bytes: Vec<u8>) -> Environment {
        Environment {
            blob_version: 1,
            bytes,
        }
    }

    /// The serializable vocabulary round-trips through serde losslessly (the
    /// "serializable, serde" contract of the spec's vocabulary section).
    #[test]
    fn vocabulary_round_trips_through_serde() {
        let trace = RunTrace {
            terminal: StopReason::Crash {
                vtime: VTime(80),
                info: vec![2, 4],
            },
            env: env(vec![1, 2, 3]),
            coverage: Some(CoverageView { map: vec![0, 1, 9] }),
            events: vec![(
                Moment(40),
                GuestEvent {
                    kind: "assert_sometimes".into(),
                    attrs: [("hit".into(), Value::Bool(true))].into_iter().collect(),
                },
            )],
            records: vec![(
                Moment(50),
                Record {
                    kind: "log".into(),
                    attrs: [("lsn".into(), Value::UInt(7))].into_iter().collect(),
                },
            )],
        };
        let json = serde_json::to_string(&trace).expect("serialize");
        let back: RunTrace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(trace, back);

        let ex = VirtualExemplar {
            parent: SnapId(3),
            seed: 99,
            suffix: env(vec![9]),
            at: Moment(40),
        };
        let back: VirtualExemplar =
            serde_json::from_str(&serde_json::to_string(&ex).expect("ser")).expect("de");
        assert_eq!(ex, back);

        let bug = Bug {
            env: env(vec![5]),
            stop: StopReason::Assertion {
                vtime: VTime(50),
                id: 5,
                data: vec![3],
            },
            fingerprint: [7u8; 32],
        };
        let back: Bug =
            serde_json::from_str(&serde_json::to_string(&bug).expect("ser")).expect("de");
        assert_eq!(bug, back);

        let frontier = {
            let mut f = Frontier::new();
            let r = f.insert(FrontierEntry {
                exemplar: ex,
                env: env(vec![9]),
                reward: Reward { new_cells: 2 },
            });
            f.claim(vec![0, 1], r);
            f
        };
        let back: Frontier =
            serde_json::from_str(&serde_json::to_string(&frontier).expect("ser")).expect("de");
        assert_eq!(frontier, back);
    }

    /// `Frontier` bookkeeping: admission order, first-wins claims, domination
    /// via `occupy`, and the cell bound.
    #[test]
    fn frontier_orders_claims_and_dominates() {
        let mut f = Frontier::new();
        assert!(f.is_empty());
        assert_eq!(f.nth(0), None);

        let e = |seed: u64| FrontierEntry {
            exemplar: VirtualExemplar {
                parent: SnapId(1),
                seed,
                suffix: env(vec![]),
                at: Moment(40),
            },
            env: env(vec![]),
            reward: Reward { new_cells: 1 },
        };
        let r0 = f.insert(e(0));
        let r1 = f.insert(e(1));
        assert_eq!((r0, r1), (ExemplarRef(0), ExemplarRef(1)));
        assert_eq!(f.len(), 2);

        // Salt-indexed pick wraps in admission order.
        assert_eq!(f.nth(0), Some(r0));
        assert_eq!(f.nth(3), Some(r1));

        // First-wins claim: the second claimant is refused.
        assert!(f.claim(vec![7], r0));
        assert!(!f.claim(vec![7], r1));
        assert_eq!(f.occupant(&vec![7]), Some(r0));

        // Domination replaces unconditionally and reports the loser.
        assert_eq!(f.occupy(vec![7], r1), Some(r0));
        assert_eq!(f.occupant(&vec![7]), Some(r1));

        // One cell, two entries: entries can exceed cells only through
        // domination history, never through admission (the archive claims at
        // least one fresh cell per admitted entry).
        assert_eq!(f.occupied_cells(), 1);
    }

    /// `FeatureSet` is canonically ordered and deduplicated.
    #[test]
    fn feature_set_is_canonical() {
        let f1 = Feature {
            channel: ChannelId(0),
            id: FeatureId(9),
        };
        let f2 = Feature {
            channel: ChannelId(0),
            id: FeatureId(1),
        };
        let mut s = FeatureSet::new();
        assert!(s.insert(f1));
        assert!(s.insert(f2));
        assert!(!s.insert(f1), "duplicates are refused");
        assert_eq!(s.len(), 2);
        assert!(s.contains(&f1));
        let order: Vec<u64> = s.iter().map(|f| f.id.0).collect();
        assert_eq!(order, vec![1, 9], "iteration is sorted, not insertion");
        assert_eq!(FeatureSet::singleton(f1).len(), 1);
        assert!(FeatureSet::new().is_empty());
    }
}
