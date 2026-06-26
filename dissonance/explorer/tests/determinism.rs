// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — toy machine + determinism.
//!
//! Same `(strategy seed, toy machine)` ⇒ identical exploration trace and an
//! identical set of admitted corpus entries. A `HashMap` reaching the strategy
//! draw, the corpus index, or the bug fingerprint would make this flaky. Property
//! test, ≥256 cases.

mod common;

use common::{ToyCodec, ToyMachine, config};
use explorer::{Bug, CovScore, CoverageStrategy, Environment, Explorer, SnapId};
use proptest::prelude::*;

/// One admitted corpus entry, materialized for comparison.
type CorpusEntry = (SnapId, Environment, CovScore);

/// One full campaign's observable output: the bugs found and the admitted corpus.
fn campaign(seed: u64, steps: u64) -> (Vec<Bug>, Vec<CorpusEntry>) {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(seed),
        Box::new(ToyCodec),
    )
    .unwrap();
    let bugs = ex.explore(steps).unwrap();
    let corpus: Vec<CorpusEntry> = (0..ex.corpus().len())
        .map(|i| {
            let (snap, env, score) = ex.corpus().entry(i).unwrap();
            (snap, env.clone(), score)
        })
        .collect();
    (bugs, corpus)
}

proptest! {
    #![proptest_config(config(256))]

    /// Two campaigns with the same seed produce identical bugs and an identical
    /// admitted corpus.
    #[test]
    fn same_seed_yields_identical_campaign(seed in any::<u64>(), steps in 1u64..40) {
        let a = campaign(seed, steps);
        let b = campaign(seed, steps);
        prop_assert_eq!(a, b);
    }
}

/// A fixed, longer campaign is reproducible to the byte (a concrete witness
/// alongside the property).
#[test]
fn fixed_seed_long_campaign_is_reproducible() {
    let a = campaign(0xDEADBEEF, 250);
    let b = campaign(0xDEADBEEF, 250);
    assert_eq!(a, b);
    assert!(!a.0.is_empty(), "the campaign actually found bugs");
    assert!(!a.1.is_empty(), "and admitted corpus entries");
}
