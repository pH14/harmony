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
use explorer::{Explorer, Machine, MachineError, StopConditions, StopMask};

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

/// Seal pairing is positional and exact: the first step admits an exemplar at
/// each of the two fork moments, and each entry holds its **own** fork's seal
/// (distinct, live handles) — pins the admit-pairing walk (a stuck or skewed
/// index would leave the second entry sealless or share one seal).
#[test]
fn each_admitted_entry_keeps_its_own_fork_seal() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 7).unwrap();
    ex.multiverse_step().unwrap();
    assert_eq!(ex.frontier().len(), 2, "one genesis run admits both forks");

    let s0 = ex
        .seal_of(explorer::ExemplarRef(0))
        .expect("entry 0 keeps its fork seal");
    let s1 = ex
        .seal_of(explorer::ExemplarRef(1))
        .expect("entry 1 keeps its fork seal");
    assert_ne!(s0, s1, "each entry holds its own seal");
    assert_eq!(ex.sealed_count(), 2);

    // And they are the right states: each seal replays to its exemplar's `at`.
    for (r, seal) in [
        (explorer::ExemplarRef(0), s0),
        (explorer::ExemplarRef(1), s1),
    ] {
        let at = ex.frontier().get(r).unwrap().exemplar.at.0;
        ex.machine_mut().replay(seal).unwrap();
        // The toy's answer-log length is its V-time / VTIME_STEP; hash equality
        // with a genesis re-drive is gated in replay.rs — here the cheap pin:
        // replaying a live seal succeeds (it was not dropped or swapped).
        assert!(at > 0);
    }
}

/// A `drop_snap` failure mid-eviction forgets nothing: a mapping is removed
/// only after its drop succeeds, so every undropped handle (the failed one
/// included) stays cached and the call is retryable — never silently orphaned.
#[test]
fn failed_seal_eviction_forgets_no_handle() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 7).unwrap();
    ex.multiverse_step().unwrap();
    assert_eq!(ex.sealed_count(), 2, "both fork seals are cached");

    // Sabotage: release the first seal behind the engine's back, so the
    // engine's own drop of that (now stale) handle fails loudly.
    let s0 = ex
        .seal_of(explorer::ExemplarRef(0))
        .expect("entry 0 is sealed");
    ex.machine_mut().drop_snap(s0).unwrap();

    // Eviction hits the stale handle first (BTreeMap order) and aborts —
    // forgetting nothing: every mapping is still cached, retry-ably.
    assert!(matches!(
        ex.evict_seals(),
        Err(MachineError::UnknownSnapshot(_))
    ));
    assert_eq!(
        ex.sealed_count(),
        2,
        "a failed eviction forgets no mapping (the failed one included)"
    );
    assert!(
        ex.seal_of(explorer::ExemplarRef(1)).is_some(),
        "undropped seals remain cached"
    );
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
