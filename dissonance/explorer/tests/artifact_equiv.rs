// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 (task 12) — decline vs pin equivalence of artifact.
//!
//! A declining tactic (no overrides) and a pinning tactic (sparse overrides)
//! both emit a replayable `Reproducer`; replaying either reproduces its run.
//! The reproducible artifact is the same shape regardless of how the search
//! produced it — the AFL/FoundationDB convergence the design rests on, now
//! stated over [`Tactic`]s instead of the retired `Strategy`.

mod common;

use common::{PinTactic, ToyCodec, ToyMachine, decode, drive_to_terminal};
use explorer::{
    Composition, CoverageArchive, DeclineTactic, EnvCodec, ExploreExploitSelector, Explorer,
    IdentityCells, Machine, StopConditions, StopMask, Tactic, TerminalOracle,
};

fn composition(tactic: Box<dyn Tactic>) -> Composition {
    Composition {
        tactic,
        selector: Box::new(ExploreExploitSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    }
}

/// Run one rollout with `tactic` answering decisions under `mask`, then assert
/// the recorded env replays the run bit-for-bit. Returns the decoded override
/// count so the caller can characterize the artifact.
fn run_and_replay(tactic: Box<dyn Tactic>, mask: StopMask, base_seed: u64) -> usize {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        composition(tactic),
        base_seed,
    )
    .unwrap();
    let genesis = ex.genesis();
    let until = StopConditions {
        deadline: None,
        on: mask,
    };

    let env0 = ToyCodec.seeded(base_seed);
    let outcome = ex.rollout(genesis, &env0, &until).unwrap();
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
fn decline_tactic_artifact_is_pure_seed_and_replays() {
    // Decisions surface but are declined → no overrides → a pure-seed artifact.
    let overrides = run_and_replay(Box::new(DeclineTactic::new()), StopMask::ALL, 7);
    assert_eq!(overrides, 0, "DeclineTactic emits no overrides (pure DST)");
}

#[test]
fn pin_tactic_artifact_has_overrides_and_replays() {
    // Every class surfaces → the tactic pins overrides → still replays.
    let overrides = run_and_replay(Box::new(PinTactic), StopMask::ALL, 7);
    assert!(overrides > 0, "PinTactic pins overrides");
}

/// Both artifacts replaying is the point: the two tactics are interchangeable
/// at the reproducer boundary.
#[test]
fn both_tactics_emit_replayable_artifacts() {
    for seed in [0u64, 3, 42, 9999] {
        run_and_replay(Box::new(DeclineTactic::new()), StopMask::ALL, seed);
        run_and_replay(Box::new(PinTactic), StopMask::ALL, seed);
    }
}
