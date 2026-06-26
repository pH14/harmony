// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end smoke: the engine drives the toy machine through both loops,
//! grows a corpus, and finds bugs. A scaffold check, not a gate.

mod common;

use common::{ToyCodec, ToyMachine};
use explorer::{CoverageStrategy, Explorer, SeedStrategy, StopConditions, StopMask};

#[test]
fn coverage_campaign_grows_corpus_and_finds_bugs() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0x1234),
        Box::new(ToyCodec),
    )
    .expect("genesis snapshot");

    let bugs = ex.explore(400).expect("no backend error");
    assert!(!bugs.is_empty(), "coverage search should find the toy bugs");
    assert!(!ex.corpus().is_empty(), "genesis runs admit snapshots");

    // Every reported bug is a crash or assertion, never a non-bug stop.
    for b in &bugs {
        assert!(b.stop.is_bug());
    }
}

#[test]
fn seed_campaign_is_pure_dst_no_corpus() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        SeedStrategy::new(0x99),
        Box::new(ToyCodec),
    )
    .expect("genesis snapshot");
    // Pure seed-driven: no decision classes surface, so the corpus never grows.
    ex.set_stop_conditions(StopConditions {
        deadline: None,
        on: StopMask::NONE,
    });

    let _ = ex.explore(200).expect("no backend error");
    assert_eq!(
        ex.corpus().len(),
        0,
        "seed campaign admits nothing (Multiverse alone)"
    );
}
