// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Portable determinism proptest (task 58, acceptance gate 1).** Over ≥256
//! cases: an arbitrary set of seeds and repeat count, driven through the full
//! socket loopback (adapter ⇄ server, mock guest), satisfies the two headline
//! properties —
//!
//! 1. `branch(s, seed) → run → hash` is **bit-identical** across repeated runs
//!    of the same seed (per-seed reproducibility), and
//! 2. `replay(base)` after the whole interleaved sweep reproduces the
//!    **pre-snapshot** capture hash —
//!
//! and, whenever the case has ≥ 2 distinct seeds, the futures **diverge** (≥ 2
//! distinct terminal hashes). Same properties as the box gate, proven portably
//! against the mock guest.

use conductor::mock::{self, default_fork_script};
use conductor::{SweepConfig, run_session, sweep_client, verify};
use environment::{EnvSpec, FaultPolicy};
use proptest::prelude::*;

fn config(cases: u32) -> ProptestConfig {
    // No `unsafe` in this crate, but keep the Miri cut for portability with the
    // rest of the suite; the socket loopback is not Miri-executable anyway.
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 4 } else { cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

proptest! {
    #![proptest_config(config(256))]

    /// The core gate-1 property over arbitrary seed sets.
    #[test]
    fn branch_run_hash_is_deterministic_and_replay_reproduces_capture(
        seeds in prop::collection::vec(any::<u64>(), 1..6),
        runs in 2usize..4,
    ) {
        let mut server = mock::server(default_fork_script()).expect("mock server");
        let cfg = SweepConfig {
            seeds: seeds.clone(),
            runs_per_seed: runs,
            ..SweepConfig::default()
        };
        let boot_env = EnvSpec::Seeded {
            seed: mock::BOOT_SEED,
            policy: FaultPolicy::none(),
        };
        let (served, report) = run_session(&mut server, move |stream| {
            sweep_client(stream, boot_env, cfg).expect("sweep")
        });
        prop_assert!(served.is_ok(), "server session ends cleanly");

        // Reproducibility (per seed) + replay == capture always hold. Distinct
        // futures are required only when the case actually has distinct seeds.
        let distinct_seeds = {
            let mut s = seeds.clone();
            s.sort_unstable();
            s.dedup();
            s.len()
        };
        let min_distinct = distinct_seeds.clamp(1, 2);
        let failures = verify(&report, min_distinct);
        prop_assert!(
            failures.is_empty(),
            "gate-1 properties failed: {failures:?}"
        );

        // A second, independent session with the same seeds yields the SAME
        // per-seed terminal hashes — the whole loop is a pure function of the
        // (script, seed) inputs, not of session/timing.
        let mut server2 = mock::server(default_fork_script()).expect("mock server 2");
        let cfg2 = SweepConfig {
            seeds: seeds.clone(),
            runs_per_seed: runs,
            ..SweepConfig::default()
        };
        let boot_env2 = EnvSpec::Seeded {
            seed: mock::BOOT_SEED,
            policy: FaultPolicy::none(),
        };
        let (served2, report2) = run_session(&mut server2, move |stream| {
            sweep_client(stream, boot_env2, cfg2).expect("sweep 2")
        });
        prop_assert!(served2.is_ok());
        prop_assert_eq!(report.base_hash, report2.base_hash, "capture is session-independent");
        for (r1, r2) in report.rows.iter().zip(report2.rows.iter()) {
            prop_assert_eq!(r1.runs[0].hash, r2.runs[0].hash, "per-seed hash is session-independent");
        }
    }
}
