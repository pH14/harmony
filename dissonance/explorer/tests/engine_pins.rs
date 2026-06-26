// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mutation-pinning gates for `Explorer` internals that need the toy machine: the
//! default `stop_conditions` and the `base_snap == genesis` rebase decision in
//! `multiverse_step` (a snapshot forked below a non-genesis base must be rebased to
//! genesis-complete and admitted — not dropped).

mod common;

use std::collections::BTreeMap;

use common::{SNAP_AT, TOTAL_DECISIONS, ToyCodec, ToyEnv, ToyMachine, decode, encode};
use explorer::{
    Answer, Corpus, CoverageStrategy, EnvCodec, Environment, Explorer, SnapId, StopMask, Strategy,
};

/// The default `StopConditions` are `StopMask::ALL` with no deadline — not the
/// type's `Default` (which is `StopMask::NONE`). Pins `stop_conditions()`.
#[test]
fn default_stop_conditions_are_all_no_deadline() {
    let ex = Explorer::new(
        ToyMachine::new(),
        CoverageStrategy::new(0),
        Box::new(ToyCodec),
    )
    .unwrap();
    assert_eq!(ex.stop_conditions().on, StopMask::ALL);
    assert_eq!(ex.stop_conditions().deadline, None);
}

/// A strategy that first seeds the corpus from genesis, then deterministically
/// **exploits the first corpus entry** (the `SNAP_AT` snapshot), branching off it
/// with a delta that diverges at `SNAP_AT` so the nested fork's prefix is novel.
struct ForceExploit;

impl Strategy for ForceExploit {
    fn choose(&mut self, _ctx: &[u8], _coverage: &[u8]) -> Answer {
        Answer(vec![1])
    }

    fn next_env(
        &mut self,
        corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment) {
        match corpus.entry(0) {
            // Empty corpus → seed it from genesis.
            None => (genesis, env.seeded(0)),
            // Exploit the first (SNAP_AT) base with a branch-local delta that pins
            // the first suffix decision to a value the genesis run did not use, so
            // the nested snapshot's prefix coverage is novel and gets admitted.
            Some((snap, base, _)) => {
                let seed = decode(base).map(|d| d.seed).unwrap_or(0);
                let mut overrides = BTreeMap::new();
                overrides.insert(0u64, 2u8); // local 0 == abs SNAP_AT
                let blob = encode(&ToyEnv {
                    base_offset: SNAP_AT,
                    pos: TOTAL_DECISIONS,
                    seed,
                    overrides,
                });
                (snap, blob)
            }
        }
    }
}

/// A snapshot forked below a non-genesis corpus base must be rebased through that
/// base (genesis-complete) and admitted — pins the `base_snap == self.genesis`
/// decision (with `!=`, the nested snapshot is dropped, so the corpus never grows
/// from the exploit step).
#[test]
fn nested_snapshot_below_a_non_genesis_base_is_admitted_genesis_complete() {
    let mut ex = Explorer::new(ToyMachine::new(), ForceExploit, Box::new(ToyCodec)).unwrap();

    // Step 1: a genesis run admits the first-generation bases (at SNAP_AT and the
    // deeper point).
    ex.multiverse_step().unwrap();
    let after_seed = ex.corpus().len();
    assert!(after_seed >= 1, "the genesis run seeded the corpus");

    // Step 2: exploit the SNAP_AT base; the run forks a nested snapshot below it,
    // which must be admitted (rebased to genesis-complete), growing the corpus.
    ex.multiverse_step().unwrap();
    assert!(
        ex.corpus().len() > after_seed,
        "the nested snapshot was admitted, not dropped"
    );

    // And every admitted entry — the nested one included — is genesis-complete.
    for i in 0..ex.corpus().len() {
        let (_, env, _) = ex.corpus().entry(i).unwrap();
        assert_eq!(
            decode(env).unwrap().base_offset,
            0,
            "corpus entry {i} is genesis-complete"
        );
    }
}
