// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — replay: a recorded `NetAnswer` sequence, replayed by an oracle,
//! reproduces a byte-identical delivery schedule.

mod common;

use common::{ScriptedOracle, SeededOracle, config, frame, node_map};
use proptest::prelude::*;
use pv_net::{NetDeliver, Switch, VTime};

/// Run `sends` through a fresh switch with `oracle`, returning the full drain
/// and the final snapshot bytes.
fn run(
    oracle: &mut dyn pv_net::NetOracle,
    sends: &[(u64, u8, u8, u8)],
) -> (Vec<NetDeliver>, Vec<u8>) {
    let mut s = Switch::new(node_map(4), VTime(100));
    let mut now = 0u64;
    for &(delta, src, dst, fill) in sends {
        now = now.saturating_add(delta);
        s.on_tx(VTime(now), frame(src % 4, dst % 4, fill, 6), oracle);
    }
    (s.due(VTime(u64::MAX)), s.save_state())
}

proptest! {
    #![proptest_config(config(256))]

    /// Record a seeded session's answers, then feed them verbatim to a
    /// `ScriptedOracle` over the same frames/clock: the schedule is identical
    /// down to the snapshot bytes.
    #[test]
    fn recorded_answers_reproduce_the_schedule(
        seed in any::<u64>(),
        sends in prop::collection::vec((1u64..3000, any::<u8>(), any::<u8>(), any::<u8>()), 1..40),
    ) {
        let mut recorder = SeededOracle::new(seed);
        let (live_drain, live_snap) = run(&mut recorder, &sends);

        let mut replay = ScriptedOracle::new(recorder.recorded.clone());
        let (replay_drain, replay_snap) = run(&mut replay, &sends);

        prop_assert_eq!(live_drain, replay_drain, "replayed delivery schedule diverged");
        prop_assert_eq!(live_snap, replay_snap, "replayed snapshot diverged");
        // The replay consumed exactly the recorded answers, in order.
        prop_assert_eq!(replay.idx, recorder.recorded.len());
    }
}
