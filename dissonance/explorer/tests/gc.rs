// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — corpus GC.
//!
//! `drop_snap` is issued for evicted entries, and no `SnapId` is used after it is
//! dropped. The toy machine enforces the second half itself: branching or
//! replaying a dropped handle returns `MachineError::UnknownSnapshot`, so a clean
//! `explore` under a tiny corpus capacity (which forces eviction) is itself proof
//! that evicted handles are never reused.

mod common;

use common::{ToyCodec, ToyMachine};
use explorer::{CoverageStrategy, Explorer, MachineError, StopConditions, StopMask};

#[test]
fn eviction_drops_snapshots_and_never_reuses_them() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0x5EED),
        Box::new(ToyCodec),
    )
    .unwrap();
    // A tiny capacity forces eviction as soon as a third novel base is admitted.
    let cap = 2;
    ex.set_corpus_capacity(cap).unwrap();

    // A clean run is the no-reuse proof: had any dropped (evicted or non-admitted)
    // handle been branched/replayed, the toy would have returned UnknownSnapshot
    // and `explore` would have aborted with Err here.
    ex.explore(300).expect("no dropped snapshot is ever reused");

    let corpus_len = ex.corpus().len();
    assert!(corpus_len <= cap, "corpus stays within capacity");
    assert!(corpus_len >= 1, "the corpus did admit bases");

    // Live handles = genesis + the kept corpus bases; every other snapshot the
    // campaign minted (non-novel and evicted alike) was dropped, nothing leaked.
    let m = ex.machine_mut();
    assert!(
        m.dropped_count() > 0,
        "evicted/non-admitted snapshots were dropped"
    );
    assert_eq!(
        m.live_snaps(),
        1 + corpus_len,
        "live snapshots = genesis + kept bases (no leak)"
    );
}

/// A `recorded_env` failure *after* the `SnapshotPoint` snapshot already succeeded
/// must release that handle, not leak it. The timeline aborts with the original
/// error and the freshly-minted snapshot is dropped (only genesis remains).
#[test]
fn snapshot_handle_is_dropped_if_prefix_capture_fails() {
    let machine = ToyMachine::new().fail_recorded_env();
    let mut ex = Explorer::new(machine, CoverageStrategy::new(1), Box::new(ToyCodec)).unwrap();
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

    // The snapshot minted at the fork was dropped — no leaked handle.
    let m = ex.machine_mut();
    assert_eq!(
        m.live_snaps(),
        1,
        "only the genesis snapshot remains; the fork snapshot was released"
    );
    assert!(m.dropped_count() >= 1, "the fork snapshot was drop_snap'd");
}

/// Re-capacitating a **non-empty** corpus must `drop_snap` the entries it discards,
/// never silently forget their handles.
#[test]
fn set_corpus_capacity_drops_discarded_entries() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0xA11CE),
        Box::new(ToyCodec),
    )
    .unwrap();

    // Grow a corpus first.
    ex.explore(40).unwrap();
    let kept = ex.corpus().len();
    assert!(kept >= 1, "the campaign admitted entries to discard");
    let dropped_before = ex.machine_mut().dropped_count();

    // Re-capacitating discards every entry; each kept snapshot must be released.
    ex.set_corpus_capacity(8).unwrap();
    assert!(ex.corpus().is_empty(), "the corpus was discarded");

    let m = ex.machine_mut();
    assert_eq!(
        m.dropped_count(),
        dropped_before + kept,
        "every discarded entry's snapshot was drop_snap'd"
    );
    assert_eq!(
        m.live_snaps(),
        1,
        "only the genesis snapshot remains after discarding the corpus"
    );
}
