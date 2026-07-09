// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property gates for the exploration extension (task 86 gate 1): the
//! distinct-cell/depth bookkeeping against naive models, serde round-trips,
//! and the strict signal-beats-random predicate against synthetic
//! distributions of known separation.

use std::collections::BTreeSet;

use benchmark::exploration::{
    DiscoveryEvent, ExplorationConfig, ExplorationLog, ExplorationReport, GameManifest, Verdict,
};
use benchmark::report::MIN_SEEDS;
use proptest::prelude::*;

fn log_from(config: ExplorationConfig, seed: u64, branches: &[(Vec<u64>, u64)]) -> ExplorationLog {
    ExplorationLog {
        workload: "smb".to_string(),
        config,
        seed,
        events: branches
            .iter()
            .enumerate()
            .map(|(i, (touched, depth))| DiscoveryEvent {
                branch: i as u64,
                touched: touched.clone(),
                depth: *depth,
                state_hash: format!("{i:x}"),
            })
            .collect(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(if cfg!(miri) { 8 } else { 256 }))]

    /// `distinct_cells_at` equals the naive BTreeSet model and is monotone in
    /// the budget; `depth_at` equals the naive max and is monotone too.
    #[test]
    fn bookkeeping_matches_the_naive_model(
        branches in prop::collection::vec(
            (prop::collection::vec(0u64..50, 0..6), 0u64..40),
            1..12,
        ),
        budget in 0u64..16,
    ) {
        let log = log_from(ExplorationConfig::Signal, 1, &branches);
        let naive_cells: BTreeSet<u64> = branches
            .iter()
            .take(budget as usize)
            .flat_map(|(t, _)| t.iter().copied())
            .collect();
        let naive_depth = branches
            .iter()
            .take(budget as usize)
            .map(|(_, d)| *d)
            .max()
            .unwrap_or(0);
        prop_assert_eq!(log.distinct_cells_at(budget), naive_cells.len() as u64);
        prop_assert_eq!(log.depth_at(budget), naive_depth);
        // Monotone in budget.
        prop_assert!(log.distinct_cells_at(budget) <= log.distinct_cells_at(budget + 1));
        prop_assert!(log.depth_at(budget) <= log.depth_at(budget + 1));
    }

    /// Logs and manifests round-trip through serde_json unchanged.
    #[test]
    fn logs_and_manifests_round_trip_serde(
        branches in prop::collection::vec(
            (prop::collection::vec(0u64..1000, 0..4), 0u64..32),
            0..8,
        ),
        seed in 0u64..1000,
        budget in 1u64..1000,
    ) {
        let log = log_from(ExplorationConfig::PureRandom, seed, &branches);
        let json = serde_json::to_string(&log).unwrap();
        let back: ExplorationLog = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&back, &log);

        let m = GameManifest::smb(Some("abc123".to_string()), budget);
        let mjson = serde_json::to_string(&m).unwrap();
        let mback: GameManifest = serde_json::from_str(&mjson).unwrap();
        prop_assert_eq!(&mback, &m);
    }

    /// Known separation ⇒ known verdict: when every signal seed's cells AND
    /// depth strictly exceed every baseline seed's (disjoint ranges), the
    /// verdict is Pass; when the two configurations' samples are identical,
    /// it is Fail — the strict predicate can neither miss a clean win nor
    /// award a tie.
    #[test]
    fn disjoint_ranges_pass_and_ties_fail(
        base_cells in 1u64..8,
        gap in 1u64..5,
        depth in 1u64..10,
        jitter in prop::collection::vec(0u64..2, MIN_SEEDS as usize),
    ) {
        let budget = 4u64;
        let manifest = GameManifest::smb(Some("d00d".into()), budget);
        let mk = |config, cells_per: u64, depth: u64, seed: u64| {
            let branches: Vec<(Vec<u64>, u64)> = (0..budget)
                .map(|b| {
                    let t: Vec<u64> = (0..cells_per).map(|i| seed * 100_000 + b * 100 + i).collect();
                    (t, depth)
                })
                .collect();
            log_from(config, seed, &branches)
        };
        // Disjoint: every signal sample strictly above every baseline sample.
        let mut logs: Vec<ExplorationLog> = (0..MIN_SEEDS)
            .map(|s| mk(ExplorationConfig::PureRandom, base_cells + jitter[s as usize], depth, s))
            .collect();
        let signal_cells = base_cells + 2 + gap; // > base_cells + max jitter (1)
        logs.extend(
            (0..MIN_SEEDS).map(|s| mk(ExplorationConfig::Signal, signal_cells, depth + gap, s)),
        );
        let report = ExplorationReport::compute(&manifest, &logs, (1, 1_000_000)).unwrap();
        prop_assert_eq!(report.verdict, Verdict::Pass);

        // Ties: identical samples on both sides can never pass.
        let mut tied: Vec<ExplorationLog> = (0..MIN_SEEDS)
            .map(|s| mk(ExplorationConfig::PureRandom, base_cells, depth, s))
            .collect();
        tied.extend((0..MIN_SEEDS).map(|s| mk(ExplorationConfig::Signal, base_cells, depth, s)));
        let report = ExplorationReport::compute(&manifest, &tied, (1, 1_000_000)).unwrap();
        prop_assert_eq!(report.verdict, Verdict::Fail);
    }
}
