// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **behavior-equivalence defaults**: the pre-refactor `Strategy`/`Corpus`
//! policies, decomposed onto the spine traits so that — composed as the old
//! god-object was — they reproduce the task-12 campaign byte-for-byte
//! (`tests/behavior_equiv.rs`). Nothing here is new search cleverness (task-64
//! non-goal); richer tactics/selectors/archives are tasks 65–75.
//!
//! One pre-refactor behavior does **not** survive: `CoverageStrategy::choose`
//! folded the *live* coverage map into each decision answer — exactly the
//! closed-loop, intra-run feedback the open-loop [`Tactic`] invariant outlaws
//! (and the trait shape now makes unexpressible). The decomposition therefore
//! ships [`DeclineTactic`] (the seed-strategy answering half, unchanged) and
//! moves all feedback to where the architecture puts it: between runs, in the
//! [`Selector`]/[`Archive`].

use std::collections::BTreeSet;

use crate::fingerprint;
use crate::prng::Prng;
use crate::spine::{
    Archive, Bug, CellFn, CellKey, ChannelId, CoverageView, DecisionPoint, ExemplarRef, Feature,
    FeatureId, FeatureSet, Fork, Frontier, FrontierEntry, Moment, Oracle, Reward, RunTrace,
    Selector, Sensor, Tactic,
};
use crate::{Answer, StopReason};

/// The channel the default archive files instrument-tier coverage features
/// under. Channel numbering is a campaign convention; `0` is reserved for
/// coverage by this crate's defaults.
pub const COVERAGE_CHANNEL: ChannelId = ChannelId(0);

// ---------------------------------------------------------------------------
// Tactic
// ---------------------------------------------------------------------------

/// The declining tactic: answers every decision with the empty [`Answer`], so
/// the backing's seed answers locally and the recorded artifact stays a pure
/// seed (FoundationDB style). The answering half of the pre-refactor
/// `SeedStrategy`, and trivially open-loop.
#[derive(Clone, Debug, Default)]
pub struct DeclineTactic;

impl DeclineTactic {
    /// The declining tactic (stateless).
    pub fn new() -> Self {
        Self
    }
}

impl Tactic for DeclineTactic {
    /// Decline: an empty answer falls through to the environment's seed, so no
    /// override is recorded.
    fn decide(&mut self, _pt: &DecisionPoint, _rng: &mut Prng) -> Answer {
        Answer(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Selectors
// ---------------------------------------------------------------------------

/// The always-explore selector: never picks a frontier exemplar, so every step
/// branches fresh from genesis with a new campaign seed (pure DST — the outer
/// half of the pre-refactor `SeedStrategy`).
#[derive(Clone, Debug, Default)]
pub struct GenesisSelector;

impl GenesisSelector {
    /// The always-explore selector (stateless).
    pub fn new() -> Self {
        Self
    }
}

impl Selector for GenesisSelector {
    /// Always `None`: explore fresh from genesis.
    fn choose(&mut self, _frontier: &Frontier, _rng: &mut Prng) -> Option<ExemplarRef> {
        None
    }

    /// Pure DST ignores rewards.
    fn reward(&mut self, _chosen: ExemplarRef, _r: Reward) {}
}

/// Every Nth step explores a fresh genesis seed; the rest exploit a frontier
/// exemplar (the outer half of the pre-refactor `CoverageStrategy`, draw-for-
/// draw). Tunable via
/// [`with_explore_period`](ExploreExploitSelector::with_explore_period).
const DEFAULT_EXPLORE_PERIOD: u64 = 3;

/// The explore/exploit selector: exploits a salt-picked frontier exemplar most
/// steps and periodically explores a fresh genesis seed to keep discovering new
/// prefixes (Antithesis style). Deterministic given the campaign stream.
#[derive(Clone, Debug)]
pub struct ExploreExploitSelector {
    step: u64,
    explore_period: u64,
}

impl Default for ExploreExploitSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl ExploreExploitSelector {
    /// A selector at the default explore period.
    pub fn new() -> Self {
        Self {
            step: 0,
            explore_period: DEFAULT_EXPLORE_PERIOD,
        }
    }

    /// Set how often (every Nth step) the selector explores from genesis
    /// instead of exploiting the frontier. Clamped to at least one.
    pub fn with_explore_period(mut self, period: u64) -> Self {
        self.explore_period = period.max(1);
        self
    }
}

impl Selector for ExploreExploitSelector {
    /// Exploit a salt-picked exemplar off-period; explore (return `None`) on
    /// the period boundary and whenever the frontier is empty. The pick draw
    /// happens only on exploit steps, mirroring the pre-refactor draw order
    /// exactly.
    fn choose(&mut self, frontier: &Frontier, rng: &mut Prng) -> Option<ExemplarRef> {
        self.step = self.step.wrapping_add(1);
        if frontier.is_empty() || self.step.is_multiple_of(self.explore_period) {
            return None;
        }
        let pick = rng.next_u64();
        frontier.nth(pick)
    }

    /// The count-free baseline ignores rewards (the bandit hook is task 70).
    fn reward(&mut self, _chosen: ExemplarRef, _r: Reward) {}
}

// ---------------------------------------------------------------------------
// CellFn
// ---------------------------------------------------------------------------

/// The identity cell function: a slice's cell is its canonical byte encoding
/// (each feature's channel + id, little-endian, in sorted order). Moment-blind:
/// the same features form the same cell whenever they occur, which is exactly
/// the pre-refactor global-novelty behavior. The finest useful keying — richer,
/// coarser CellFns are the task-67 iteration surface.
#[derive(Clone, Debug, Default)]
pub struct IdentityCells;

impl IdentityCells {
    /// The identity cell function (stateless).
    pub fn new() -> Self {
        Self
    }
}

impl CellFn for IdentityCells {
    /// Canonical bytes of the slice: `(channel_le, id_le)` per feature, sorted.
    fn key(&self, _at: Moment, feats: &FeatureSet) -> CellKey {
        let mut key = Vec::with_capacity(feats.len() * 10);
        for f in feats.iter() {
            key.extend_from_slice(&f.channel.0.to_le_bytes());
            key.extend_from_slice(&f.id.0.to_le_bytes());
        }
        key
    }
}

// ---------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------

/// The AFL count-bucket classifier: collapse a raw edge hit-count into a small
/// bucket so "novel" means a coarse new behaviour, not every off-by-one count.
/// Bucket `0` is "edge never hit" and is not itself novelty.
fn bucket(count: u8) -> u8 {
    match count {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=15 => 5,
        16..=31 => 6,
        32..=127 => 7,
        _ => 8,
    }
}

/// The `(edge, bucket)` features present in a coverage view, ascending by edge
/// (deterministic). Filed under [`COVERAGE_CHANNEL`]; the id packs
/// `edge * 256 + bucket` (bucket < 256, so the packing is injective).
fn coverage_features(view: &CoverageView) -> Vec<Feature> {
    let mut out = Vec::new();
    for (edge, &count) in view.map.iter().enumerate() {
        let b = bucket(count);
        if b != 0 {
            out.push(Feature {
                channel: COVERAGE_CHANNEL,
                id: FeatureId((edge as u64) * 256 + b as u64),
            });
        }
    }
    out
}

/// The coverage-novelty archive: the pre-refactor `Corpus`, generalized to
/// cells. Walks the run's sealable timeline (its [`Fork`]s, in order) and
/// admits the fork's [`VirtualExemplar`](crate::VirtualExemplar) iff its
/// coverage view claims at least one fresh cell — the AFL fresh-pair rule,
/// with each `(edge, bucket)` pair keyed through the injected [`CellFn`] as
/// its own cell.
///
/// Best-per-cell policy: **first admission wins** (the degenerate domination
/// key, preserving task-12 outcomes byte-for-byte); a quality-keyed
/// replacement policy is task-70+ territory and needs no spine change. The
/// frontier is bounded by **distinct cells**, never by runs: an entry is only
/// ever added when it claims a fresh cell, and entries are never dropped —
/// eviction of *seals* (the expensive part) is the engine's
/// reproducibility-safe knob ([`Explorer::evict_seals`](crate::Explorer::evict_seals)).
///
/// Coverage here is consumed **per sealable point** (the toy machine exposes
/// its map live), which is the faithful port of task-12's fork-time admission.
/// A production shmem map may only be terminal-tier; sensors over the (task
/// 65+/73+) event/record streams supply the along-timeline features then —
/// through this same `admit` walk, with zero spine change.
pub struct CoverageArchive {
    frontier: Frontier,
    /// The injected sealability predicate (task 63's `sealable` plugs in here;
    /// default always-true).
    sealable: Box<dyn Fn(Moment) -> bool>,
}

impl Default for CoverageArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl CoverageArchive {
    /// An empty archive admitting at every moment (the task-63 GO default).
    pub fn new() -> Self {
        Self {
            frontier: Frontier::new(),
            sealable: Box::new(|_| true),
        }
    }

    /// An empty archive admitting only where `sealable` allows — task 63's
    /// RESTRICTED ruling plugs in here with zero spine change.
    pub fn with_sealable(sealable: Box<dyn Fn(Moment) -> bool>) -> Self {
        Self {
            frontier: Frontier::new(),
            sealable,
        }
    }
}

impl Archive for CoverageArchive {
    /// Timeline admission, the AFL fresh-pair rule per sealable point: featurize
    /// the fork's coverage view, key each feature through `cells`, and admit the
    /// fork's exemplar iff it claims at least one unoccupied cell. Sensors'
    /// timeline features (from the trace's event/record streams — empty until
    /// tasks 65/73) join the same walk, keyed at their own moments.
    fn admit(
        &mut self,
        t: &RunTrace,
        forks: &[Fork],
        cells: &dyn CellFn,
        sensors: &[Box<dyn Sensor>],
    ) -> Reward {
        // Sensor-derived timeline features, grouped by moment (deterministic:
        // sensors emit in their own order; the fold below sorts by moment via
        // BTreeSet keying on (Moment, Feature)).
        let mut sensed: BTreeSet<(Moment, Feature)> = BTreeSet::new();
        for s in sensors {
            for (at, f) in s.observe(t) {
                sensed.insert((at, f));
            }
        }

        let mut total = 0u64;
        for fork in forks {
            let at = fork.exemplar.at;
            if !(self.sealable)(at) {
                continue;
            }
            // The features live at this sealable point: its coverage view's
            // (edge, bucket) pairs, plus any sensed features stamped at `at`.
            let mut feats: Vec<Feature> = fork
                .coverage
                .as_ref()
                .map(coverage_features)
                .unwrap_or_default();
            feats.extend(sensed.iter().filter(|(m, _)| *m == at).map(|(_, f)| *f));

            // Each feature keys its own (finest-slice) cell; fresh = unoccupied.
            // A BTreeSet dedups in O(log n) per key (a big fork carries many
            // features) and keeps the claim order canonical.
            let mut fresh: BTreeSet<CellKey> = BTreeSet::new();
            for f in feats {
                let key = cells.key(at, &FeatureSet::singleton(f));
                if self.frontier.occupant(&key).is_none() {
                    fresh.insert(key);
                }
            }
            if fresh.is_empty() {
                continue;
            }

            let reward = Reward {
                new_cells: fresh.len() as u64,
            };
            total += reward.new_cells;
            let r = self.frontier.insert(FrontierEntry {
                exemplar: fork.exemplar.clone(),
                env: fork.env.clone(),
                reward,
            });
            for key in fresh {
                // First-wins by construction: every key was fresh above and
                // deduplicated, so each claim succeeds.
                self.frontier.claim(key, r);
            }
        }
        Reward { new_cells: total }
    }

    /// The injected sealability predicate (default always-true).
    fn admissible(&self, at: Moment) -> bool {
        (self.sealable)(at)
    }

    /// Nothing to trim: first-wins admission never accumulates dominated
    /// exemplars, and frontier entries (kilobytes) are never dropped —
    /// reproducibility-safe seal eviction is the engine's
    /// [`evict_seals`](crate::Explorer::evict_seals) knob.
    fn evict(&mut self) {}

    /// The current frontier.
    fn frontier(&self) -> &Frontier {
        &self.frontier
    }
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// The terminal-stop oracle: a run exhibits a bug iff it ended in a
/// [`Crash`](StopReason::Crash) or [`Assertion`](StopReason::Assertion) — the
/// pre-refactor `is_bug` rule as a pluggable [`Oracle`]. Richer trace oracles
/// (Elle, declarative `never` matches) are task 75.
#[derive(Clone, Debug, Default)]
pub struct TerminalOracle;

impl TerminalOracle {
    /// The terminal-stop oracle (stateless).
    pub fn new() -> Self {
        Self
    }
}

impl Oracle for TerminalOracle {
    /// `Some` exactly on a bug-bearing terminal stop; the reported
    /// [`Bug::env`] is the trace's (genesis-complete) reproducer.
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        if !t.terminal.is_bug() {
            return None;
        }
        Some(Bug {
            env: t.env.clone(),
            stop: t.terminal.clone(),
            fingerprint: fingerprint(&t.terminal),
        })
    }
}

/// The `TerminalOracle`'s stable id for the shared fingerprint's coordinate 1.
const TERMINAL_ORACLE_ID: &str = "terminal";

/// Anomaly-class codes the terminal oracle assigns (coordinate 1's `class`).
/// Only the two bug-bearing stops are ever judged; the rest keep the mapping
/// total so `fingerprint` never panics on an unexpected stop.
fn terminal_class(stop: &StopReason) -> u32 {
    match stop {
        StopReason::Crash { .. } => 0,
        StopReason::Assertion { .. } => 1,
        StopReason::Deadline { .. } => 2,
        StopReason::Quiescent { .. } => 3,
        StopReason::Decision { .. } => 4,
        StopReason::SnapshotPoint { .. } => 5,
    }
}

/// The **normalized** detail bytes for coordinate 1: the crash marker or the
/// assertion `(id, data)`. The V-time is *not* here — it is coordinate 3
/// (quantized). No raw addresses (the toy's `info`/`data` are opaque markers).
fn terminal_detail(stop: &StopReason) -> Vec<u8> {
    match stop {
        StopReason::Crash { info, .. } => info.clone(),
        StopReason::Assertion { id, data, .. } => {
            let mut d = id.to_le_bytes().to_vec();
            d.extend_from_slice(data);
            d
        }
        _ => Vec::new(),
    }
}

/// A stable 32-byte digest of a bug stop, so the same crash/assertion dedups
/// across the many environments that reach it. Mints through the pinned,
/// versioned [`fingerprint`](crate::fingerprint) schema (task 75), superseding
/// task 12's stop-reason-only `dissonance.explorer.bug.v1` digest: coordinate 1
/// is the terminal signature (oracle id + class + normalized crash/assertion
/// detail + stop discriminant), coordinate 2 is empty (a pure trace oracle over
/// an opaque `Environment` cannot enumerate faults — the campaign's schema-aware
/// path populates it), and coordinate 3 is the quantized terminal V-time.
pub(crate) fn fingerprint(stop: &StopReason) -> [u8; 32] {
    let sig = fingerprint::TerminalSig::new(
        TERMINAL_ORACLE_ID,
        terminal_class(stop),
        stop.discriminant(),
    )
    .with_detail(terminal_detail(stop));
    fingerprint::mint(
        &sig,
        &fingerprint::FaultCoord::none(),
        fingerprint::VTimeCoord::quantize(Moment(stop.vtime().0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Environment, SnapId, VTime, VirtualExemplar};

    fn env() -> Environment {
        Environment {
            blob_version: 1,
            bytes: vec![],
        }
    }

    fn fork(at: u64, coverage: &[u8]) -> Fork {
        Fork {
            exemplar: VirtualExemplar {
                parent: SnapId(1),
                seed: 0,
                suffix: env(),
                at: Moment(at),
            },
            env: env(),
            coverage: Some(CoverageView {
                map: coverage.to_vec(),
            }),
        }
    }

    fn trace() -> RunTrace {
        RunTrace {
            terminal: StopReason::Quiescent { vtime: VTime(80) },
            env: env(),
            coverage: None,
            events: vec![],
            records: vec![],
        }
    }

    fn admit(a: &mut CoverageArchive, forks: &[Fork]) -> Reward {
        a.admit(&trace(), forks, &IdentityCells, &[])
    }

    /// The bug fingerprint mints through the pinned task-75 three-coordinate
    /// schema (superseding the task-12 stop-only digest): coordinate 1 is the
    /// terminal signature, coordinate 2 is empty (schema-blind), coordinate 3 is
    /// the quantized V-time. Cross-checks the `defaults` path against the shared
    /// `fingerprint::mint`, pins a golden (kills the `[0;32]`/`[1;32]` mutants and
    /// locks scheme stability), and proves the V-time now lives in coordinate 3.
    #[test]
    fn fingerprint_is_a_pinned_digest() {
        let crash = StopReason::Crash {
            vtime: VTime(80),
            info: vec![2, 4],
        };
        // The `defaults` fingerprint IS the shared mint over the terminal
        // signature (oracle id + crash class + info detail + stop discriminant),
        // an empty fault coordinate, and the quantized V-time.
        let sig = fingerprint::TerminalSig::new("terminal", 0, crash.discriminant())
            .with_detail(vec![2, 4]);
        let expect = fingerprint::mint(
            &sig,
            &fingerprint::FaultCoord::none(),
            fingerprint::VTimeCoord::quantize(Moment(80)),
        );
        assert_eq!(fingerprint(&crash), expect);

        let golden: [u8; 32] = [
            0x93, 0x20, 0xde, 0xa2, 0x73, 0xd4, 0x15, 0x61, 0x79, 0x54, 0x9b, 0xea, 0x76, 0x4e,
            0x66, 0x1b, 0xc6, 0xef, 0xaf, 0x01, 0xcb, 0xcd, 0x54, 0x4c, 0x50, 0xe9, 0xf2, 0xd1,
            0x51, 0x7c, 0x62, 0x2f,
        ];
        assert_eq!(fingerprint(&crash), golden);
        assert_ne!(fingerprint(&crash), [0u8; 32]);
        assert_ne!(fingerprint(&crash), [1u8; 32]);

        // The V-time is coordinate 3 (quantized), not raw: two crashes in the
        // same bracket share a fingerprint, one a bracket away does not.
        let same_bracket = StopReason::Crash {
            vtime: VTime(
                80 + fingerprint::FINGERPRINT_VTIME_BRACKET
                    - 1
                    - (80 % fingerprint::FINGERPRINT_VTIME_BRACKET),
            ),
            info: vec![2, 4],
        };
        assert_eq!(fingerprint(&crash), fingerprint(&same_bracket));
    }

    /// Distinct stops fingerprint differently so dedup keeps distinct bugs:
    /// class (crash vs assertion), normalized detail (crash marker / assertion
    /// data), and a V-time a whole *bracket* away all split. A raw V-time
    /// difference *within* a bracket does **not** split (that is coordinate 3's
    /// deliberate quantization — proven in `fingerprint_is_a_pinned_digest`).
    #[test]
    fn fingerprint_distinguishes_stops() {
        let crash = StopReason::Crash {
            vtime: VTime(80),
            info: vec![2, 4],
        };
        let assertion = StopReason::Assertion {
            vtime: VTime(80),
            id: 5,
            data: vec![3],
        };
        // Different class (crash vs assertion).
        assert_ne!(fingerprint(&crash), fingerprint(&assertion));
        // Different normalized detail (crash marker).
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: VTime(80),
                info: vec![2, 5],
            })
        );
        // A V-time a full bracket away splits (coordinate 3).
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: VTime(80 + fingerprint::FINGERPRINT_VTIME_BRACKET),
                info: vec![2, 4],
            })
        );
        // Two assertions with different ids split on their detail.
        assert_ne!(
            fingerprint(&assertion),
            fingerprint(&StopReason::Assertion {
                vtime: VTime(80),
                id: 7,
                data: vec![3],
            })
        );
    }

    /// The AFL bucket classifier, pinned per range — one representative per arm,
    /// so deleting any arm changes a value.
    #[test]
    fn bucket_classifier_is_pinned_per_range() {
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(1), 1);
        assert_eq!(bucket(2), 2);
        assert_eq!(bucket(3), 3);
        assert_eq!(bucket(5), 4); // 4..=7
        assert_eq!(bucket(10), 5); // 8..=15
        assert_eq!(bucket(20), 6); // 16..=31
        assert_eq!(bucket(64), 7); // 32..=127
        assert_eq!(bucket(200), 8); // 128..
    }

    /// Coverage featurization: bucket-0 edges are dropped; the id packs
    /// `edge << 8 | bucket` under the coverage channel (pins the shift and the
    /// nonzero guard).
    #[test]
    fn coverage_features_pack_edge_and_bucket() {
        let feats = coverage_features(&CoverageView { map: vec![0, 1, 9] });
        assert_eq!(
            feats,
            vec![
                Feature {
                    channel: COVERAGE_CHANNEL,
                    id: FeatureId((1 << 8) | 1),
                },
                Feature {
                    channel: COVERAGE_CHANNEL,
                    id: FeatureId((2 << 8) | 5),
                },
            ]
        );
    }

    /// The archive admits exactly on a fresh cell: first non-zero coverage is
    /// novel, a subset re-offer is not, a new edge or a higher bucket is (the
    /// pre-refactor `Corpus::admit` rule verbatim).
    #[test]
    fn admits_exactly_on_a_fresh_cell() {
        let mut a = CoverageArchive::new();
        assert_eq!(admit(&mut a, &[fork(40, &[0, 1, 0, 0])]).new_cells, 1);
        // Same single pair again — no fresh cell, nothing admitted.
        assert_eq!(admit(&mut a, &[fork(40, &[0, 1, 0, 0])]).new_cells, 0);
        // A new edge is fresh; a higher bucket on a seen edge is fresh.
        assert_eq!(admit(&mut a, &[fork(40, &[0, 1, 0, 1])]).new_cells, 1);
        assert_eq!(admit(&mut a, &[fork(40, &[0, 9, 0, 1])]).new_cells, 1);
        assert_eq!(admit(&mut a, &[fork(40, &[0, 9, 0, 1])]).new_cells, 0);
        assert_eq!(a.frontier().len(), 3);
        // Bounded by cells: every entry claimed at least one.
        assert!(a.frontier().len() <= a.frontier().occupied_cells());
    }

    /// All-zero coverage is never novel (bucket 0 is not novelty).
    #[test]
    fn all_zero_coverage_is_never_novel() {
        let mut a = CoverageArchive::new();
        assert_eq!(admit(&mut a, &[fork(40, &[0, 0, 0])]).new_cells, 0);
        assert!(a.frontier().is_empty());
    }

    /// One run's forks are admitted in timeline order, folding freshness
    /// between them (the second fork is only fresh for what the first did not
    /// already claim), and the returned reward is the run total.
    #[test]
    fn timeline_admission_folds_across_forks() {
        let mut a = CoverageArchive::new();
        let r = admit(&mut a, &[fork(40, &[1, 1, 0]), fork(60, &[1, 1, 1])]);
        // Fork 1 claims edges 0+1; fork 2 only edge 2 is fresh.
        assert_eq!(r.new_cells, 3);
        assert_eq!(a.frontier().len(), 2);
        let e0 = a.frontier().get(ExemplarRef(0)).expect("entry 0");
        let e1 = a.frontier().get(ExemplarRef(1)).expect("entry 1");
        assert_eq!(e0.reward.new_cells, 2);
        assert_eq!(e1.reward.new_cells, 1);
        assert_eq!(e0.exemplar.at, Moment(40));
        assert_eq!(e1.exemplar.at, Moment(60));
    }

    /// The injected sealability predicate gates admission (task 63's RESTRICTED
    /// ruling plugs in with zero spine change): a fork at a non-sealable moment
    /// is skipped entirely, however novel.
    #[test]
    fn sealability_predicate_gates_admission() {
        let mut a = CoverageArchive::with_sealable(Box::new(|at| at.0 >= 60));
        assert!(!a.admissible(Moment(40)));
        assert!(a.admissible(Moment(60)));
        let r = admit(&mut a, &[fork(40, &[1, 0, 0]), fork(60, &[0, 1, 0])]);
        assert_eq!(r.new_cells, 1, "only the sealable fork admits");
        assert_eq!(a.frontier().len(), 1);
        assert_eq!(
            a.frontier()
                .get(ExemplarRef(0))
                .expect("the sealable fork's entry")
                .exemplar
                .at,
            Moment(60)
        );
    }

    /// Sensor-derived timeline features join the walk at their own moments: a
    /// feature stamped at a fork's moment can make that fork novel even with
    /// no coverage view.
    #[test]
    fn sensed_features_admit_at_their_moment() {
        struct OneFeature;
        impl Sensor for OneFeature {
            fn observe(&self, _t: &RunTrace) -> Vec<(Moment, Feature)> {
                vec![(
                    Moment(40),
                    Feature {
                        channel: ChannelId(2),
                        id: FeatureId(7),
                    },
                )]
            }
        }
        let mut a = CoverageArchive::new();
        let mut f = fork(40, &[]);
        f.coverage = None;
        let sensors: Vec<Box<dyn Sensor>> = vec![Box::new(OneFeature)];
        let r = a.admit(&trace(), &[f], &IdentityCells, &sensors);
        assert_eq!(r.new_cells, 1);
        // A fork at a different moment sees none of it.
        let mut a2 = CoverageArchive::new();
        let mut f2 = fork(60, &[]);
        f2.coverage = None;
        let r2 = a2.admit(&trace(), &[f2], &IdentityCells, &sensors);
        assert_eq!(r2.new_cells, 0);
    }

    /// `IdentityCells` canonically encodes the slice (sorted, channel+id
    /// little-endian) and ignores the moment — pinned bytes.
    #[test]
    fn identity_cells_key_is_pinned() {
        let f1 = Feature {
            channel: ChannelId(1),
            id: FeatureId(0x0201),
        };
        let f2 = Feature {
            channel: ChannelId(0),
            id: FeatureId(3),
        };
        let set: FeatureSet = [f1, f2].into_iter().collect();
        let key = IdentityCells.key(Moment(40), &set);
        // Sorted: (0,3) then (1,0x0201); 2-byte channel LE + 8-byte id LE each.
        assert_eq!(
            key,
            vec![
                0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0x01, 0x02, 0, 0, 0, 0, 0, 0
            ]
        );
        // Moment-blind: same slice, same key at another moment.
        assert_eq!(key, IdentityCells.key(Moment(60), &set));
    }

    /// The selectors' explore-vs-exploit decisions are pinned: off-period steps
    /// exploit (a salt draw picks in admission order), the period boundary and
    /// an empty frontier explore, and `GenesisSelector` always explores.
    #[test]
    fn selector_explore_vs_exploit_is_pinned() {
        let mut frontier = Frontier::new();
        let entry = FrontierEntry {
            exemplar: VirtualExemplar {
                parent: SnapId(1),
                seed: 0,
                suffix: env(),
                at: Moment(40),
            },
            env: env(),
            reward: Reward { new_cells: 1 },
        };
        let r0 = frontier.insert(entry.clone());
        frontier.claim(vec![1], r0);

        let mut rng = Prng::new(5);
        let mut s = ExploreExploitSelector::new().with_explore_period(2);
        // Step 1: 1 % 2 != 0 → exploit → the one entry.
        assert_eq!(s.choose(&frontier, &mut rng), Some(r0));
        // Step 2: 2 % 2 == 0 → explore.
        assert_eq!(s.choose(&frontier, &mut rng), None);
        // An empty frontier always explores, whatever the step.
        let mut s2 = ExploreExploitSelector::new().with_explore_period(100);
        assert_eq!(s2.choose(&Frontier::new(), &mut rng), None);
        // GenesisSelector never exploits.
        assert_eq!(GenesisSelector::new().choose(&frontier, &mut rng), None);
    }

    /// `DeclineTactic` always declines with an empty answer and never draws
    /// from the stream (the pre-refactor `SeedStrategy::choose`).
    #[test]
    fn decline_tactic_is_always_empty_and_draws_nothing() {
        let mut t = DeclineTactic::new();
        let mut rng = Prng::new(7);
        let before = rng.clone();
        let pt = DecisionPoint {
            at: Moment(40),
            id: 4,
            ctx: vec![1, 2],
        };
        assert_eq!(t.decide(&pt, &mut rng), Answer(vec![]));
        assert_eq!(rng, before, "declining consumes no stream words");
    }

    /// `TerminalOracle` judges exactly the bug-bearing stops and reports the
    /// trace's (genesis-complete) env verbatim.
    #[test]
    fn terminal_oracle_judges_exactly_bug_stops() {
        let mut t = trace();
        assert!(TerminalOracle::new().judge(&t).is_none());
        t.terminal = StopReason::Crash {
            vtime: VTime(80),
            info: vec![2, 4],
        };
        t.env = Environment {
            blob_version: 1,
            bytes: vec![9, 9],
        };
        let bug = TerminalOracle::new().judge(&t).expect("a crash is a bug");
        assert_eq!(bug.env, t.env);
        assert_eq!(bug.stop, t.terminal);
        assert_eq!(bug.fingerprint, fingerprint(&t.terminal));
    }
}
