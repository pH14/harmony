// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-64 gate 3 — **behavior equivalence**.
//!
//! A fixed suite (≥50 campaigns × the existing toy machine) yields
//! **byte-identical bug fingerprints, bug reproducers, and admission
//! decisions** pre- and post-refactor: the reference side is the vendored
//! pre-refactor engine (`tests/reference`), the live side is the refactored
//! engine driving the default `Tactic`/`Selector`/`Archive` composed as the old
//! `Strategy` was (one campaign stream, same draw order). Structure changes,
//! outcomes don't.
//!
//! ## The suite's two configurations
//!
//! - **Seed campaigns** (`StopMask::ALL`): the pre-refactor `SeedStrategy` vs.
//!   its decomposition `DeclineTactic` + `GenesisSelector`. Decisions surface
//!   and are declined; forks are admitted on coverage novelty.
//! - **Explore/exploit campaigns** (`SNAP_BIT` only): the pre-refactor
//!   `CoverageStrategy` vs. its decomposition `DeclineTactic` +
//!   `ExploreExploitSelector`. The full outer loop — salt-picked exploits,
//!   mutation minting, nested forks, compose rebasing — with no decision
//!   surfacing.
//!
//! `CoverageStrategy` + surfaced decisions is deliberately **not** in the
//! suite: its `choose` folded the *live* coverage map into each answer, the
//! closed-loop feedback the task-64 open-loop invariant outlaws. That fold is
//! the one pre-refactor behavior the refactor drops (by ruling, not by
//! accident); everything expressible in the ruled architecture is compared
//! here, byte-for-byte.

mod common;
mod reference;

use common::{SNAP_BIT, ToyCodec, ToyMachine};
use explorer::{
    Composition, CoverageArchive, DeclineTactic, ExploreExploitSelector, GenesisSelector,
    IdentityCells, StopConditions, StopMask, TerminalOracle,
};

/// One campaign's observable output, engine-agnostic: the deduplicated bugs
/// (fingerprint + genesis-complete reproducer bytes) and the admitted frontier
/// in admission order (genesis-complete env bytes + novelty score).
#[derive(PartialEq, Eq, Debug)]
struct CampaignResult {
    bugs: Vec<([u8; 32], Vec<u8>)>,
    admitted: Vec<(Vec<u8>, u64)>,
}

/// Drive the **vendored pre-refactor** engine.
fn reference_campaign<S: reference::Strategy>(
    strategy: S,
    mask: StopMask,
    steps: u64,
) -> CampaignResult {
    let mut ex = reference::Explorer::new(ToyMachine::new(), strategy, Box::new(ToyCodec)).unwrap();
    // Disable the reference's capacity eviction so both sides run the
    // eviction-free regime (the refactored archive is bounded by cells, not by
    // an entry count). The suite proves below that the default capacity (64)
    // would never have evicted anyway, so this knob is comparison hygiene, not
    // a behavioral concession.
    ex.set_corpus_capacity(usize::MAX >> 1).unwrap();
    ex.set_stop_conditions(StopConditions {
        deadline: None,
        on: mask,
    });
    let bugs = ex.explore(steps).unwrap();
    let corpus = ex.corpus();
    assert!(
        corpus.len() < 64,
        "the suite stays under the reference's default capacity, so eviction \
         never fires either way"
    );
    CampaignResult {
        bugs: bugs
            .into_iter()
            .map(|b| (b.fingerprint, b.env.bytes))
            .collect(),
        admitted: (0..corpus.len())
            .map(|i| {
                let (_, env, score) = corpus.entry(i).unwrap();
                (env.bytes.clone(), score.0)
            })
            .collect(),
    }
}

/// Drive the **refactored** engine with the given default composition.
fn refactored_campaign(
    parts: Composition,
    seed: u64,
    mask: StopMask,
    steps: u64,
) -> CampaignResult {
    let mut ex =
        explorer::Explorer::new(ToyMachine::new(), Box::new(ToyCodec), parts, seed).unwrap();
    ex.set_stop_conditions(StopConditions {
        deadline: None,
        on: mask,
    });
    let bugs = ex.explore(steps).unwrap();
    CampaignResult {
        bugs: bugs
            .into_iter()
            .map(|b| (b.fingerprint, b.env.bytes))
            .collect(),
        admitted: ex
            .frontier()
            .iter()
            .map(|(_, e)| (e.env.bytes.clone(), e.reward.new_cells))
            .collect(),
    }
}

/// The decomposition of the pre-refactor `SeedStrategy`.
fn seed_parts() -> Composition {
    Composition {
        tactic: Box::new(DeclineTactic::new()),
        selector: Box::new(GenesisSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    }
}

/// The decomposition of the pre-refactor `CoverageStrategy` (its outer half;
/// the answering half is declined — decisions never surface in this config).
fn explore_exploit_parts() -> Composition {
    Composition {
        tactic: Box::new(DeclineTactic::new()),
        selector: Box::new(ExploreExploitSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    }
}

/// The fixed suite: 25 seeds × 2 configurations = 50 campaigns, byte-identical
/// on both sides, and non-vacuous (bugs found, entries admitted, exploits
/// actually exercised).
#[test]
fn fifty_campaigns_are_byte_identical_across_the_refactor() {
    let seeds: Vec<u64> = (1..=25).map(|i| i * 0x9E37_79B9 + 7).collect();
    let mut campaigns = 0usize;
    let mut total_bugs = 0usize;
    let mut total_admitted = 0usize;

    // Configuration 1: seed campaigns, decisions surfacing (and declined).
    for &seed in &seeds {
        let steps = 20 + seed % 17;
        let pre = reference_campaign(reference::SeedStrategy::new(seed), StopMask::ALL, steps);
        let post = refactored_campaign(seed_parts(), seed, StopMask::ALL, steps);
        assert_eq!(pre, post, "seed campaign diverged (seed {seed:#x})");
        campaigns += 1;
        total_bugs += pre.bugs.len();
        total_admitted += pre.admitted.len();
    }

    // Configuration 2: the full outer loop (explore/exploit, mutation minting,
    // nested forks, compose rebasing), no decision surfacing.
    let snap_only = StopMask(SNAP_BIT);
    for &seed in &seeds {
        let steps = 30 + seed % 23;
        let pre = reference_campaign(reference::CoverageStrategy::new(seed), snap_only, steps);
        let post = refactored_campaign(explore_exploit_parts(), seed, snap_only, steps);
        assert_eq!(
            pre, post,
            "explore/exploit campaign diverged (seed {seed:#x})"
        );
        campaigns += 1;
        total_bugs += pre.bugs.len();
        total_admitted += pre.admitted.len();
    }

    assert!(campaigns >= 50, "the gate requires >= 50 campaigns");
    assert!(total_bugs > 0, "the suite actually found bugs");
    assert!(total_admitted > 0, "the suite actually admitted entries");
}
