// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **default policies** the surviving control seams ship with: the
//! declining [`Tactic`], the two baseline [`Selector`]s, the terminal-stop
//! [`Oracle`], and the pinned bug fingerprint. Task 132 M3 physically
//! deleted the legacy engine and its compat archive path
//! (`Explorer::step`/`Archive::admit`, `CoverageArchive`, `IdentityCells`,
//! the `Sensor`/`Feature`/`FeatureSet` currencies) — the two-barrier
//! [`DifferentialCampaign`](crate::DifferentialCampaign) is the one
//! production search loop, and these defaults are the policies its seams
//! (and the campaign drivers above) compose.

use sha2::{Digest, Sha256};

use crate::prng::Prng;
use crate::spine::{
    Bug, DecisionPoint, ExemplarRef, Frontier, Oracle, Reward, RunTrace, Selector, Tactic,
};
use crate::{Answer, StopReason};

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

/// A stable 32-byte digest of a bug stop, so the same crash/assertion dedups
/// across the many environments that reach it. Domain-separated by a leading tag;
/// only the bug-bearing variants are expected, but every variant hashes totally.
/// Byte-identical to the pre-refactor fingerprint (the equivalence gate depends
/// on it).
pub(crate) fn fingerprint(stop: &StopReason) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"dissonance.explorer.bug.v1");
    match stop {
        StopReason::Crash { vtime, info } => {
            h.update([0xC1]);
            h.update(vtime.0.to_le_bytes());
            h.update(info);
        }
        StopReason::Assertion { vtime, id, data } => {
            h.update([0xA1]);
            h.update(vtime.0.to_le_bytes());
            h.update(id.to_le_bytes());
            h.update(data);
        }
        // Non-bug stops are never fingerprinted in practice; hash their tag so
        // the function stays total.
        StopReason::Deadline { vtime } => {
            h.update([0xD1]);
            h.update(vtime.0.to_le_bytes());
        }
        StopReason::Quiescent { vtime } => {
            h.update([0x01]);
            h.update(vtime.0.to_le_bytes());
        }
        StopReason::Decision { vtime, id, ctx } => {
            h.update([0xDE]);
            h.update(vtime.0.to_le_bytes());
            h.update(id.to_le_bytes());
            h.update(ctx);
        }
        StopReason::SnapshotPoint { vtime } => {
            h.update([0x5A]);
            h.update(vtime.0.to_le_bytes());
        }
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::FrontierEntry;
    use crate::{EvidenceCut, Moment, Reproducer, SnapId, VirtualExemplar};

    fn env() -> Reproducer {
        Reproducer {
            blob_version: 1,
            bytes: vec![],
        }
    }

    /// The bug fingerprint is the pinned `sha2` digest of the stop reason — locks
    /// the domain tag + field layout, kills the `[0;32]`/`[1;32]` mutants, and
    /// (being the same golden as the pre-refactor test) pins cross-refactor
    /// fingerprint stability.
    #[test]
    fn fingerprint_is_a_pinned_digest() {
        let crash = StopReason::Crash {
            vtime: Moment(80),
            info: vec![2, 4],
        };
        let golden: [u8; 32] = [
            0x87, 0x98, 0x12, 0xff, 0x07, 0x03, 0x95, 0x3f, 0x5d, 0x41, 0x10, 0xd9, 0xb7, 0xc9,
            0x06, 0xcc, 0xfc, 0xf9, 0xc2, 0xeb, 0x81, 0x71, 0x5e, 0xd6, 0xaf, 0x1b, 0x5c, 0x21,
            0x5c, 0x23, 0x6e, 0x16,
        ];
        assert_eq!(fingerprint(&crash), golden);
        assert_ne!(fingerprint(&crash), [0u8; 32]);
        assert_ne!(fingerprint(&crash), [1u8; 32]);
    }

    /// Two different stops fingerprint differently (so dedup keeps distinct bugs).
    #[test]
    fn fingerprint_distinguishes_stops() {
        let crash = StopReason::Crash {
            vtime: Moment(80),
            info: vec![2, 4],
        };
        let assertion = StopReason::Assertion {
            vtime: Moment(80),
            id: 5,
            data: vec![3],
        };
        assert_ne!(fingerprint(&crash), fingerprint(&assertion));
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: Moment(81),
                info: vec![2, 4],
            })
        );
        assert_ne!(
            fingerprint(&crash),
            fingerprint(&StopReason::Crash {
                vtime: Moment(80),
                info: vec![2, 5],
            })
        );
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
                cut: EvidenceCut {
                    at: Moment(40),
                    sdk_events: 0,
                },
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

    fn trace() -> RunTrace {
        RunTrace {
            terminal: StopReason::Quiescent { vtime: Moment(80) },
            env: env(),
            coverage: None,
            events: vec![],
            records: vec![],
        }
    }

    /// `TerminalOracle` judges exactly the bug-bearing stops and reports the
    /// trace's (genesis-complete) env verbatim.
    #[test]
    fn terminal_oracle_judges_exactly_bug_stops() {
        let mut t = trace();
        assert!(TerminalOracle::new().judge(&t).is_none());
        t.terminal = StopReason::Crash {
            vtime: Moment(80),
            info: vec![2, 4],
        };
        t.env = Reproducer {
            blob_version: 1,
            bytes: vec![9, 9],
        };
        let bug = TerminalOracle::new().judge(&t).expect("a crash is a bug");
        assert_eq!(bug.env, t.env);
        assert_eq!(bug.stop, t.terminal);
        assert_eq!(bug.fingerprint, fingerprint(&t.terminal));
    }
}
