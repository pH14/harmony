// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Crash-oracle mapping proptest (task 60, acceptance gate 2).** The
//! workload-aware [`CampaignOracle`] mapping — "a non-benign crash or an SDK
//! assertion is a bug; the benign reboot terminal and every non-terminal stop
//! are not" — proved total and consistent over ≥256 random stops.

use conductor::campaign::{CRASH_KIND_SHUTDOWN, CampaignOracle};
use explorer::{Environment, Oracle, RunTrace, StopReason, TerminalOracle, VTime};
use proptest::prelude::*;

/// A trace with the given terminal and an arbitrary (oracle-irrelevant) env.
fn trace(terminal: StopReason, env_bytes: Vec<u8>) -> RunTrace {
    RunTrace {
        terminal,
        env: Environment {
            blob_version: 1,
            bytes: env_bytes,
        },
        coverage: None,
        events: Vec::new(),
        records: Vec::new(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// A crash is a bug **iff** its leading kind byte is not the benign reboot
    /// terminal — for every kind, detail, and V-time.
    #[test]
    fn a_crash_is_a_bug_iff_not_the_benign_kind(
        kind in any::<u8>(),
        detail in prop::collection::vec(any::<u8>(), 0..8),
        vt in any::<u64>(),
        env in prop::collection::vec(any::<u8>(), 0..8),
    ) {
        let o = CampaignOracle::default();
        let mut info = vec![kind];
        info.extend_from_slice(&detail);
        let t = trace(StopReason::Crash { vtime: VTime(vt), info }, env);
        prop_assert_eq!(o.judge(&t).is_some(), kind != CRASH_KIND_SHUTDOWN);
    }

    /// With a custom benign kind, exactly that kind is benign and every other is
    /// a bug (the mapping is parametric, not hard-coded to Shutdown).
    #[test]
    fn custom_benign_kind_gates_correctly(benign in any::<u8>(), kind in any::<u8>(), vt in any::<u64>()) {
        let o = CampaignOracle::new(benign);
        let t = trace(StopReason::Crash { vtime: VTime(vt), info: vec![kind] }, vec![]);
        prop_assert_eq!(o.judge(&t).is_some(), kind != benign);
    }

    /// An SDK assertion is always a bug, whatever its id/data.
    #[test]
    fn an_assertion_is_always_a_bug(id in any::<u32>(), data in prop::collection::vec(any::<u8>(), 0..8), vt in any::<u64>()) {
        let o = CampaignOracle::default();
        let t = trace(StopReason::Assertion { vtime: VTime(vt), id, data }, vec![]);
        prop_assert!(o.judge(&t).is_some());
    }

    /// A non-terminal / non-bug stop is never a bug.
    #[test]
    fn non_bug_stops_are_never_bugs(vt in any::<u64>(), id in any::<u64>(), ctx in prop::collection::vec(any::<u8>(), 0..4)) {
        let o = CampaignOracle::default();
        for stop in [
            StopReason::Deadline { vtime: VTime(vt) },
            StopReason::Quiescent { vtime: VTime(vt) },
            StopReason::SnapshotPoint { vtime: VTime(vt) },
            StopReason::Decision { vtime: VTime(vt), id, ctx: ctx.clone() },
        ] {
            prop_assert!(o.judge(&trace(stop, vec![])).is_none());
        }
    }

    /// When the oracle calls a bug, its fingerprint is the explorer's canonical
    /// one (a function of the stop, not the env) — so a campaign bug dedups
    /// identically to any other, across the many envs that reach it.
    #[test]
    fn a_reported_bug_has_the_canonical_fingerprint(
        kind in prop::sample::select(vec![0u8, 1u8]), // Panic / TripleFault — both bugs
        vt in any::<u64>(),
        e1 in prop::collection::vec(any::<u8>(), 0..6),
        e2 in prop::collection::vec(any::<u8>(), 0..6),
    ) {
        let o = CampaignOracle::default();
        let stop = StopReason::Crash { vtime: VTime(vt), info: vec![kind] };
        let t1 = trace(stop.clone(), e1);
        let t2 = trace(stop.clone(), e2);
        let b1 = o.judge(&t1).expect("a non-benign crash is a bug");
        let b2 = o.judge(&t2).expect("a non-benign crash is a bug");
        // Same stop ⇒ same fingerprint, whatever the env.
        prop_assert_eq!(b1.fingerprint, b2.fingerprint);
        // And it matches the explorer's canonical terminal oracle exactly.
        let canonical = TerminalOracle::new().judge(&t1).expect("terminal oracle");
        prop_assert_eq!(b1.fingerprint, canonical.fingerprint);
    }
}

/// An empty-info crash (no kind byte) is treated as a bug — a crash we cannot
/// prove benign is not silently dropped. A plain `#[test]` edge case alongside
/// the properties.
#[test]
fn an_empty_info_crash_is_a_bug() {
    let o = CampaignOracle::default();
    let t = trace(
        StopReason::Crash {
            vtime: VTime(1),
            info: vec![],
        },
        vec![],
    );
    assert!(o.judge(&t).is_some());
}
