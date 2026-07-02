// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — seal GC.
//!
//! `drop_snap` is issued for every fork seal not admitted, `evict_seals`
//! releases every live seal, and no `SnapId` is used after it is dropped. The
//! toy machine enforces the last half itself: branching or replaying a dropped
//! handle returns `MachineError::UnknownSnapshot`, so a clean `explore` under
//! aggressive per-step seal eviction (which forces re-materialization) is
//! itself proof that dropped handles are never reused.

mod common;

use common::{ToyCodec, ToyMachine, pin_composition};
use explorer::{Explorer, MachineError, StopConditions, StopMask};

/// Seal accounting: live backend handles are exactly genesis + the admitted
/// entries' seals; every non-admitted fork seal was dropped; and evicting all
/// seals leaves only genesis alive.
#[test]
fn seal_lifecycle_never_leaks_a_handle() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        0x5EED,
    )
    .unwrap();

    ex.explore(120).expect("no dropped snapshot is ever reused");

    let sealed = ex.sealed_count();
    assert!(sealed >= 1, "admitted exemplars hold seals");
    let m = ex.machine_mut();
    assert!(
        m.dropped_count() > 0,
        "non-admitted fork seals were dropped"
    );
    assert_eq!(
        m.live_snaps(),
        1 + sealed,
        "live snapshots = genesis + admitted seals (no leak)"
    );

    // Evicting every seal releases the handles (only genesis remains) and
    // leaves the frontier intact.
    let entries = ex.frontier().len();
    ex.evict_seals().unwrap();
    assert_eq!(ex.sealed_count(), 0);
    assert_eq!(ex.frontier().len(), entries, "eviction never drops entries");
    assert_eq!(
        ex.machine_mut().live_snaps(),
        1,
        "only the genesis snapshot survives seal eviction"
    );

    // And the campaign continues cleanly across the eviction: exploits
    // re-materialize rather than touching any dropped handle (the toy would
    // return UnknownSnapshot and abort explore if one were reused).
    ex.explore(60)
        .expect("re-materialization never reuses a dropped handle");
}

/// A `recorded_env` failure *after* the `SnapshotPoint` seal already succeeded
/// must release that handle, not leak it. The timeline aborts with the original
/// error and the freshly-minted seal is dropped (only genesis remains).
#[test]
fn fork_seal_is_dropped_if_prefix_capture_fails() {
    let machine = ToyMachine::new().fail_recorded_env();
    let mut ex = Explorer::new(machine, Box::new(ToyCodec), pin_composition(), 1).unwrap();
    let genesis = ex.genesis();
    let env0 = explorer::EnvCodec::seeded(&ToyCodec, 7);

    // A genesis run forks a SnapshotPoint; `snapshot()` succeeds but the prefix-env
    // capture fails, so the timeline aborts with that transport error.
    let until = StopConditions {
        deadline: None,
        on: StopMask::ALL,
    };
    let result = ex.timeline(genesis, &env0, &until);
    assert!(matches!(result, Err(MachineError::Transport(_))));

    // The seal minted at the fork was dropped — no leaked handle.
    let m = ex.machine_mut();
    assert_eq!(
        m.live_snaps(),
        1,
        "only the genesis snapshot remains; the fork seal was released"
    );
    assert!(m.dropped_count() >= 1, "the fork seal was drop_snap'd");
}

/// A direct `timeline` call leaves its forks pending; the next call must
/// `drop_snap` them (never silently forget), so repeated direct runs cannot
/// leak backend handles.
#[test]
fn leftover_pending_forks_are_dropped_not_forgotten() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 3).unwrap();
    let genesis = ex.genesis();
    let until = StopConditions {
        deadline: None,
        on: StopMask::ALL,
    };

    let env0 = explorer::EnvCodec::seeded(&ToyCodec, 1);
    ex.timeline(genesis, &env0, &until).unwrap();
    let live_after_first = ex.machine_mut().live_snaps();
    assert!(
        live_after_first > 1,
        "the direct run left fork seals pending"
    );

    // The second direct run drops the leftovers before running.
    let env1 = explorer::EnvCodec::seeded(&ToyCodec, 2);
    ex.timeline(genesis, &env1, &until).unwrap();
    let dropped = ex.machine_mut().dropped_count();
    assert!(
        dropped >= live_after_first - 1,
        "the prior run's pending fork seals were drop_snap'd"
    );
}
