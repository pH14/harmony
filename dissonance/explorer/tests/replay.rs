// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — rollout replay (the OQ10 gate).
//!
//! A `rollout` run accumulates `env₁` (= `Machine::recorded_env` at the terminal
//! stop); `branch(base, env₁)` + re-run to the same deadline yields the **same
//! `hash`** — the recorded `Reproducer` reproduces the run bit-for-bit. And a
//! reported `Bug` replays from **genesis**: a run branched below a non-genesis
//! frontier exemplar is rebased (its branch-local delta composed with the entry's
//! genesis-complete env) so `branch(genesis, bug.env)` reproduces `bug.stop`.

mod common;

use std::collections::BTreeMap;

use common::{
    SNAP_AT, SNAP_AT2, TOTAL_DECISIONS, ToyCodec, ToyEnv, ToyMachine, VTIME_STEP, config, decode,
    drive_to_terminal, encode, pin_composition,
};
use explorer::{EnvCodec, ExemplarRef, Explorer, Machine, Moment, StopConditions, StopMask};
use proptest::prelude::*;

fn all() -> StopConditions {
    StopConditions {
        deadline: None,
        on: StopMask::ALL,
    }
}

fn explorer(seed: u64) -> Explorer<ToyMachine> {
    Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        seed,
    )
    .unwrap()
}

/// 2a — the recorded env replays the run that produced it, bit-for-bit, from the
/// same base.
#[test]
fn recorded_env_replays_to_the_same_hash() {
    let mut ex = explorer(7);
    let genesis = ex.genesis();

    let env0 = ToyCodec.seeded(42);
    let outcome = ex.rollout(genesis, &env0, &all()).unwrap();
    let h1 = ex.machine_mut().hash().unwrap();

    // branch(base, env₁) + re-run: the recorded overrides pin every decision, so
    // nothing surfaces and a single drive reproduces the run.
    ex.machine_mut().branch(genesis, &outcome.env).unwrap();
    let stop2 = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
    let h2 = ex.machine_mut().hash().unwrap();

    assert_eq!(h1, h2, "recorded env reproduces the run's hash");
    assert_eq!(outcome.stop, stop2, "and its terminal stop");
}

/// 2a, with a deadline — re-running to the *same deadline* matches.
#[test]
fn recorded_env_replays_to_the_same_deadline() {
    let mut ex = explorer(11);
    let genesis = ex.genesis();
    let until = StopConditions {
        deadline: Some(explorer::Moment(50)),
        on: StopMask::ALL,
    };

    let env0 = ToyCodec.seeded(99);
    let outcome = ex.rollout(genesis, &env0, &until).unwrap();
    let h1 = ex.machine_mut().hash().unwrap();

    ex.machine_mut().branch(genesis, &outcome.env).unwrap();
    let stop2 = drive_to_terminal(ex.machine_mut(), &until, None).unwrap();
    let h2 = ex.machine_mut().hash().unwrap();

    assert_eq!(h1, h2);
    assert_eq!(outcome.stop, stop2);
    assert!(matches!(
        outcome.stop,
        explorer::StopReason::Deadline { .. }
    ));
}

/// 2b core — a run branched off a non-genesis frontier seal recomposes to a
/// genesis-complete env that reproduces it bit-for-bit from genesis.
#[test]
fn compose_rebases_a_non_genesis_run_to_genesis() {
    let codec = ToyCodec;
    let mut ex = explorer(5);
    let genesis = ex.genesis();

    // One search-loop step populates the frontier with a non-genesis exemplar.
    ex.step().unwrap();
    assert!(
        !ex.frontier().is_empty(),
        "genesis run admits a frontier exemplar"
    );
    let r = ExemplarRef(0);
    let base_env = ex.frontier().get(r).unwrap().env.clone();
    let snap = ex.seal_of(r).expect("an admitted exemplar keeps its seal");
    assert_ne!(
        snap, genesis,
        "the exemplar's seal is a mid-run, non-genesis snapshot"
    );

    // Branch off that seal with a branch-local mutation and run a rollout.
    let branch_local_in = codec
        .mutate(&base_env, 0xABCD)
        .expect("toy codec is infallible");
    let outcome = ex.rollout(snap, &branch_local_in, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();

    // Rebase to genesis and replay from genesis — same hash, same stop.
    let composed = codec
        .compose(&base_env, &outcome.env)
        .expect("toy codec is infallible");
    ex.machine_mut().branch(genesis, &composed).unwrap();
    let g_stop = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
    let g_hash = ex.machine_mut().hash().unwrap();

    assert_eq!(
        mid_hash, g_hash,
        "composed genesis env reproduces the non-genesis run bit-for-bit"
    );
    assert_eq!(mid_stop, g_stop);
}

/// 2b — every bug an `explore` campaign reports replays from genesis (covers both
/// genesis-rooted and rebased-from-exemplar bugs).
#[test]
fn every_reported_bug_replays_from_genesis() {
    let mut ex = explorer(0xC0FFEE);
    let genesis = ex.genesis();

    let bugs = ex.explore(300).unwrap();
    assert!(!bugs.is_empty(), "the search finds the toy bugs");

    for bug in &bugs {
        ex.machine_mut().branch(genesis, &bug.env).unwrap();
        let stop = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
        assert_eq!(
            stop, bug.stop,
            "branch(genesis, bug.env) reproduces the reported stop"
        );
        assert!(stop.is_bug());
    }
}

/// 2b regression — a bug found below a mid-run exemplar **whose run continued
/// past it** must still replay from genesis. This guards the exemplar-env
/// pairing: the admitted env must be the prefix *as of the fork* (the run
/// accumulated more overrides after it), or the rebase `compose`s a mis-keyed
/// base and the reproducer fails to replay from genesis.
#[test]
fn bug_below_a_continued_snapshot_replays_from_genesis() {
    let codec = ToyCodec;
    let mut ex = explorer(0x1357);
    let genesis = ex.genesis();

    // A genesis run forks a SnapshotPoint at SNAP_AT and then runs on to the
    // terminal stop, so its terminal env carries overrides past the fork.
    ex.step().unwrap();
    let r = ExemplarRef(0);
    let entry_env = ex.frontier().get(r).unwrap().env.clone();
    let snap = ex.seal_of(r).expect("the exemplar keeps its fork seal");
    assert_ne!(snap, genesis, "the exemplar's seal is the mid-run snapshot");

    // The admitted env is the PREFIX env (overrides only before SNAP_AT), not
    // the whole-run env — this is exactly the pairing the blocking review
    // caught in task 12; the refactor must preserve it.
    let decoded = decode(&entry_env).unwrap();
    assert_eq!(decoded.base_offset, 0, "frontier env is genesis-complete");
    assert!(
        decoded.overrides.keys().all(|&k| k < SNAP_AT),
        "frontier env is the prefix as of the fork, not the continued run (keys: {:?})",
        decoded.overrides.keys().collect::<Vec<_>>()
    );

    // Branch off that seal with a delta that forces a suffix bug (assertion at
    // absolute index 5 == 3, which is past SNAP_AT, so it is suffix-controllable).
    let mut overrides = BTreeMap::new();
    overrides.insert(5 - SNAP_AT, 3u8);
    let branch_local_in = encode(&ToyEnv {
        base_offset: SNAP_AT,
        pos: TOTAL_DECISIONS,
        seed: decoded.seed,
        overrides,
    });
    let outcome = ex.rollout(snap, &branch_local_in, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();
    assert!(mid_stop.is_bug(), "the forced suffix override yields a bug");

    // Rebase to genesis exactly as `step` reports a bug, and replay.
    let bug_env = codec
        .compose(&entry_env, &outcome.env)
        .expect("toy codec is infallible");
    ex.machine_mut().branch(genesis, &bug_env).unwrap();
    let g_stop = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
    let g_hash = ex.machine_mut().hash().unwrap();

    assert_eq!(
        mid_hash, g_hash,
        "the rebased bug env reproduces the mid-fork run bit-for-bit from genesis"
    );
    assert_eq!(mid_stop, g_stop);
}

/// 2b round-2 regression — a bug found below a **nested** (non-genesis-rooted)
/// exemplar still replays from genesis. A fork below a non-genesis exemplar is
/// captured *branch-local* to it; its frontier entry must be rebased to
/// genesis-complete (through the parent entry's own genesis-complete env), or a
/// child mutation / bug rebase `compose`s from the wrong decision-index origin
/// and fails to replay.
#[test]
fn bug_below_a_nested_snapshot_replays_from_genesis() {
    let codec = ToyCodec;
    let mut ex = explorer(0x2468);
    let genesis = ex.genesis();

    // 1. A genesis run admits the first-generation exemplar at SNAP_AT.
    ex.step().unwrap();
    let r = ExemplarRef(0);
    let e_outer = ex.frontier().get(r).unwrap().env.clone();
    let s_outer = ex.seal_of(r).expect("first-generation seal");
    assert_eq!(
        ex.frontier().get(r).unwrap().exemplar.cut.at.0,
        SNAP_AT * VTIME_STEP,
        "first-generation exemplar sits at SNAP_AT"
    );

    // 2. Branch off that seal and drive down to its NESTED SnapshotPoint
    //    (SNAP_AT2), capturing the nested snapshot + its branch-local prefix env.
    let into_outer = codec
        .mutate(&e_outer, 0x99)
        .expect("toy codec is infallible");
    ex.machine_mut().branch(s_outer, &into_outer).unwrap();
    let (s_nested, prefix_nested) = common::drive_to_snapshot(ex.machine_mut(), &all());

    // 3. Rebase the nested prefix to genesis-complete exactly as step
    //    does for a fork below a non-genesis exemplar.
    let e_nested = codec
        .compose(&e_outer, &prefix_nested)
        .expect("toy codec is infallible");
    let de = decode(&e_nested).unwrap();
    assert_eq!(de.base_offset, 0, "nested frontier env is genesis-complete");
    assert_eq!(de.pos, SNAP_AT2, "and records the nested fork offset");
    assert!(
        de.overrides.keys().all(|&k| k < SNAP_AT2),
        "its overrides are the prefix up to the nested fork (keys: {:?})",
        de.overrides.keys().collect::<Vec<_>>()
    );

    // 4. Force a deep-suffix bug below the nested snapshot (a[7] == 2, past SNAP_AT2).
    let mut overrides = BTreeMap::new();
    overrides.insert(7 - SNAP_AT2, 2u8);
    let into_nested = encode(&ToyEnv {
        base_offset: SNAP_AT2,
        pos: TOTAL_DECISIONS,
        seed: de.seed,
        overrides,
    });
    let outcome = ex.rollout(s_nested, &into_nested, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();
    assert!(
        mid_stop.is_bug(),
        "the forced deep-suffix override yields a bug"
    );

    // 5. Rebase the bug through the nested base and replay from genesis.
    let bug_env = codec
        .compose(&e_nested, &outcome.env)
        .expect("toy codec is infallible");
    ex.machine_mut().branch(genesis, &bug_env).unwrap();
    let g_stop = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
    let g_hash = ex.machine_mut().hash().unwrap();

    assert_eq!(
        mid_hash, g_hash,
        "the nested bug env reproduces the run bit-for-bit from genesis"
    );
    assert_eq!(mid_stop, g_stop);
}

proptest! {
    #![proptest_config(config(256))]

    /// Task-93 property gate — `branch(genesis, compose(base, delta))` reproduces
    /// the run that produced `delta`, for arbitrary campaigns, frontier exemplars
    /// (genesis-rooted *and* nested, since a campaign admits both), and mutation
    /// salts. This is the ruling's condition for keeping `compose`: the composed
    /// genesis-complete env replays the non-genesis-based run bit-for-bit.
    ///
    /// Scope: this exercises `ToyCodec`/`ToyMachine`, so it validates the *model*
    /// (one-axis re-keying over splice-invariant seed answers), not the production
    /// `environment::EnvCodec::compose` under the tail-complete contract — that
    /// instantiation, against the real codec and `recorded_env`, is the frontier
    /// R2-adapter task's acceptance gate per the task-93 ruling.
    #[test]
    fn compose_rebase_replays_from_genesis(
        campaign_seed in any::<u64>(),
        warmup_steps in 1u64..8,
        entry_pick in any::<usize>(),
        salt in any::<u64>(),
    ) {
        let codec = ToyCodec;
        let mut ex = explorer(campaign_seed);
        let genesis = ex.genesis();
        let all = StopConditions { deadline: None, on: StopMask::ALL };

        // Warm the frontier so there are non-genesis exemplars to branch below.
        for _ in 0..warmup_steps {
            ex.step().unwrap();
        }
        prop_assume!(!ex.frontier().is_empty());
        let r = ex.frontier().nth(entry_pick as u64).unwrap();
        let base_env = ex.frontier().get(r).unwrap().env.clone();
        // Materialize covers both the still-sealed and (after eviction
        // elsewhere) re-materialized cases; here the eager seal is live.
        let snap = ex.materialize(r).unwrap();
        prop_assert_ne!(snap, genesis, "frontier seals are mid-run snapshots");

        // A run branched below the non-genesis exemplar produces the delta.
        let branch_local_in = codec.mutate(&base_env, salt).expect("toy codec is infallible");
        let outcome = ex.rollout(snap, &branch_local_in, &all).unwrap();
        let mid_hash = ex.machine_mut().hash().unwrap();
        let mid_stop = outcome.stop.clone();

        // The property: composing the base with the delta yields a
        // genesis-complete env that reproduces that run from genesis.
        let composed = codec.compose(&base_env, &outcome.env).expect("toy codec is infallible");
        prop_assert_eq!(
            decode(&composed).unwrap().base_offset, 0,
            "the composed env is genesis-complete"
        );
        ex.machine_mut().branch(genesis, &composed).unwrap();
        let g_stop = drive_to_terminal(ex.machine_mut(), &all, None).unwrap();
        let g_hash = ex.machine_mut().hash().unwrap();

        prop_assert_eq!(mid_hash, g_hash, "composed env replays the run bit-for-bit");
        prop_assert_eq!(mid_stop, g_stop, "and reproduces its terminal stop");
    }
}

/// 2b round-2 end-to-end — drive a real `explore` campaign (which forks both
/// genesis-rooted and **nested** exemplars) and assert that **every** admitted
/// frontier entry reproduces its own sealed state from genesis:
/// `branch(genesis, entry.env)` run to the exemplar's `at` must match
/// `replay(seal)` bit-for-bit. A nested entry admitted branch-local mis-keys
/// here and fails.
#[test]
fn every_frontier_entry_replays_its_seal_from_genesis() {
    let mut ex = explorer(0xBEEF);
    let genesis = ex.genesis();
    ex.explore(300).unwrap();

    let entries: Vec<(ExemplarRef, explorer::Reproducer, u64)> = ex
        .frontier()
        .iter()
        .map(|(r, e)| (r, e.env.clone(), e.exemplar.cut.at.0))
        .collect();
    assert!(
        !entries.is_empty(),
        "the campaign admitted frontier entries"
    );

    for (i, (r, env, at)) in entries.iter().enumerate() {
        assert_eq!(
            decode(env).unwrap().base_offset,
            0,
            "frontier entry {i} must be genesis-complete"
        );

        // The sealed state's own hash (materialize returns the live seal).
        let seal = ex.materialize(*r).unwrap();
        ex.machine_mut().replay(seal).unwrap();
        let seal_hash = ex.machine_mut().hash().unwrap();

        // Branch from genesis with the entry env and run to the exemplar's
        // moment; the reproduced prefix must match the sealed state exactly.
        let until_at = StopConditions {
            deadline: Some(Moment(*at)),
            on: StopMask::ALL,
        };
        ex.machine_mut().branch(genesis, env).unwrap();
        drive_to_terminal(ex.machine_mut(), &until_at, None).unwrap();
        let from_genesis_hash = ex.machine_mut().hash().unwrap();

        assert_eq!(
            seal_hash, from_genesis_hash,
            "frontier entry {i} (at {at}) must reproduce its sealed state from genesis"
        );
    }
}
