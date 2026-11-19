// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use searcher::search::{archive::ArchiveKey, rand::RomuDuoJrRand};

use super::*;
use crate::{
    assertion::{AssertionKind, AssertionOutcome},
    shadow::Shadow,
};

const STATES: usize = 20_000;

fn below(rand: &mut RomuDuoJrRand, bound: u64) -> u64 {
    rand.below_u64(NonZeroU64::new(bound).unwrap())
}

fn observation(rand: &mut RomuDuoJrRand, nodes: u16, edges: &[u64]) -> FaultObservations {
    let mask = node_mask(nodes);
    let mut observations = FaultObservations {
        alive: rand.next_u64() & mask,
        event_ready: rand.next_u64() & mask,
        hooks_started: below(rand, 14),
        hooks_finished: below(rand, 12),
        checks_started: below(rand, 12),
        checks_finished: below(rand, 12),
        workload_started: below(rand, 3),
        workload_finished: below(rand, 3),
        event_kill_fires: below(rand, 12),
        event_park_fires: below(rand, 12),
        event_park_held: below(rand, 3),
        event_kill_armed: below(rand, 2) * rand.next_u64(),
        edge_digest: edges[usize::try_from(below(rand, edges.len() as u64)).unwrap()],
        ticks: rand.next_u64(),
        ..FaultObservations::default()
    };
    for point in 0..6 {
        if below(rand, 2) == 0 {
            continue;
        }
        let kind = if point == 5 {
            AssertionKind::Always
        } else {
            AssertionKind::Sometimes
        };
        let passed = below(rand, 3) != 0;
        observations.assertions.record(
            format!("point {point}"),
            AssertionOutcome {
                kind,
                message: String::new(),
                location: String::new(),
                passed,
                failed: !passed,
            },
        );
    }
    observations
}

fn sample(seed: u64, nodes: u16) -> Vec<FaultObservations> {
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let edges: Vec<u64> = (0..64).map(|_| rand.next_u64()).collect();
    (0..STATES)
        .map(|_| observation(&mut rand, nodes, &edges))
        .collect()
}

#[test]
fn the_strategy_key_agrees_with_the_hand_key_on_random_states() {
    for nodes in [1, 3, 8] {
        let keys = FaultKeys::new(STRATEGY, nodes).expect("the shipped strategy compiles");
        let shadow = Shadow::default();
        for observations in sample(u64::from(nodes) * 7919, nodes) {
            shadow.observe(&keys, archive_key(&observations), &observations);
        }
        let report = shadow.report(&keys).unwrap();
        assert_eq!(report.states, STATES as u64);
        assert!(report.hand_cells > 1_000, "{report:?}");
        assert!(report.hand_slots > report.hand_cells, "{report:?}");
        assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
        assert_eq!(report.hand_cells, report.strategy_cells);
        assert_eq!(report.hand_slots, report.strategy_slots);
    }
}

#[test]
fn the_duration_context_partitions_and_orders_parents_as_the_hand_context_does() {
    for nodes in [1, 3, 8] {
        let keys = FaultKeys::new(STRATEGY, nodes).unwrap();
        let mut pairs = BTreeSet::new();
        for observations in sample(u64::from(nodes) * 104_729, nodes) {
            let hand = FaultArchiveKey::duration_context(&keys, archive_key(&observations));
            let strategy = AssertionKey::duration_context(&keys, keys.key(&observations).unwrap());
            assert_eq!(
                FaultArchiveKey::event_ready(&keys, hand),
                AssertionKey::event_ready(&keys, strategy)
            );
            pairs.insert((hand, strategy));
        }
        let mut forward: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        let mut backward: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for (hand, strategy) in &pairs {
            forward.entry(*hand).or_default().insert(*strategy);
            backward.entry(*strategy).or_default().insert(*hand);
        }
        assert!(forward.values().all(|strategies| strategies.len() == 1));
        assert!(backward.values().all(|hands| hands.len() == 1));
        let ordered: Vec<_> = pairs.iter().map(|(_, strategy)| *strategy).collect();
        assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(pairs.len() > 100);
    }
}

#[test]
fn the_shipped_strategy_has_the_hand_key_shape() {
    let keys = FaultKeys::new(STRATEGY, 3).unwrap();
    assert_eq!(AssertionKey::shape(&keys), FaultArchiveKey::shape(&keys));
    assert!(AssertionKey::policy(&keys).starts_with("assertion_key_"));
    assert_ne!(
        AssertionKey::policy(&keys),
        AssertionKey::policy(&FaultKeys::new(STRATEGY, 4).unwrap())
    );
}

#[test]
fn a_bundle_past_the_strategy_widths_fails_to_compile() {
    let error = FaultKeys::new(STRATEGY, 9).unwrap_err().to_string();
    assert!(error.contains("key field holds"), "{error}");
    assert!(FaultKeys::new(STRATEGY, 30).is_err());
}

#[test]
fn an_edited_strategy_compiles_without_a_rebuild_and_keys_differently() {
    let coarse =
        "capacity = 1\ntier = \"sometimes.count\"\nspread = [\"min(hooks.finished, 2)\"]\n";
    let keys = FaultKeys::new(coarse, 3).unwrap();
    let mut rand = RomuDuoJrRand::with_seed(5);
    let edges = [1, 2, 3];
    let cells: BTreeSet<_> = (0..1_000)
        .map(|_| {
            let key = keys.key(&observation(&mut rand, 3, &edges)).unwrap();
            (key.progress(), key.place())
        })
        .collect();
    assert!(cells.len() <= 6 * 3);
    let context =
        AssertionKey::duration_context(&keys, keys.key(&FaultObservations::default()).unwrap());
    assert_eq!(AssertionKey::event_ready(&keys, context), 0);
}

#[test]
fn a_strategy_with_aggregates_stages_or_rejects_is_refused() {
    for strategy in [
        "tier = \"highest(ticks)\"\n",
        "[[stage]]\ngoal = \"sometimes.count > 2\"\n",
        "reject = \"always.violated > 0\"\n",
    ] {
        assert!(FaultKeys::new(strategy, 3).is_err(), "{strategy}");
    }
}

#[test]
fn the_declarations_saturate_into_their_ranges() {
    let observations = FaultObservations {
        alive: u64::MAX,
        hooks_finished: u64::MAX,
        edge_digest: u64::MAX,
        ..FaultObservations::default()
    };
    let keys = FaultKeys::new(STRATEGY, 3).unwrap();
    keys.catalog()
        .checked_record(declare(&observations, 3))
        .expect("every value lies in its declared range");
    assert_eq!(keys.catalog().signals().len(), DECLARATIONS);
}

#[test]
fn a_shadow_needs_the_hand_key() {
    let request = |hand, shadow| KeyRequest {
        strategy: None,
        hand,
        shadow,
    };
    assert!(request(true, true).validate().is_ok());
    assert!(request(true, false).validate().is_ok());
    assert!(request(false, false).validate().is_ok());
    assert!(request(false, true).validate().is_err());
}
