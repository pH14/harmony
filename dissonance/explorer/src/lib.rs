// SPDX-License-Identifier: AGPL-3.0-or-later
//! # explorer — the Timeline/Multiverse coverage-guided exploration engine
//!
//! `explorer` is **all of dissonance policy**: the brain that drives a
//! deterministic guest through many environments to find bugs. It owns the
//! corpus, the coverage-novelty scoring, the per-run decision policy, and the
//! mutation/scheduling strategy. Mutation lives **here**, never in the wire (the
//! AFL lesson): the engine ferries an opaque, versioned [`Environment`] blob and
//! mutates it only through the [`EnvCodec`] seam, so the schema (dissonance task
//! 24) stays owned by the catalog and the wire stays fixed independently of it.
//!
//! ## The two loops
//!
//! - **Timeline (inner, [`Explorer::timeline`]):** drive ONE run forward —
//!   `run` ⇄ `run(resolve)` — answering each surfaced [`StopReason::Decision`]
//!   via [`Strategy::choose`] and accumulating the answers as the reproducer
//!   [`Environment`] ([`Machine::recorded_env`]). Ends at a terminal
//!   [`StopReason`].
//! - **Multiverse (outer, [`Explorer::multiverse_step`]):** across runs —
//!   pick/mutate an [`Environment`] from the [`Corpus`], `branch`, run one
//!   Timeline, score coverage novelty + assertions, keep the snapshot if novel.
//!   One Multiverse step = one Timeline.
//!
//! ## The seams (defined locally, conventions rule 2)
//!
//! The engine codes against a [`Machine`]/[`MachineFactory`] driver seam and an
//! [`EnvCodec`] minting seam, both defined in this crate. In production the
//! [`mod@adapter`] module's [`SocketMachine`] implements [`Machine`] over a
//! `control-proto` client stream (against vmm-core's control-transport server,
//! task 58) and [`SpecEnvCodec`] binds [`EnvCodec`] to the `environment`
//! crate's real reproducer codec per the task-93 ruling; in tests an in-crate
//! deterministic **toy machine** does both — so the same engine and the same
//! determinism gate run both sides unchanged.
//!
//! ## Determinism discipline
//!
//! Nothing here observes wall-clock time, host entropy, `HashMap`/`HashSet`
//! iteration order, or floating point. The corpus novelty index is a
//! `BTreeMap`/`BTreeSet`, every strategy draw comes from a caller-seeded PRNG,
//! and the [`Bug`] fingerprint is a `sha2` digest of the stop reason — so the
//! same `(strategy seed, machine)` yields a bit-identical exploration trace and
//! an identical set of admitted corpus entries.
//!
//! ## Module layout
//!
//! [`mod@error`] (the [`MachineError`] enum) · `seam` (the [`Machine`],
//! [`MachineFactory`], and [`EnvCodec`] traits) · `strategy` ([`Strategy`],
//! [`SeedStrategy`], [`CoverageStrategy`]) · `corpus` ([`Corpus`], the
//! deterministic novelty index) · `engine` ([`Explorer`], [`RunOutcome`],
//! [`Bug`]) · `prng` (the local xorshift64\* generator the strategies draw
//! from) · [`mod@adapter`] (the R2 socket adapter: [`SocketMachine`],
//! [`SpecEnvCodec`], and the [`AdapterEnv`] blob — task 58).

pub mod adapter;
mod corpus;
mod engine;
mod error;
mod prng;
mod seam;
mod strategy;

pub use adapter::{ADAPTER_BLOB_VERSION, AdapterEnv, SocketMachine, SpecEnvCodec, client_caps};
pub use corpus::{Corpus, CovScore};
pub use engine::{Bug, Explorer, RunOutcome};
pub use error::MachineError;
pub use seam::{EnvCodec, Machine, MachineFactory};
pub use strategy::{CoverageStrategy, SeedStrategy, Strategy};

/// An ephemeral, pool-wide handle to a captured machine state (a snapshot). It
/// is a transient resource handle — **never** part of a portable reproducer
/// artifact (that is the [`Environment`]); the only stable, always-reproducible
/// base is the genesis snapshot from [`Explorer::new`]. The control plane mints
/// these on `snapshot` and frees them on `drop` (corpus GC).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SnapId(pub u64);

/// V-time: a count of retired conditional branches — the project's only
/// deterministic clock. Mirrors the integration type (conventions rule 2); the
/// integrator unifies it with `vtime`'s clock. Deadlines and the V-time carried
/// in every [`StopReason`] are in these units.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct VTime(pub u64);

/// The reproducer the explorer ferries: an **opaque, versioned blob**. The
/// explorer never parses `bytes` — dissonance task 24 owns the structure — it
/// only moves the blob between [`Machine`], [`EnvCodec`], and [`Corpus`]. The
/// `blob_version` lets a backend reject a blob it does not understand
/// (`BadEnvVersion`) without the explorer decoding anything.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Environment {
    /// The blob format version (task 24's `EnvSpec::BLOB_VERSION`), checked by
    /// the backend, opaque to the explorer.
    pub blob_version: u16,
    /// The opaque, versioned reproducer bytes. Mutated only through
    /// [`EnvCodec`], never byte-flipped in place.
    pub bytes: Vec<u8>,
}

/// One answer to a surfaced [`StopReason::Decision`], opaque service↔policy
/// bytes the [`Strategy`] mints and the [`Machine`] stages into the suspended
/// hypercall on `run(resolve)`. Opaque to the explorer.
///
/// Deliberately **not** `Default`: an "empty answer" (a declining
/// [`SeedStrategy`]) is `Answer(Vec::new())`, which would be indistinguishable
/// from a derived `Default` — leaving a mutation-testing blind spot. Construct
/// the empty answer explicitly instead.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer(pub Vec<u8>);

/// Which decision *classes* a `run` surfaces, mirroring the control-proto /
/// `DecisionClass` bits. Everything not selected the environment answers
/// locally (the seed), so a campaign tunes how reactive a Timeline is by which
/// bits it sets. Interpreted by the [`Machine`]; the explorer carries it through
/// unparsed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopMask(pub u32);

impl StopMask {
    /// Surface nothing — the environment's seed answers every decision locally,
    /// so a Timeline driven with this mask has zero stops (pure seed-driven
    /// exploration, the Multiverse alone). The [`SeedStrategy`] campaign mask.
    pub const NONE: StopMask = StopMask(0);
    /// Surface every decision class (and the snapshot point) — the
    /// coverage-guided default, so the [`Strategy`] answers each interesting
    /// decision and the explorer can fork a snapshot mid-run.
    pub const ALL: StopMask = StopMask(u32::MAX);
}

/// The conditions that bound one `run`: an optional V-time `deadline` and the
/// [`StopMask`] selecting which decision classes surface.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StopConditions {
    /// Stop with [`StopReason::Deadline`] once V-time reaches this, if set.
    pub deadline: Option<VTime>,
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
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StopReason {
    /// The V-time deadline was reached.
    Deadline {
        /// The V-time at the stop.
        vtime: VTime,
    },
    /// The guest went idle with no decision outstanding (a clean terminal stop;
    /// snapshots are taken only here).
    Quiescent {
        /// The V-time at the stop.
        vtime: VTime,
    },
    /// The guest crashed. Becomes a [`Bug`].
    Crash {
        /// The V-time at the stop.
        vtime: VTime,
        /// Opaque crash detail (e.g. a panic message / register dump).
        info: Vec<u8>,
    },
    /// A decision surfaced for the explorer to answer (the Timeline's inner
    /// step); resolved by `run(resolve)`. Not terminal.
    Decision {
        /// The V-time at the stop.
        vtime: VTime,
        /// The decision identity (opaque; the toy uses the absolute index).
        id: u64,
        /// Opaque service↔policy context bytes handed to [`Strategy::choose`].
        ctx: Vec<u8>,
    },
    /// A guest SDK assertion was violated. Becomes a [`Bug`].
    Assertion {
        /// The V-time at the stop.
        vtime: VTime,
        /// The assertion identity.
        id: u32,
        /// Opaque assertion detail.
        data: Vec<u8>,
    },
    /// A quiescent point the explorer may snapshot to fork the Multiverse. Not
    /// terminal.
    SnapshotPoint {
        /// The V-time at the stop.
        vtime: VTime,
    },
}

impl StopReason {
    /// The V-time at which the run stopped, for every variant.
    pub fn vtime(&self) -> VTime {
        match self {
            StopReason::Deadline { vtime }
            | StopReason::Quiescent { vtime }
            | StopReason::Crash { vtime, .. }
            | StopReason::Decision { vtime, .. }
            | StopReason::Assertion { vtime, .. }
            | StopReason::SnapshotPoint { vtime } => *vtime,
        }
    }

    /// Whether this is a **terminal** stop that ends a Timeline. Everything but
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
        assert_eq!(StopReason::Deadline { vtime: VTime(11) }.vtime(), VTime(11));
        assert_eq!(
            StopReason::Quiescent { vtime: VTime(12) }.vtime(),
            VTime(12)
        );
        assert_eq!(
            StopReason::Crash {
                vtime: VTime(13),
                info: vec![]
            }
            .vtime(),
            VTime(13)
        );
        assert_eq!(
            StopReason::Decision {
                vtime: VTime(14),
                id: 0,
                ctx: vec![]
            }
            .vtime(),
            VTime(14)
        );
        assert_eq!(
            StopReason::Assertion {
                vtime: VTime(15),
                id: 0,
                data: vec![]
            }
            .vtime(),
            VTime(15)
        );
        assert_eq!(
            StopReason::SnapshotPoint { vtime: VTime(16) }.vtime(),
            VTime(16)
        );
    }

    /// `is_terminal()` is `false` for exactly `Decision`/`SnapshotPoint`, `true`
    /// for the four terminal variants.
    #[test]
    fn is_terminal_is_pinned_per_variant() {
        let z = VTime(0);
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
        let z = VTime(0);
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
