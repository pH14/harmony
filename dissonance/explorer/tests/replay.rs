// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — Timeline replay (the OQ10 gate).
//!
//! A `timeline` run accumulates `env₁` (= `Machine::recorded_env` at the terminal
//! stop); `branch(base, env₁)` + re-run to the same deadline yields the **same
//! `hash`** — the recorded `Environment` reproduces the run bit-for-bit. And a
//! reported `Bug` replays from **genesis**: a run branched off a non-genesis
//! corpus snapshot is rebased (its branch-local delta composed with the snapshot's
//! genesis-complete base) so `branch(genesis, bug.env)` reproduces `bug.stop`.

mod common;

use std::collections::BTreeMap;

use common::{
    SNAP_AT, SNAP_AT2, TOTAL_DECISIONS, ToyCodec, ToyEnv, ToyMachine, VTIME_STEP, config, decode,
    drive_to_terminal, encode,
};
use explorer::{CoverageStrategy, EnvCodec, Explorer, Machine, StopConditions, StopMask, VTime};
use proptest::prelude::*;

fn all() -> StopConditions {
    StopConditions {
        deadline: None,
        on: StopMask::ALL,
    }
}

/// 2a — the recorded env replays the run that produced it, bit-for-bit, from the
/// same base.
#[test]
fn recorded_env_replays_to_the_same_hash() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(7),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();

    let env0 = ToyCodec.seeded(42);
    let outcome = ex.timeline(genesis, &env0, &all()).unwrap();
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
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(11),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();
    let until = StopConditions {
        deadline: Some(explorer::VTime(50)),
        on: StopMask::ALL,
    };

    let env0 = ToyCodec.seeded(99);
    let outcome = ex.timeline(genesis, &env0, &until).unwrap();
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

/// 2b core — a run branched off a non-genesis corpus snapshot recomposes to a
/// genesis-complete env that reproduces it bit-for-bit from genesis.
#[test]
fn compose_rebases_a_non_genesis_run_to_genesis() {
    let codec = ToyCodec;
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(5),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();

    // One Multiverse step populates the corpus with a non-genesis base.
    ex.multiverse_step().unwrap();
    assert!(
        !ex.corpus().is_empty(),
        "genesis run admits a snapshot base"
    );
    let (snap, base_env, _) = ex.corpus().entry(0).unwrap();
    let base_env = base_env.clone();
    assert_ne!(
        snap, genesis,
        "the corpus base is a mid-run, non-genesis snapshot"
    );

    // Branch off that snapshot with a branch-local mutation and run a Timeline.
    let branch_local_in = codec.mutate(&base_env, 0xABCD);
    let outcome = ex.timeline(snap, &branch_local_in, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();

    // Rebase to genesis and replay from genesis — same hash, same stop.
    let composed = codec.compose(&base_env, &outcome.env);
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
/// genesis-rooted and rebased-from-snapshot bugs).
#[test]
fn every_reported_bug_replays_from_genesis() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0xC0FFEE),
        Box::new(ToyCodec),
    )
    .unwrap();
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

/// 2b regression — a bug found below a mid-run snapshot **whose run continued
/// past it** must still replay from genesis. This guards the corpus-env/snapshot
/// pairing: the admitted base must be the prefix env *as of the snapshot* (the
/// run accumulated more overrides after the fork), or the rebase `compose`s a
/// mis-keyed base and the reproducer fails to replay from genesis.
#[test]
fn bug_below_a_continued_snapshot_replays_from_genesis() {
    let codec = ToyCodec;
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0x1357),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();

    // A genesis run forks a SnapshotPoint at SNAP_AT and then runs on to the
    // terminal stop, so its terminal env carries overrides past the snapshot.
    ex.multiverse_step().unwrap();
    let (snap, corpus_env, _) = ex.corpus().entry(0).unwrap();
    let corpus_env = corpus_env.clone();
    assert_ne!(snap, genesis, "the corpus base is the mid-run snapshot");

    // The admitted base is the PREFIX env (overrides only before SNAP_AT), not the
    // whole-run env — this is exactly the pairing the blocking review caught.
    let decoded = decode(&corpus_env).unwrap();
    assert_eq!(decoded.base_offset, 0, "corpus base is genesis-complete");
    assert!(
        decoded.overrides.keys().all(|&k| k < SNAP_AT),
        "corpus env is the prefix as of the snapshot, not the continued run (keys: {:?})",
        decoded.overrides.keys().collect::<Vec<_>>()
    );

    // Branch off that snapshot with a delta that forces a suffix bug (assertion at
    // absolute index 5 == 3, which is past SNAP_AT, so it is suffix-controllable).
    let mut overrides = BTreeMap::new();
    overrides.insert(5 - SNAP_AT, 3u8);
    let branch_local_in = encode(&ToyEnv {
        base_offset: SNAP_AT,
        pos: TOTAL_DECISIONS,
        seed: decoded.seed,
        overrides,
    });
    let outcome = ex.timeline(snap, &branch_local_in, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();
    assert!(mid_stop.is_bug(), "the forced suffix override yields a bug");

    // Rebase to genesis exactly as `multiverse_step` reports a bug, and replay.
    let bug_env = codec.compose(&corpus_env, &outcome.env);
    ex.machine_mut().branch(genesis, &bug_env).unwrap();
    let g_stop = drive_to_terminal(ex.machine_mut(), &all(), None).unwrap();
    let g_hash = ex.machine_mut().hash().unwrap();

    assert_eq!(
        mid_hash, g_hash,
        "the rebased bug env reproduces the mid-snapshot run bit-for-bit from genesis"
    );
    assert_eq!(mid_stop, g_stop);
}

/// 2b round-2 regression — a bug found below a **nested** (non-genesis-rooted)
/// snapshot still replays from genesis. A snapshot forked below a non-genesis
/// corpus base is captured *branch-local* to that base; its corpus entry must be
/// rebased to genesis-complete (through the base's own genesis-complete env), or a
/// child mutation / bug rebase `compose`s from the wrong decision-index origin and
/// fails to replay. (Latent until the toy could fork a deeper SnapshotPoint.)
#[test]
fn bug_below_a_nested_snapshot_replays_from_genesis() {
    let codec = ToyCodec;
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0x2468),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();

    // 1. A genesis run admits the first-generation base snapshot at SNAP_AT.
    ex.multiverse_step().unwrap();
    let (s_outer, e_outer, _) = ex.corpus().entry(0).unwrap();
    let (s_outer, e_outer) = (s_outer, e_outer.clone());
    assert_eq!(
        decode(&e_outer).unwrap().pos,
        SNAP_AT,
        "first-generation base sits at SNAP_AT"
    );

    // 2. Branch off that base and drive down to its NESTED SnapshotPoint (SNAP_AT2),
    //    capturing the nested snapshot + its branch-local prefix env.
    let into_outer = codec.mutate(&e_outer, 0x99);
    ex.machine_mut().branch(s_outer, &into_outer).unwrap();
    let (s_nested, prefix_nested) = common::drive_to_snapshot(ex.machine_mut(), &all());

    // 3. Rebase the nested prefix to genesis-complete exactly as multiverse_step now
    //    does for a snapshot forked below a non-genesis base.
    let e_nested = codec.compose(&e_outer, &prefix_nested);
    let de = decode(&e_nested).unwrap();
    assert_eq!(de.base_offset, 0, "nested corpus base is genesis-complete");
    assert_eq!(de.pos, SNAP_AT2, "and records the nested snapshot offset");
    assert!(
        de.overrides.keys().all(|&k| k < SNAP_AT2),
        "its overrides are the prefix up to the nested snapshot (keys: {:?})",
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
    let outcome = ex.timeline(s_nested, &into_nested, &all()).unwrap();
    let mid_hash = ex.machine_mut().hash().unwrap();
    let mid_stop = outcome.stop.clone();
    assert!(
        mid_stop.is_bug(),
        "the forced deep-suffix override yields a bug"
    );

    // 5. Rebase the bug through the nested base and replay from genesis.
    let bug_env = codec.compose(&e_nested, &outcome.env);
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
    /// the run that produced `delta`, for arbitrary campaigns, corpus bases
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
        let mut ex = Explorer::new(
            ToyMachine::new(),
            CoverageStrategy::new(campaign_seed),
            Box::new(ToyCodec),
        )
        .unwrap();
        let genesis = ex.genesis();
        let all = StopConditions { deadline: None, on: StopMask::ALL };

        // Warm the corpus so there are non-genesis bases to branch below.
        for _ in 0..warmup_steps {
            ex.multiverse_step().unwrap();
        }
        prop_assume!(!ex.corpus().is_empty());
        let (snap, base_env, _) = ex.corpus().entry(entry_pick % ex.corpus().len()).unwrap();
        let base_env = base_env.clone();
        prop_assert_ne!(snap, genesis, "corpus bases are mid-run snapshots");

        // A run branched below the non-genesis base produces the delta.
        let branch_local_in = codec.mutate(&base_env, salt);
        let outcome = ex.timeline(snap, &branch_local_in, &all).unwrap();
        let mid_hash = ex.machine_mut().hash().unwrap();
        let mid_stop = outcome.stop.clone();

        // The property: composing the base with the delta yields a
        // genesis-complete env that reproduces that run from genesis.
        let composed = codec.compose(&base_env, &outcome.env);
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
/// genesis-rooted and **nested** snapshots) and assert that **every** admitted
/// corpus entry reproduces its own snapshot from genesis: `branch(genesis, entry)`
/// run to the snapshot's position must match `replay(snap)` bit-for-bit. This is
/// the invariant the round-2 blocking item violated — a nested entry admitted
/// branch-local mis-keys here and fails (verified against the unfixed engine).
#[test]
fn every_corpus_entry_replays_its_snapshot_from_genesis() {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0xBEEF),
        Box::new(ToyCodec),
    )
    .unwrap();
    let genesis = ex.genesis();
    ex.explore(300).unwrap();

    // Snapshot the corpus so the machine can be driven freely below.
    let entries: Vec<(explorer::SnapId, explorer::Environment, u64)> = (0..ex.corpus().len())
        .map(|i| {
            let (snap, env, _) = ex.corpus().entry(i).unwrap();
            let pos = decode(env).unwrap().pos;
            (snap, env.clone(), pos)
        })
        .collect();
    assert!(!entries.is_empty(), "the campaign admitted corpus entries");

    for (i, (snap, env, pos)) in entries.iter().enumerate() {
        assert_eq!(
            decode(env).unwrap().base_offset,
            0,
            "corpus entry {i} must be genesis-complete"
        );

        // The snapshot's own frozen-prefix hash.
        ex.machine_mut().replay(*snap).unwrap();
        let snap_hash = ex.machine_mut().hash().unwrap();

        // Branch from genesis with the entry env and run to the snapshot's position;
        // the reproduced prefix must match the snapshot exactly.
        let until_pos = StopConditions {
            deadline: Some(VTime(pos * VTIME_STEP)),
            on: StopMask::ALL,
        };
        ex.machine_mut().branch(genesis, env).unwrap();
        drive_to_terminal(ex.machine_mut(), &until_pos, None).unwrap();
        let from_genesis_hash = ex.machine_mut().hash().unwrap();

        assert_eq!(
            snap_hash, from_genesis_hash,
            "corpus entry {i} (pos {pos}) must reproduce its snapshot from genesis"
        );
    }
}
