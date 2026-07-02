// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — toy machine + determinism.
//!
//! Same `(campaign seed, toy machine)` ⇒ identical exploration trace: the same
//! bugs and a byte-identical admitted frontier. A `HashMap` reaching a policy
//! draw, the frontier index, or the bug fingerprint would make this flaky.
//! Property test, ≥256 cases.

mod common;

use common::{ToyCodec, ToyMachine, config, pin_composition};
use explorer::{Bug, Explorer};
use proptest::prelude::*;

/// One admitted frontier entry, materialized for comparison: the exemplar's
/// (parent-independent) address and payload plus its genesis-complete env and
/// reward.
type Entry = (u64, Vec<u8>, Vec<u8>, u64);

/// One full campaign's observable output: the bugs found and the admitted
/// frontier.
fn campaign(seed: u64, steps: u64) -> (Vec<Bug>, Vec<Entry>) {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        seed,
    )
    .unwrap();
    let bugs = ex.explore(steps).unwrap();
    let frontier: Vec<Entry> = ex
        .frontier()
        .iter()
        .map(|(_, e)| {
            (
                e.exemplar.at.0,
                e.exemplar.suffix.bytes.clone(),
                e.env.bytes.clone(),
                e.reward.new_cells,
            )
        })
        .collect();
    (bugs, frontier)
}

proptest! {
    #![proptest_config(config(256))]

    /// Two campaigns with the same seed produce identical bugs and an identical
    /// admitted frontier.
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
    assert!(!a.1.is_empty(), "and admitted frontier entries");
}
