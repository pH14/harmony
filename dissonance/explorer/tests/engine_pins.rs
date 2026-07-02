// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mutation-pinning gates for `Explorer` internals that need the toy machine:
//! the default `stop_conditions` and the genesis-rebase decision in
//! `progression_step` (a fork below a non-genesis exemplar must be rebased to
//! genesis-complete and admitted — not dropped, not admitted branch-local).

mod common;

use common::{SNAP_AT2, ToyCodec, ToyMachine, VTIME_STEP, decode, pin_composition};
use explorer::{
    Composition, CoverageArchive, DeclineTactic, ExemplarRef, Explorer, Frontier, IdentityCells,
    Prng, Selector, StopMask, TerminalOracle,
};

/// The default `StopConditions` are `StopMask::ALL` with no deadline — not the
/// type's `Default` (which is `StopMask::NONE`). Pins `stop_conditions()`.
#[test]
fn default_stop_conditions_are_all_no_deadline() {
    let ex = Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 0).unwrap();
    assert_eq!(ex.stop_conditions().on, StopMask::ALL);
    assert_eq!(ex.stop_conditions().deadline, None);
}

/// A selector that deterministically **exploits the first frontier entry** (the
/// `SNAP_AT` exemplar) as soon as one exists, so the exploit run forks the
/// nested `SNAP_AT2` point below a non-genesis base every step after the first.
struct ForceExploitSelector;

impl Selector for ForceExploitSelector {
    fn choose(&mut self, frontier: &Frontier, _rng: &mut Prng) -> Option<ExemplarRef> {
        frontier.nth(0)
    }
    fn reward(&mut self, _chosen: ExemplarRef, _r: explorer::Reward) {}
}

/// A fork below a non-genesis exemplar must be rebased through that entry's
/// genesis-complete env and admitted — pins the compose-at-admission decision
/// (admitted branch-local, the nested entry's `base_offset` would be non-zero;
/// dropped, the frontier would never gain a `SNAP_AT2` entry from exploits).
#[test]
fn nested_fork_below_a_non_genesis_base_is_admitted_genesis_complete() {
    let parts = Composition {
        tactic: Box::new(DeclineTactic::new()),
        selector: Box::new(ForceExploitSelector),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    };
    let mut ex = Explorer::new(ToyMachine::new(), Box::new(ToyCodec), parts, 42).unwrap();

    // Step 1: a genesis run admits the first-generation exemplars.
    ex.progression_step().unwrap();
    let after_seed = ex.frontier().len();
    assert!(after_seed >= 1, "the genesis run seeded the frontier");

    // Exploit the SNAP_AT exemplar until a mutation makes the nested prefix
    // novel; the nested fork must then be admitted, growing the frontier.
    for _ in 0..40 {
        ex.progression_step().unwrap();
        if ex.frontier().len() > after_seed {
            break;
        }
    }
    assert!(
        ex.frontier().len() > after_seed,
        "a nested fork was admitted, not dropped"
    );

    // Every admitted entry — the nested ones included — is genesis-complete,
    // and at least one truly sits at the nested fork point.
    let mut nested = 0;
    for (i, (_, entry)) in ex.frontier().iter().enumerate() {
        assert_eq!(
            decode(&entry.env).unwrap().base_offset,
            0,
            "frontier entry {i} is genesis-complete"
        );
        if entry.exemplar.at.0 == SNAP_AT2 * VTIME_STEP {
            nested += 1;
            assert_eq!(
                decode(&entry.env).unwrap().pos,
                SNAP_AT2,
                "the nested entry records its own fork offset"
            );
        }
    }
    assert!(nested >= 1, "the growth came from a nested (SNAP_AT2) fork");
}
