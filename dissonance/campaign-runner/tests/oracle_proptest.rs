// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Crash-oracle mapping proptest (task 60, acceptance gate 2).** The
//! workload-aware [`CrashOracle`] mapping — "any crash or assertion is the
//! bug; the clean `Quiescent` halt and every non-terminal stop are not" — proved
//! total and consistent over ≥256 random stops. (On this campaign workload
//! `/init` reboots on the bug → `Crash{Shutdown}` and halts on a clean run →
//! `Quiescent`, so the terminal class is the whole signal — see the campaign
//! module doc.)

use campaign_runner::campaign::CrashOracle;
use explorer::{Moment, Oracle, Reproducer, RunTrace, StopReason, TerminalOracle};
use proptest::prelude::*;

/// A trace with the given terminal and an arbitrary (oracle-irrelevant) env.
fn trace(terminal: StopReason, env_bytes: Vec<u8>) -> RunTrace {
    RunTrace {
        terminal,
        env: Reproducer {
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

    /// A crash is ALWAYS the bug — whatever its kind byte or detail (the bug
    /// reboots to `Crash{Shutdown}` here, but the oracle keys on the class, not
    /// the kind).
    #[test]
    fn a_crash_is_always_a_bug(
        info in prop::collection::vec(any::<u8>(), 0..8),
        vt in any::<u64>(),
        env in prop::collection::vec(any::<u8>(), 0..8),
    ) {
        let o = CrashOracle::new();
        let t = trace(StopReason::Crash { vtime: Moment(vt), info }, env);
        prop_assert!(o.judge(&t).is_some());
    }

    /// An SDK assertion is always the bug, whatever its id/data.
    #[test]
    fn an_assertion_is_always_a_bug(id in any::<u32>(), data in prop::collection::vec(any::<u8>(), 0..8), vt in any::<u64>()) {
        let o = CrashOracle::new();
        let t = trace(StopReason::Assertion { vtime: Moment(vt), id, data }, vec![]);
        prop_assert!(o.judge(&t).is_some());
    }

    /// The clean `Quiescent` halt and every other non-bug stop are never the bug.
    #[test]
    fn clean_and_nonterminal_stops_are_never_bugs(vt in any::<u64>(), id in any::<u64>(), ctx in prop::collection::vec(any::<u8>(), 0..4)) {
        let o = CrashOracle::new();
        for stop in [
            StopReason::Quiescent { vtime: Moment(vt) },
            StopReason::Deadline { vtime: Moment(vt) },
            StopReason::SnapshotPoint { vtime: Moment(vt) },
            StopReason::Decision { vtime: Moment(vt), id, ctx: ctx.clone() },
        ] {
            prop_assert!(o.judge(&trace(stop, vec![])).is_none());
        }
    }

    /// When the oracle calls a bug, its fingerprint is the explorer's canonical
    /// one (a function of the stop, not the env) — so a campaign bug dedups
    /// identically to any other, across the many envs that reach it.
    #[test]
    fn a_reported_bug_has_the_canonical_fingerprint(
        info in prop::collection::vec(any::<u8>(), 0..6),
        vt in any::<u64>(),
        e1 in prop::collection::vec(any::<u8>(), 0..6),
        e2 in prop::collection::vec(any::<u8>(), 0..6),
    ) {
        let o = CrashOracle::new();
        let stop = StopReason::Crash { vtime: Moment(vt), info };
        let t1 = trace(stop.clone(), e1);
        let t2 = trace(stop.clone(), e2);
        let b1 = o.judge(&t1).expect("a crash is a bug");
        let b2 = o.judge(&t2).expect("a crash is a bug");
        // Same stop ⇒ same fingerprint, whatever the env.
        prop_assert_eq!(b1.fingerprint, b2.fingerprint);
        // And it matches the explorer's canonical terminal oracle exactly.
        let canonical = TerminalOracle::new().judge(&t1).expect("terminal oracle");
        prop_assert_eq!(b1.fingerprint, canonical.fingerprint);
    }
}

/// An empty-info crash (no kind byte) is still the bug — the oracle keys on the
/// terminal class, not the info bytes.
#[test]
fn an_empty_info_crash_is_a_bug() {
    let o = CrashOracle::new();
    let t = trace(
        StopReason::Crash {
            vtime: Moment(1),
            info: vec![],
        },
        vec![],
    );
    assert!(o.judge(&t).is_some());
}
