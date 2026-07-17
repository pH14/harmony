// SPDX-License-Identifier: AGPL-3.0-or-later
//! # explorer — the coverage-guided exploration engine and the search-plane spine
//!
//! `explorer` is **all of dissonance policy**: the brain that drives a
//! deterministic guest through many environments to find bugs. Task 12 built the
//! engine; task 64 decomposed its god-object `Strategy` into the **search-plane
//! trait spine** (`spine.rs`) every later signal/search/oracle task
//! implements, and generalized its AFL-shaped corpus into a cell [`Archive`].
//! Mutation lives **here**, never in the wire (the AFL lesson): the engine
//! ferries an opaque, versioned [`Reproducer`] blob and mutates it only through
//! the [`EnvCodec`] seam, so the schema (dissonance task 24) stays owned by the
//! catalog and the wire stays fixed independently of it.
//!
//! ## The two loops
//!
//! - **Rollout (inner, [`Explorer::rollout`]):** drive ONE run forward —
//!   `run` ⇄ `run(resolve)` — answering each surfaced [`StopReason::Decision`]
//!   via the **open-loop [`Tactic`]** and capturing every sealable
//!   [`StopReason::SnapshotPoint`] as parent-rooted [`VirtualExemplar`]
//!   material. Ends at a terminal [`StopReason`].
//! - **Search loop (outer, [`Explorer::step`]):** across runs — the
//!   [`Selector`] picks a frontier exemplar (or genesis), the engine
//!   materializes it and mints the next [`Reproducer`] through the codec, runs
//!   one rollout, folds the run into the [`Archive`] (timeline admission),
//!   and judges it with the [`Oracle`]. One search-loop step = one rollout.
//!
//! (These are the loop pair `docs/EXPLORATION.md` also describes; the
//! temporal-axis term of art "timeline admission" is a distinct concept and
//! stays.)
//!
//! ## The seams (defined locally, conventions rule 2)
//!
//! The engine codes against a [`Machine`]/[`MachineFactory`] driver seam and an
//! [`EnvCodec`] minting seam (`seam.rs`), and composes the search-plane
//! traits of `spine.rs` — [`Sensor`], [`CellFn`], [`Oracle`], [`Archive`],
//! [`Selector`], [`Tactic`], [`Matchable`] — whose behavior-equivalence default
//! implementations live in `defaults.rs`. In production the [`mod@adapter`]
//! module's [`SocketMachine`] implements [`Machine`] over a `control-proto`
//! client stream (against vmm-core's control-transport server, task 58) and
//! [`SpecEnvCodec`] binds [`EnvCodec`] to the `environment` crate's real
//! reproducer codec per the task-93 ruling; in tests an in-crate deterministic
//! **toy machine** does both — so the same engine and the same determinism
//! gate run both sides unchanged.
//!
//! ## Determinism discipline
//!
//! Nothing here observes wall-clock time, host entropy, `HashMap`/`HashSet`
//! iteration order, or floating point. The frontier is a `Vec` + `BTreeMap`,
//! every policy draw comes from a caller-seeded [`Prng`], and the [`Bug`]
//! fingerprint is a `sha2` digest of the stop reason — so the same
//! `(campaign seed, machine)` yields a bit-identical exploration trace and an
//! identical admitted frontier.
//!
//! ## Module layout
//!
//! `error.rs` (the [`MachineError`] enum) · `seam.rs` (the [`Machine`],
//! [`MachineFactory`], and [`EnvCodec`] traits) · `spine.rs` (the search-plane
//! vocabulary + traits — the task-64 contract) · `defaults.rs` (the
//! behavior-equivalence default implementations) · `engine.rs` ([`Explorer`],
//! [`Composition`], [`RunOutcome`]) · `materialize.rs` (the task-68 lazy
//! materialization engine: [`Materializer`], the lineage table, and the
//! spanning-ancestor retention pool — [`SealBudget`]) · `prng.rs` (the public
//! xorshift64\* generator the policies draw from) · [`mod@adapter`] (the R2
//! socket adapter: [`SocketMachine`], [`SpecEnvCodec`], and the [`AdapterEnv`]
//! blob — task 58).

pub mod adapter;
mod campaign;
mod defaults;
mod engine;
mod error;
mod evidence;
mod ledger;
mod materialize;
mod occurrence;
mod prng;
mod retention;
mod seam;
mod spine;
pub mod stads;
#[cfg(test)]
pub(crate) mod testkit;

/// Convert an `sdk-events` V-time coordinate to the spine [`Moment`] (they are
/// one-for-one — `sdk-events` mirrors the axis locally to stay dependency-free,
/// so this is a bare newtype re-wrap, never a rescale).
pub(crate) fn sdk_moment_to_spine(m: sdk_events::Moment) -> Moment {
    Moment(m.0)
}

pub use adapter::{ADAPTER_BLOB_VERSION, AdapterEnv, SocketMachine, SpecEnvCodec, client_caps};
pub use campaign::{
    CampaignConfig, CampaignError, DifferentialCampaign, Ingress, Occupied, StepReport,
};
pub use defaults::{
    COVERAGE_CHANNEL, CoverageArchive, DeclineTactic, ExploreExploitSelector, GenesisSelector,
    IdentityCells, TerminalOracle,
};
pub use engine::{Composition, Explorer, RunOutcome};
pub use error::{EnvCodecError, MachineError};
pub use evidence::{
    CompletedRunEvidence, DefaultObservationCells, EvidenceRole, ObservationCells, ObservationMap,
    ReducedValue, RunId, reduce_at_cut,
};
pub use ledger::{EvidenceLedger, LedgerError, PayloadRef, TraceStore};
pub use materialize::{Lineage, Materialization, Materializer, SealBudget};
pub use occurrence::{
    AbsenceFinding, AbsenceLedger, CounterexampleKind, OccurrenceCounterexample, OccurrenceOracle,
};
pub use prng::Prng;
pub use retention::{
    BatchAvailability, CellAssignment, CollectedBatch, CoverageRef, ExpiryOrder, FinalizedSummary,
    FoldOutcome, GcReport, GcSkipReason, RawAvailability, Recomputation, RetentionCheckpoint,
    RetentionError, RetentionProfile, RetentionReport, RetentionViews, WorkingSet,
    WorkingSetUpdate,
};
pub use seam::{EnvCodec, Machine, MachineFactory};
pub use spine::{
    Archive, Bug, CellFn, CellKey, ChannelId, CoverageView, DecisionPoint, EvidenceCut,
    ExemplarRef, Feature, FeatureId, FeatureSet, Fork, Frontier, FrontierEntry, GuestEvent,
    Matchable, Moment, Oracle, Record, Reward, RunTrace, Selector, Sensor, StreamId, Tactic, Value,
    VirtualExemplar,
};

use serde::{Deserialize, Serialize};

/// An ephemeral, pool-wide handle to a captured machine state (a snapshot). It
/// is a transient resource handle — **never** part of a portable reproducer
/// artifact (that is the [`Reproducer`]); the only stable, always-reproducible
/// base is the genesis snapshot from [`Explorer::new`]. The control plane mints
/// these on `snapshot` and frees them on `drop` (pool GC).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct SnapId(pub u64);

/// The reproducer the explorer ferries: an **opaque, versioned blob**. The
/// explorer never parses `bytes` — dissonance task 24 owns the structure — it
/// only moves the blob between [`Machine`], [`EnvCodec`], and the
/// [`Frontier`]. The `blob_version` lets a backend reject a blob it does not
/// understand (`BadEnvVersion`) without the explorer decoding anything.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Reproducer {
    /// The blob format version (task 24's `EnvSpec::BLOB_VERSION`), checked by
    /// the backend, opaque to the explorer.
    pub blob_version: u16,
    /// The opaque, versioned reproducer bytes. Mutated only through
    /// [`EnvCodec`], never byte-flipped in place.
    pub bytes: Vec<u8>,
}

/// One answer to a surfaced [`StopReason::Decision`], opaque service↔policy
/// bytes the [`Tactic`] mints and the [`Machine`] stages into the suspended
/// hypercall on `run(resolve)`. Opaque to the explorer.
///
/// Deliberately **not** `Default`: an "empty answer" (a declining
/// [`DeclineTactic`]) is `Answer(Vec::new())`, which would be indistinguishable
/// from a derived `Default` — leaving a mutation-testing blind spot. Construct
/// the empty answer explicitly instead.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer(pub Vec<u8>);

/// Which decision *classes* a `run` surfaces, mirroring the control-proto /
/// `DecisionClass` bits. Everything not selected the environment answers
/// locally (the seed), so a campaign tunes how reactive a rollout is by which
/// bits it sets. Interpreted by the [`Machine`]; the explorer carries it through
/// unparsed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopMask(pub u32);

impl StopMask {
    /// Surface nothing — the environment's seed answers every decision locally,
    /// so a rollout driven with this mask has zero stops (pure seed-driven
    /// exploration, the search loop alone). The seed-campaign mask, and the mask
    /// the engine re-materializes evicted exemplars under (a re-materialization
    /// is a pinned replay, never a fresh exploration).
    pub const NONE: StopMask = StopMask(0);
    /// Surface every decision class (and the snapshot point) — the
    /// coverage-guided default, so the [`Tactic`] answers each interesting
    /// decision and the explorer can fork a snapshot mid-run.
    pub const ALL: StopMask = StopMask(u32::MAX);
    /// Surface SDK **assertion** violations (task 73). A campaign arms this so a
    /// cooperating guest's `assert_always`/`assert_unreachable` violation stops as
    /// [`StopReason::Assertion`] (a [`Bug`]) instead of running past, unjudged, to
    /// the terminal. The bit is single-sourced from `control_proto::class_bit::
    /// ASSERTION` (the adapter passes `.0` straight through to the control plane),
    /// so it can never drift from the surfacing gate.
    pub const ASSERTION: StopMask = StopMask(1u32 << control_proto::class_bit::ASSERTION as u32);
}

/// The conditions that bound one `run`: an optional V-time `deadline` and the
/// [`StopMask`] selecting which decision classes surface.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StopConditions {
    /// Stop with [`StopReason::Deadline`] once V-time reaches this, if set.
    pub deadline: Option<Moment>,
    /// Which decision classes surface as a [`StopReason::Decision`].
    pub on: StopMask,
}

/// Why a `run` returned — a **guest-observable** outcome, never a
/// transport/backend failure (that is a [`MachineError`], reported separately;
/// the two are never confused, `docs/DISSONANCE.md` "two result categories").
///
/// [`Deadline`](StopReason::Deadline), [`Quiescent`](StopReason::Quiescent), and
/// [`Crash`](StopReason::Crash) are always present;
/// [`Decision`](StopReason::Decision), [`Assertion`](StopReason::Assertion), and
/// [`SnapshotPoint`](StopReason::SnapshotPoint) appear with a cooperating SDK and
/// the matching [`StopMask`] bit. Only [`Crash`](StopReason::Crash) and
/// [`Assertion`](StopReason::Assertion) become a [`Bug`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StopReason {
    /// The V-time deadline was reached.
    Deadline {
        /// The V-time at the stop.
        vtime: Moment,
    },
    /// The guest went idle with no decision outstanding (a clean terminal stop;
    /// snapshots are taken only here).
    Quiescent {
        /// The V-time at the stop.
        vtime: Moment,
    },
    /// The guest crashed. Becomes a [`Bug`].
    Crash {
        /// The V-time at the stop.
        vtime: Moment,
        /// Opaque crash detail (e.g. a panic message / register dump).
        info: Vec<u8>,
    },
    /// A decision surfaced for the explorer to answer (the rollout's inner
    /// step); resolved by `run(resolve)`. Not terminal.
    Decision {
        /// The V-time at the stop.
        vtime: Moment,
        /// The decision identity (opaque; the toy uses the absolute index).
        id: u64,
        /// Opaque service↔policy context bytes handed to [`Tactic::decide`]
        /// (as the [`DecisionPoint`]'s `ctx`).
        ctx: Vec<u8>,
    },
    /// A guest SDK assertion was violated. Becomes a [`Bug`].
    Assertion {
        /// The V-time at the stop.
        vtime: Moment,
        /// The assertion identity.
        id: u32,
        /// Opaque assertion detail.
        data: Vec<u8>,
    },
    /// A quiescent point the explorer may snapshot to fork the search loop. Not
    /// terminal.
    SnapshotPoint {
        /// The V-time at the stop.
        vtime: Moment,
    },
}

impl StopReason {
    /// The V-time at which the run stopped, for every variant.
    pub fn vtime(&self) -> Moment {
        match self {
            StopReason::Deadline { vtime }
            | StopReason::Quiescent { vtime }
            | StopReason::Crash { vtime, .. }
            | StopReason::Decision { vtime, .. }
            | StopReason::Assertion { vtime, .. }
            | StopReason::SnapshotPoint { vtime } => *vtime,
        }
    }

    /// Whether this is a **terminal** stop that ends a rollout. Everything but
    /// [`Decision`](StopReason::Decision) and
    /// [`SnapshotPoint`](StopReason::SnapshotPoint) — the two the driver acts on
    /// and continues past — ends the run.
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            StopReason::Decision { .. } | StopReason::SnapshotPoint { .. }
        )
    }

    /// Whether this terminal stop is a reportable [`Bug`] — a
    /// [`Crash`](StopReason::Crash) or [`Assertion`](StopReason::Assertion). A
    /// [`MachineError`] is never one of these (it aborts the step instead).
    pub fn is_bug(&self) -> bool {
        matches!(
            self,
            StopReason::Crash { .. } | StopReason::Assertion { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `vtime()` returns the embedded V-time for every variant (not a default).
    #[test]
    fn vtime_is_pinned_per_variant() {
        assert_eq!(
            StopReason::Deadline { vtime: Moment(11) }.vtime(),
            Moment(11)
        );
        assert_eq!(
            StopReason::Quiescent { vtime: Moment(12) }.vtime(),
            Moment(12)
        );
        assert_eq!(
            StopReason::Crash {
                vtime: Moment(13),
                info: vec![]
            }
            .vtime(),
            Moment(13)
        );
        assert_eq!(
            StopReason::Decision {
                vtime: Moment(14),
                id: 0,
                ctx: vec![]
            }
            .vtime(),
            Moment(14)
        );
        assert_eq!(
            StopReason::Assertion {
                vtime: Moment(15),
                id: 0,
                data: vec![]
            }
            .vtime(),
            Moment(15)
        );
        assert_eq!(
            StopReason::SnapshotPoint { vtime: Moment(16) }.vtime(),
            Moment(16)
        );
    }

    /// `is_terminal()` is `false` for exactly `Decision`/`SnapshotPoint`, `true`
    /// for the four terminal variants.
    #[test]
    fn is_terminal_is_pinned_per_variant() {
        let z = Moment(0);
        assert!(StopReason::Deadline { vtime: z }.is_terminal());
        assert!(StopReason::Quiescent { vtime: z }.is_terminal());
        assert!(
            StopReason::Crash {
                vtime: z,
                info: vec![]
            }
            .is_terminal()
        );
        assert!(
            StopReason::Assertion {
                vtime: z,
                id: 0,
                data: vec![]
            }
            .is_terminal()
        );
        assert!(
            !StopReason::Decision {
                vtime: z,
                id: 0,
                ctx: vec![]
            }
            .is_terminal()
        );
        assert!(!StopReason::SnapshotPoint { vtime: z }.is_terminal());
    }

    /// `is_bug()` is `true` for exactly `Crash`/`Assertion`.
    #[test]
    fn is_bug_is_pinned_per_variant() {
        let z = Moment(0);
        assert!(
            StopReason::Crash {
                vtime: z,
                info: vec![]
            }
            .is_bug()
        );
        assert!(
            StopReason::Assertion {
                vtime: z,
                id: 0,
                data: vec![]
            }
            .is_bug()
        );
        assert!(!StopReason::Deadline { vtime: z }.is_bug());
        assert!(!StopReason::Quiescent { vtime: z }.is_bug());
        assert!(!StopReason::SnapshotPoint { vtime: z }.is_bug());
        assert!(
            !StopReason::Decision {
                vtime: z,
                id: 0,
                ctx: vec![]
            }
            .is_bug()
        );
    }
}
