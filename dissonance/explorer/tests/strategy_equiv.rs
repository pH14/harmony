// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — seed vs coverage equivalence of artifact.
//!
//! `SeedStrategy` (no overrides) and `CoverageStrategy` (sparse overrides) both
//! emit a replayable `Environment`; replaying either reproduces its run. The
//! reproducible artifact is the same shape regardless of how the search produced
//! it — the AFL/FoundationDB convergence the design rests on.

mod common;

use common::{ToyCodec, ToyMachine, decode, drive_to_terminal};
use explorer::{
    CoverageStrategy, EnvCodec, Explorer, Machine, SeedStrategy, StopConditions, StopMask, Strategy,
};

/// Run one Timeline with `strat` answering decisions under `mask`, then assert the
/// recorded env replays the run bit-for-bit. Returns the decoded override count so
/// the caller can characterize the artifact.
fn run_and_replay<S: Strategy + 'static>(strat: S, mask: StopMask, base_seed: u64) -> usize {
    let mut ex = Explorer::new(ToyMachine::new(), strat, Box::new(ToyCodec)).unwrap();
    let genesis = ex.genesis();
    let until = StopConditions {
        deadline: None,
        on: mask,
    };

    let env0 = ToyCodec.seeded(base_seed);
    let outcome = ex.timeline(genesis, &env0, &until).unwrap();
    let h1 = ex.machine_mut().hash().unwrap();

    // Replay the recorded env from the same base.
    ex.machine_mut().branch(genesis, &outcome.env).unwrap();
    let stop2 = drive_to_terminal(ex.machine_mut(), &until, None).unwrap();
    let h2 = ex.machine_mut().hash().unwrap();

    assert_eq!(h1, h2, "recorded artifact reproduces its run");
    assert_eq!(outcome.stop, stop2);

    decode(&outcome.env).unwrap().overrides.len()
}

#[test]
fn seed_strategy_artifact_is_pure_seed_and_replays() {
    // No classes surface → no overrides → a pure-seed artifact.
    let overrides = run_and_replay(SeedStrategy::new(1), StopMask::NONE, 7);
    assert_eq!(overrides, 0, "SeedStrategy emits no overrides (pure DST)");
}

#[test]
fn coverage_strategy_artifact_has_overrides_and_replays() {
    // Every class surfaces → the strategy pins overrides → still replays.
    let overrides = run_and_replay(CoverageStrategy::new(1), StopMask::ALL, 7);
    assert!(overrides > 0, "CoverageStrategy pins overrides");
}

/// Both artifacts replaying is the point: the two strategies are interchangeable
/// at the reproducer boundary.
#[test]
fn both_strategies_emit_replayable_artifacts() {
    for seed in [0u64, 3, 42, 9999] {
        run_and_replay(SeedStrategy::new(seed), StopMask::NONE, seed);
        run_and_replay(CoverageStrategy::new(seed), StopMask::ALL, seed);
    }
}
