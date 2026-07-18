// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **surviving search-plane vocabulary and control traits**.
//!
//! Task 64 minted this spine; task 132 M3 physically deleted its legacy
//! compat half (`Archive::admit`, `Sensor`, `CellFn`, `Feature`,
//! `FeatureId`, `FeatureSet`, `ChannelId`, `Fork`) — the Differential
//! observation plane owns production observation/cell currency
//! (`docs/DISSONANCE-STRATEGY.md`, "Fate of the current spine interfaces").
//! What remains are the durable seams:
//!
//! - **Control interfaces**: [`Tactic`] (open-loop, single-pass answering
//!   policy) and [`Selector`] (entry choice) — the live-plane boundaries the
//!   strategy keeps;
//! - **[`Oracle`]**: the pure completed-run judgment boundary (its
//!   [`RunTrace`] carrier remains the compatibility currency for the
//!   scrape/trace consumers until `RunTrace` is versioned into the
//!   ledger-backed evidence view);
//! - **[`Matchable`]**: the matcher-DSL record adapter (task 66);
//! - the **archive read model**: [`Frontier`], [`FrontierEntry`],
//!   [`ExemplarRef`], [`VirtualExemplar`], [`CellKey`], [`Reward`] — the
//!   selector-facing materialized view the two-barrier campaign maintains;
//! - the **evidence vocabulary**: [`Moment`], [`EvidenceCut`], [`RunTrace`],
//!   [`Record`], [`StreamId`], [`GuestEvent`], [`CoverageView`], [`Value`],
//!   [`Bug`], [`DecisionPoint`].
//!
//! ## The load-bearing invariants
//!
//! 1. **Open-loop `Tactic`.** [`Tactic::decide`] receives only the tactic's own
//!    state, the [`DecisionPoint`], and its PRNG — structurally, there is no
//!    parameter through which live observation could reach a decision.
//! 2. **Parent-rooted exemplars.** A [`VirtualExemplar`] is `(parent SnapId,
//!    seed, tail-complete suffix, cut)`; materialization is `branch(parent)` +
//!    replay the suffix + seal — never a genesis replay. A genesis-complete
//!    reproducer folds the suffix chain via
//!    [`EnvCodec::compose`](crate::EnvCodec::compose) (the task-93 ruling).
//! 3. **Eviction is reproducibility-safe.** Dropping any seal/snapshot never
//!    changes what a later run reproduces (an evicted state re-materializes from
//!    genesis, identically); retention is a pure performance knob.
//! 4. **Search-loop blindness.** [`Selector`]s see opaque [`CellKey`]s and
//!    [`Reward`]s — no fault types, no observation meaning.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::prng::Prng;
use crate::{Answer, Reproducer, SnapId, StopReason};

// ---------------------------------------------------------------------------
// Vocabulary (serializable, `serde`)
// ---------------------------------------------------------------------------

/// A point on the single monotonic deterministic axis, mirroring
/// `environment::Moment`/`control-proto::Moment` (conventions rule 2 — defined
/// locally, not imported). The spine keys the replay-plane vocabulary on
/// `Moment`, and the engine's deadlines and stop stamps are the same type —
/// one axis (the GLOSSARY ruling that settled the task-65 escalation). The
/// in-crate toy machine stamps its stop V-times onto this axis one-for-one.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Moment(pub u64);

/// A seal's **evidence cut** (task 127, bead `hm-bbx.6`): the server-stamped
/// binding of a successful seal to its exact evidence boundary — the
/// synchronized seal [`Moment`] plus the **included SDK-event count**, the
/// ordered SDK-capture vector's prefix length at the seal. Mirrors the
/// `control-proto` snapshot reply's cut fields (conventions rule 2 — defined
/// locally, not imported); a [`Machine`] test double stamps its own from the
/// same state its seal captures.
///
/// **Half-open, by prefix length — never by `Moment` comparison**: persisted
/// SDK-capture positions `< sdk_events` are included (including the exact
/// subset emitted *at* the seal's `Moment`); positions `>= sdk_events` are
/// excluded. Several events may share one stamped `Moment`, so the count is
/// the only exact boundary. The stamp is captured **with** the seal by
/// whatever produced it — the sole authority; nothing downstream reconstructs
/// it with a second read. Console/scrape bytes are a separate source-local
/// stream with no cursor here, so they are structurally unable to enter
/// `sdk_events`; a later seal-relative source gets its own declared cursor,
/// and independent cursors never imply cross-source order.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct EvidenceCut {
    /// The synchronized seal `Moment` — where the seal was taken on the
    /// deterministic axis.
    pub at: Moment,
    /// The included SDK-event count: the SDK capture vector's prefix length
    /// at the seal (`0` for a machine with no SDK channel).
    pub sdk_events: u64,
}

/// The shape-and-content view of a coverage map (instrument tier): AFL-style
/// edge counts, snapshotted from the backend's map. Only a view — the explorer
/// never interprets its layout beyond generic novelty (search-loop blindness);
/// in production the bytes come from the negotiated shmem geometry
/// (`control-proto::CoverageGeometry`), in tests from the toy machine.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CoverageView {
    /// The raw coverage map bytes (edge-indexed hit counts).
    pub map: Vec<u8>,
}

/// A cell key: opaque bytes to the search loop, `Ord` for BTree keying. What a
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
/// results). Populated by the task-73 `link` plugin, which decodes the raw
/// `(Moment, event_id, bytes)` the guest SDK emits into this typed form. `kind` +
/// sorted `attrs` make any event adaptable to [`Matchable`] by the channel
/// plugin (a `link`-local newtype does so — orphan rules keep the impl there).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GuestEvent {
    /// The event kind (an SDK-defined discriminator, e.g. `"assert_sometimes"`).
    pub kind: String,
    /// The event attributes, deterministically ordered.
    pub attrs: BTreeMap<String, Value>,
}

/// A stable identifier for a raw byte stream a run emits: which console/log
/// source a [`Record`] was scraped from — the guest serial console, a
/// per-container log, an ingested telemetry `Console` recording. Stream
/// numbering is a campaign convention (mirroring [`ChannelId`]); the recorder
/// only requires stability, so equal runs stamp equal ids.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct StreamId(pub u16);

/// A decoded scrape-tier record: **raw and structural** — the concrete shape
/// task 65 pins here (task 64 named the `records` slot in [`RunTrace`] but its
/// fixed vocabulary left the record's shape unpinned). One record is exactly one
/// newline-delimited line of a [`StreamId`]'s byte stream. `line` retains its
/// trailing `\n` when the line was terminated, so a stream's records are a
/// **lossless partition** of its bytes — every input byte lands in exactly one
/// record, and a trailing unterminated line simply keeps no terminator. The
/// bytes are kept **verbatim**: UTF-8-lossy decoding is a display concern, never
/// applied to what is stored. Structural meaning (log vs. span, parsed fields)
/// is a channel plugin's codebook (task 67), which *consumes* records — the
/// recorder never produces anything richer than bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    /// Which byte stream this line was scraped from.
    pub stream: StreamId,
    /// The exact line bytes, including the terminating `\n` when present.
    pub line: Vec<u8>,
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
    pub env: Reproducer,
    /// The instrument-tier coverage map, snapshotted at run end. Coverage is an
    /// accumulated bitmap available (in production) only at run end, so it is a
    /// **terminal** signal — never blended into along-timeline cell keys.
    pub coverage: Option<CoverageView>,
    /// The link-tier event stream (decoded SDK) — populated by task 73's `link`
    /// plugin from the guest SDK's Event emissions (empty for a run with no
    /// cooperating SDK).
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
    /// `docs/DISSONANCE.md` task 93): replaying it from `parent` reaches
    /// `cut.at`.
    pub suffix: Reproducer,
    /// The seal's server-stamped [`EvidenceCut`] (task 127): where to seal
    /// within the branch (`cut.at` — the sealable point this exemplar
    /// addresses) **and** the included SDK-event count at that seal. Stamped
    /// by the machine at the fork's eager seal and carried through the
    /// persisted frontier/lineage — never reconstructed by a second read.
    pub cut: EvidenceCut,
}

/// A reportable bug: a genesis-complete reproducer, the stop that defines it,
/// and a stable fingerprint for dedup.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bug {
    /// The genesis-complete reproducer: `branch(genesis, env)` + re-run
    /// reproduces `stop` bit-for-bit.
    pub env: Reproducer,
    /// The bug's stop reason (a `Crash` or `Assertion`).
    pub stop: StopReason,
    /// A stable digest of the stop reason, for dedup across the many
    /// environments that reach the same bug.
    pub fingerprint: [u8; 32],
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

/// A **stable identity** for a frontier entry: a monotonic id minted at
/// admission, **never reused and never renumbered** — it survives any
/// [`Archive::evict`] compaction, so engine-side bookkeeping keyed by it (the
/// seal cache) can never be desynced onto a different exemplar by eviction.
/// A ref whose entry has been evicted simply stops resolving
/// ([`Frontier::get`] returns `None`); it never aliases a survivor. Opaque
/// enough for a [`Selector`] — it carries no cell meaning — while staying
/// `Copy`/`Ord` for deterministic bookkeeping.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ExemplarRef(pub u64);

/// One admitted frontier entry: the exemplar, its genesis-complete environment
/// (memoized suffix-chain fold), and the reward its admission earned.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrontierEntry {
    /// The parent-rooted exemplar.
    pub exemplar: VirtualExemplar,
    /// The genesis-complete environment reaching `exemplar.at` (opaque bytes;
    /// the compose base for children and the re-materialization reproducer).
    pub env: Reproducer,
    /// The admission reward (how many cells this entry claimed when admitted).
    pub reward: Reward,
}

/// The Go-Explore/MAP-Elites frontier: live exemplars in admission order under
/// **stable ids** ([`ExemplarRef`]), plus the cell index mapping each occupied
/// [`CellKey`] to its (best) occupant. Deterministic by construction — a `Vec`
/// and a `BTreeMap`, no iteration-order surface. Dumb indexed storage: *which*
/// exemplar occupies a cell (domination) and *what* to [`remove`](Frontier::remove)
/// (eviction) is the [`Archive`]'s policy, *which* to branch from next is the
/// [`Selector`]'s.
///
/// A cell claim **outlives its occupant**: removing an entry does not clear the
/// cells it claimed (novelty must not reset — an evicted behaviour is still a
/// *seen* behaviour), so [`occupant`](Frontier::occupant) may name a ref that
/// no longer [`get`](Frontier::get)s. Dead refs never alias a live entry.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Frontier {
    /// Live entries in admission order, each under its stable id (ascending —
    /// ids are minted monotonically and removal preserves order).
    entries: Vec<(ExemplarRef, FrontierEntry)>,
    // Serialized as a pair sequence: a byte-vector map key is fine for the
    // BTree but not for string-keyed formats like JSON.
    #[serde(
        serialize_with = "serialize_cells",
        deserialize_with = "deserialize_cells"
    )]
    cells: BTreeMap<CellKey, ExemplarRef>,
    /// The next id to mint; never decremented, so ids are never reused.
    next: u64,
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

    /// The **live** entry behind `r`: `None` once `r` has been
    /// [`remove`](Frontier::remove)d (a dead ref never aliases a survivor).
    /// Binary search over the ascending id order.
    pub fn get(&self, r: ExemplarRef) -> Option<&FrontierEntry> {
        self.entries
            .binary_search_by_key(&r, |(id, _)| *id)
            .ok()
            .map(|i| &self.entries[i].1)
    }

    /// The `i % len`-th **live** entry in admission order — the deterministic
    /// pick a salt-indexed selector uses. `None` on an empty frontier.
    pub fn nth(&self, i: u64) -> Option<ExemplarRef> {
        if self.entries.is_empty() {
            return None;
        }
        Some(self.entries[(i % self.entries.len() as u64) as usize].0)
    }

    /// Every live entry with its stable ref, in admission order.
    pub fn iter(&self) -> impl Iterator<Item = (ExemplarRef, &FrontierEntry)> {
        self.entries.iter().map(|(id, e)| (*id, e))
    }

    /// The current occupant of `cell`, if any — possibly a ref whose entry has
    /// since been evicted (the claim outlives the occupant; see the type docs).
    pub fn occupant(&self, cell: &CellKey) -> Option<ExemplarRef> {
        self.cells.get(cell).copied()
    }

    /// Append an entry (admission order) under a **freshly minted stable id**
    /// and return it. An archive pairs this with [`claim`](Frontier::claim) /
    /// [`occupy`](Frontier::occupy) per its domination policy.
    pub fn insert(&mut self, entry: FrontierEntry) -> ExemplarRef {
        let r = ExemplarRef(self.next);
        self.next += 1;
        self.entries.push((r, entry));
        r
    }

    /// Remove `r`'s entry — the **eviction primitive** ([`Archive::evict`]
    /// implementations call this). Returns the removed entry, or `None` if `r`
    /// was not live. Cell claims held by `r` are deliberately **not** cleared
    /// (novelty never resets); surviving entries keep their ids, so references
    /// held elsewhere (e.g. the engine's seal cache) stay exact — a dead ref
    /// stops resolving rather than silently renaming another entry.
    pub fn remove(&mut self, r: ExemplarRef) -> Option<FrontierEntry> {
        match self.entries.binary_search_by_key(&r, |(id, _)| *id) {
            Ok(i) => Some(self.entries.remove(i).1),
            Err(_) => None,
        }
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
    use crate::Moment;

    fn env(bytes: Vec<u8>) -> Reproducer {
        Reproducer {
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
                vtime: Moment(80),
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
                    stream: StreamId(0),
                    line: b"lsn=7\n".to_vec(),
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
            cut: EvidenceCut {
                at: Moment(40),
                sdk_events: 2,
            },
        };
        let back: VirtualExemplar =
            serde_json::from_str(&serde_json::to_string(&ex).expect("ser")).expect("de");
        assert_eq!(ex, back);

        let bug = Bug {
            env: env(vec![5]),
            stop: StopReason::Assertion {
                vtime: Moment(50),
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
                cut: EvidenceCut {
                    at: Moment(40),
                    sdk_events: 0,
                },
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

    /// Stable identity across eviction: `remove` never renumbers survivors,
    /// never reuses ids, leaves dead refs unresolvable, and keeps cell claims
    /// (novelty never resets). The round-1 review's blocking finding pins here.
    #[test]
    fn frontier_removal_keeps_identities_stable() {
        let mut f = Frontier::new();
        let e = |seed: u64| FrontierEntry {
            exemplar: VirtualExemplar {
                parent: SnapId(1),
                seed,
                suffix: env(vec![]),
                cut: EvidenceCut {
                    at: Moment(40),
                    sdk_events: 0,
                },
            },
            env: env(vec![]),
            reward: Reward { new_cells: 1 },
        };
        let r0 = f.insert(e(0));
        let r1 = f.insert(e(1));
        let r2 = f.insert(e(2));
        f.claim(vec![0], r0);
        f.claim(vec![1], r1);
        f.claim(vec![2], r2);

        // Evict the middle entry.
        let removed = f.remove(r1).expect("r1 was live");
        assert_eq!(removed.exemplar.seed, 1);
        assert_eq!(f.remove(r1), None, "a dead ref removes nothing twice");
        assert_eq!(f.len(), 2);

        // Survivors keep their ORIGINAL refs — no renumbering.
        assert_eq!(f.get(r0).expect("r0 live").exemplar.seed, 0);
        assert_eq!(f.get(r2).expect("r2 live").exemplar.seed, 2);
        assert_eq!(f.get(r1), None, "the dead ref stops resolving");

        // Admission-order pick walks the LIVE entries: position 1 is now r2 —
        // as a live ref, never as r1's recycled slot.
        assert_eq!(f.nth(0), Some(r0));
        assert_eq!(f.nth(1), Some(r2));

        // A fresh insert mints a NEW id — r1 is never reused.
        let r3 = f.insert(e(3));
        assert_eq!(r3, ExemplarRef(3));
        assert_ne!(r3, r1);

        // The dead entry's cell claim survives (an evicted behaviour is still
        // a seen behaviour), naming the historical occupant.
        assert_eq!(f.occupant(&vec![1]), Some(r1));

        // And the whole shape round-trips through serde (ids + next counter),
        // so a restored frontier cannot re-mint a dead id either.
        let json = serde_json::to_string(&f).expect("ser");
        let mut back: Frontier = serde_json::from_str(&json).expect("de");
        assert_eq!(f, back);
        assert_eq!(back.insert(e(4)), ExemplarRef(4), "next id survives serde");
    }
}
