// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end smoke: the engine drives the toy machine through both loops,
//! grows a frontier, and finds bugs. A scaffold check, not a gate.

mod common;

use common::{ToyCodec, ToyMachine, pin_composition, seed_composition};
use explorer::{Composition, Explorer, StopConditions, StopMask};

#[test]
fn coverage_campaign_grows_frontier_and_finds_bugs() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        0x1234,
    )
    .expect("genesis snapshot");

    let bugs = ex.explore(400).expect("no backend error");
    assert!(!bugs.is_empty(), "the search should find the toy bugs");
    assert!(!ex.frontier().is_empty(), "genesis runs admit exemplars");

    // Every reported bug is a crash or assertion, never a non-bug stop.
    for b in &bugs {
        assert!(b.stop.is_bug());
    }
}

/// The all-defaults composition (the declining tactic) also explores usefully —
/// bugs come from explore seeds and mutation-pinned overrides alone.
#[test]
fn default_composition_campaign_finds_bugs() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        Composition::defaults(),
        0xABCD,
    )
    .expect("genesis snapshot");
    let bugs = ex.explore(400).expect("no backend error");
    assert!(!bugs.is_empty(), "declined decisions still find seed bugs");
    assert!(!ex.frontier().is_empty());
}

#[test]
fn seed_campaign_is_pure_dst_no_frontier() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        seed_composition(),
        0x99,
    )
    .expect("genesis snapshot");
    // Pure seed-driven: no decision classes surface, so the frontier never grows.
    ex.set_stop_conditions(StopConditions {
        deadline: None,
        on: StopMask::NONE,
    });

    let _ = ex.explore(200).expect("no backend error");
    assert_eq!(
        ex.frontier().len(),
        0,
        "seed campaign admits nothing (Progression alone)"
    );
}
