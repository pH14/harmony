// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use searcher::search::{archive::ArchiveKey, rand::RomuDuoJrRand};

use serde::{Deserialize, Serialize};

use super::*;
use crate::assertion::{AssertionKind, AssertionOutcome};

const TABLE_SEED: u64 = 0x5eed_fa17;
const TABLE_STATES: usize = 64;
const TABLE_NODES: [u16; 4] = [1, 3, 8, 10];
const TABLE: &str = include_str!("strategy-keys.json");

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
                holds: (kind == AssertionKind::Sometimes).then_some(passed),
                failure_details: None,
            },
        );
    }
    observations
}

fn sample(seed: u64, nodes: u16, states: usize) -> Vec<FaultObservations> {
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let edges: Vec<u64> = (0..64).map(|_| rand.next_u64()).collect();
    (0..states)
        .map(|_| observation(&mut rand, nodes, &edges))
        .collect()
}

#[test]
fn a_bundle_past_the_strategy_widths_fails_to_compile() {
    assert!(FaultKeys::new(STRATEGY, 10).is_ok());
    let error = FaultKeys::new(STRATEGY, 11).unwrap_err().to_string();
    assert!(error.contains("at most 10 nodes, not 11"), "{error}");
    assert!(error.contains("key field holds"), "{error}");
    let error = FaultKeys::new(STRATEGY, 30).unwrap_err().to_string();
    assert!(error.contains("alive and event.ready"), "{error}");
    assert!(error.contains("at most 29 nodes"), "{error}");
}

#[test]
fn the_node_limit_follows_the_mask_lanes_the_strategy_reads() {
    let running =
        "\n[define]\nhooks_running = \"min(max(hooks.started - hooks.finished, 0), 8)\"\n";
    let unmasked = format!(
        "capacity = 1\nspread = [\"min(hooks.finished, 2)\", \"hooks_running\"]\n{running}"
    );
    for nodes in [30, crate::bundle::MAX_NODES] {
        let keys = FaultKeys::new(&unmasked, nodes).unwrap();
        let observations = FaultObservations {
            alive: u64::MAX,
            event_ready: u64::MAX,
            hooks_started: 1,
            ..FaultObservations::default()
        };
        let context = keys.context(keys.key(&observations).unwrap());
        assert_eq!(context.place.get(), 1 << 4);
        assert_eq!(keys.context_event_ready(context), 0);
    }
    let ready = format!("capacity = 1\nspread = [\"event.ready\", \"hooks_running\"]\n{running}");
    let keys = FaultKeys::new(&ready, 59).unwrap();
    let observations = FaultObservations {
        event_ready: u64::MAX,
        hooks_started: 1,
        ..FaultObservations::default()
    };
    let context = keys.context(keys.key(&observations).unwrap());
    assert_eq!(keys.context_event_ready(context), (1 << 59) - 1);
    let error = FaultKeys::new(&ready, 60).unwrap_err().to_string();
    assert!(error.contains("lane event.ready and"), "{error}");
    assert!(error.contains("at most 59 nodes"), "{error}");
}

#[test]
fn a_mask_lane_past_its_declared_width_is_refused() {
    let alive = "capacity = 1\nspread = [\"alive\"]";
    let error = FaultKeys::new(alive, 64).unwrap_err().to_string();
    println!("64 nodes: {error}");
    assert!(error.contains("node mask lane alive"), "{error}");
    assert!(error.contains("at most 62 nodes"), "{error}");
    assert!(error.contains("cannot key 64 nodes"), "{error}");
    let keys = FaultKeys::new(alive, 62).unwrap();
    let place = |alive| {
        keys.key(&FaultObservations {
            alive,
            ..FaultObservations::default()
        })
        .unwrap()
        .place()
    };
    println!(
        "62 nodes: compiled, {}",
        keys.compiled().policy_identifier()
    );
    assert_ne!(place(1), place(1 | 1 << 61));
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
    let context = keys.context(keys.key(&FaultObservations::default()).unwrap());
    assert_eq!(keys.context_event_ready(context), 0);
}

#[test]
fn stages_latches_and_aggregates_compile_and_stages_keep_the_shared_context_lines() {
    for strategy in [
        "tier = \"min(highest(ticks), 15)\"\n",
        "[[stage]]\ngoal = \"sometimes.count > 2\"\nlatch = true\n[[stage]]\ngoal = \"false\"\n",
        "[every_stage]\nspread = [\"alive\"]\n[[stage]]\ngoal = \"min(highest(hooks.finished), 8) > 2\"\n[[stage]]\ngoal = \"false\"\n",
        "reject = \"always.violated > 0\"\n",
        "[[stage]]\ngoal = \"false\"\nreject = \"always.violated > 0\"\n",
    ] {
        assert!(FaultKeys::new(strategy, 3).is_ok(), "{strategy}");
    }
    let shared = FaultKeys::new(
        "[every_stage]\nidentity = [\"alive\"]\n[[stage]]\ngoal = \"false\"\n",
        3,
    )
    .unwrap();
    let context = shared.context(
        shared
            .key(&FaultObservations {
                alive: 0b101,
                ..FaultObservations::default()
            })
            .unwrap(),
    );
    assert_eq!((context.place.get() >> 4) & 0b111, 0b101);
    for (strategy, refusal) in [
        (
            "[[stage]]\nname = \"early\"\ngoal = \"false\"\nspread = [\"alive\"]\n",
            "stage `early` gives its own `spread` or `identity`",
        ),
        (
            "[[stage]]\ngoal = \"false\"\nidentity = [\"alive\"]\n",
            "stage `false` gives its own",
        ),
    ] {
        let error = FaultKeys::new(strategy, 3).unwrap_err().to_string();
        assert!(error.contains(refusal), "{strategy}: {error}");
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
    let mut declared = Vec::new();
    declare(&observations, 3, &[], &mut declared);
    keys.catalog()
        .checked_record(declared)
        .expect("every value lies in its declared range");
    assert_eq!(keys.catalog().signals().len(), DECLARATIONS);
}

fn request(strategy: Option<&str>, facts: &[(&str, &[&str])]) -> KeyRequest {
    KeyRequest {
        strategy: strategy.map(str::to_owned),
        facts: facts
            .iter()
            .map(|(name, terms)| {
                (
                    (*name).to_owned(),
                    terms.iter().map(|term| (*term).to_owned()).collect(),
                )
            })
            .collect(),
    }
}

const STUCK_TRANSFER: (&str, &[&str]) =
    ("stuck_transfer", &["transfer_timeout", "second_transfer"]);

fn fact_record(observations: &mut FaultObservations, id: &str, holds: bool) {
    observations.assertions.record(
        id.to_owned(),
        AssertionOutcome {
            kind: AssertionKind::Sometimes,
            message: id.to_owned(),
            location: String::new(),
            passed: holds,
            failed: !holds,
            holds: Some(holds),
            failure_details: None,
        },
    );
}

fn json_record(
    observations: &mut FaultObservations,
    assert_type: &str,
    must_hit: bool,
    id: &str,
    condition: bool,
) {
    let event = format!(
        r#"{{"antithesis_assert":{{"id":"{id}","assert_type":"{assert_type}","must_hit":{must_hit},"hit":true,"condition":{condition}}}}}"#
    );
    let (id, outcome) = crate::assertion::decode_json_event(event.as_bytes())
        .and_then(|event| event.assertion)
        .unwrap();
    observations.assertions.record(id, outcome);
}

#[test]
fn the_key_table_names_facts_that_declare_one_sometimes_all_each() {
    let parsed: KeyRequest = serde_json::from_str(
        r#"{"facts": {"stuck_transfer": ["transfer_timeout", "second_transfer"], "paused_leader": ["paused_leader_behind"]}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed,
        request(
            None,
            &[STUCK_TRANSFER, ("paused_leader", &["paused_leader_behind"])]
        )
    );
    let keys = FaultKeys::requested(&parsed, 3).unwrap();
    let declarations = keys.catalog().declarations();
    assert_eq!(declarations.len(), DECLARATIONS + 2);
    let facts: Vec<(&str, &str)> = declarations[DECLARATIONS..]
        .iter()
        .map(|declared| (declared.name.as_str(), declared.form.as_str()))
        .collect();
    assert_eq!(
        facts,
        [
            ("paused_leader", "sometimes_all"),
            ("stuck_transfer", "sometimes_all")
        ]
    );
    let signals: Vec<&str> = keys.catalog().signals()[DECLARATIONS..]
        .iter()
        .map(|signal| signal.name.as_str())
        .collect();
    assert_eq!(
        signals,
        [
            "paused_leader",
            "paused_leader.paused_leader_behind",
            "stuck_transfer",
            "stuck_transfer.transfer_timeout",
            "stuck_transfer.second_transfer",
        ]
    );
    assert_ne!(
        keys.compiled().policy_identifier(),
        shipped().compiled().policy_identifier()
    );
    let mut observations = FaultObservations::default();
    fact_record(&mut observations, "second_transfer", true);
    let record = keys.record(&observations);
    let slot = |name| record.values()[keys.catalog().slot(name).unwrap()];
    assert_eq!(slot("stuck_transfer"), 1);
    assert_eq!(slot("stuck_transfer.transfer_timeout"), 0);
    assert_eq!(slot("stuck_transfer.second_transfer"), 1);
    assert_eq!(slot("paused_leader"), 0);
}

#[test]
fn a_key_table_without_facts_keeps_the_shipped_catalog_and_policy() {
    let absent: KeyRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(absent, KeyRequest::default());
    assert_eq!(serde_json::to_string(&absent).unwrap(), "{}");
    let keys = FaultKeys::requested(&absent, 3).unwrap();
    assert_eq!(keys.catalog(), shipped().catalog());
    assert_eq!(
        keys.compiled().policy_identifier(),
        shipped().compiled().policy_identifier()
    );
    assert_eq!(
        keys.compiled().policy_identifier(),
        SHIPPED_POLICY_AT_THREE_NODES
    );
}

const SHIPPED_POLICY_AT_THREE_NODES: &str = "assertion_key_1e87540489cc2523";

#[test]
fn malformed_facts_are_refused() {
    let many: Vec<String> = (0..64).map(|term| format!("t{term}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let too_many = [("a", &many[..]), ("b", &many[..12])];
    let wide: Vec<String> = (0..65).map(|term| format!("t{term}")).collect();
    let wide: Vec<&str> = wide.iter().map(String::as_str).collect();
    let fits = [("a", &many[..]), ("b", &many[..11])];
    assert!(FaultKeys::requested(&request(None, &fits), 3).is_ok());
    for (facts, refusal) in [
        (&[("stuck-transfer", &["a"][..])][..], "is not a name"),
        (&[("5stuck", &["a"][..])][..], "is not a name"),
        (&[("stuck.transfer", &["a"][..])][..], "is not a name"),
        (&[("alive", &["a"][..])][..], "built-in"),
        (&[("sometimes", &["a"][..])][..], "built-in"),
        (&[("stuck_transfer", &[][..])][..], "not 1 to 64"),
        (&[("stuck_transfer", &wide[..])][..], "not 1 to 64"),
        (&[("stuck_transfer", &["a", "a"][..])][..], "twice"),
        (&[("stuck_transfer", &["it's"][..])][..], "cannot quote"),
        (&[("stuck_transfer", &[""][..])][..], "cannot quote"),
        (&too_many[..], "a record holds 77"),
    ] {
        let error = FaultKeys::requested(&request(None, facts), 3)
            .unwrap_err()
            .to_string();
        assert!(error.contains(refusal), "{facts:?}: {error}");
    }
    let unknown = serde_json::from_str::<KeyRequest>(r#"{"fact": 1}"#).unwrap_err();
    assert!(unknown.to_string().contains("unknown field"), "{unknown}");
}

#[test]
fn a_fact_term_follows_the_latest_sometimes_hit_whatever_kinds_share_its_id() {
    let keys = FaultKeys::requested(&request(None, &[STUCK_TRANSFER]), 3).unwrap();
    let term = |records: &[(&str, bool, bool)]| {
        let mut observations = FaultObservations::default();
        for &(assert_type, must_hit, condition) in records {
            json_record(
                &mut observations,
                assert_type,
                must_hit,
                "transfer_timeout",
                condition,
            );
        }
        keys.record(&observations).values()[keys
            .catalog()
            .slot("stuck_transfer.transfer_timeout")
            .unwrap()]
    };
    for other in [("always", true), ("always", false), ("reachability", true)] {
        assert_eq!(term(&[(other.0, other.1, true)]), 0, "{other:?}");
    }
    assert_eq!(term(&[("sometimes", true, true)]), 1);
    assert_eq!(
        term(&[("sometimes", true, false), ("always", true, true)]),
        0
    );
    assert_eq!(
        term(&[("always", true, true), ("sometimes", true, true)]),
        1
    );
    assert_eq!(
        term(&[("sometimes", true, true), ("always", true, false)]),
        1
    );
    assert_eq!(
        term(&[
            ("always", true, false),
            ("sometimes", true, true),
            ("sometimes", true, false)
        ]),
        0
    );
}

#[test]
fn a_tier_reading_a_fact_keys_a_state_whose_fact_cleared_lower() {
    let strategy = STRATEGY.replace(
        "tier = \"sometimes.count\"",
        "tier = [\"stuck_transfer\", \"sometimes.count\"]",
    );
    let keys = FaultKeys::requested(&request(Some(&strategy), &[STUCK_TRANSFER]), 3).unwrap();
    let mut holds = FaultObservations::default();
    fact_record(&mut holds, "transfer_timeout", true);
    fact_record(&mut holds, "second_transfer", true);
    let mut cleared = holds.clone();
    fact_record(&mut cleared, "transfer_timeout", false);
    assert_eq!(holds.assertions.key(), cleared.assertions.key());
    let (holds, cleared) = (key(&keys, &holds), key(&keys, &cleared));
    assert!(cleared.progress < holds.progress, "{cleared:?} {holds:?}");
    assert_eq!(keys.view(holds).progress, [2, 2]);
    assert_eq!(keys.view(cleared).progress, [1, 2]);
}

fn endpoint(sometimes: &[u32], hooks_finished: u64, alive: u64) -> FaultObservations {
    let mut observations = FaultObservations {
        hooks_finished,
        alive,
        ..FaultObservations::default()
    };
    for id in sometimes {
        observations.assertions.record(
            id.to_string(),
            AssertionOutcome {
                kind: AssertionKind::Sometimes,
                message: id.to_string(),
                location: String::new(),
                passed: true,
                failed: false,
                holds: Some(true),
                failure_details: None,
            },
        );
    }
    observations
}

fn shipped() -> FaultKeys {
    FaultKeys::new(STRATEGY, 3).expect("the shipped strategy compiles")
}

fn key(keys: &FaultKeys, observations: &FaultObservations) -> AssertionKey {
    keys.key(observations)
        .expect("the shipped strategy keys every state")
}

#[test]
fn the_shipped_strategy_policy_names_its_node_count() {
    let policy = shipped().compiled().policy_identifier().to_owned();
    assert!(policy.starts_with("assertion_key_"), "{policy}");
    assert_ne!(
        policy,
        FaultKeys::new(STRATEGY, 4)
            .unwrap()
            .compiled()
            .policy_identifier()
    );
    assert_eq!(shipped().compiled().shape().capacity, 1);
}

#[test]
fn raw_event_sites_remain_diagnostic_only() {
    let keys = shipped();
    let first = FaultObservations {
        event_kill_site: 41,
        event_kill_fires: 1,
        ..FaultObservations::default()
    };
    let second = FaultObservations {
        event_kill_site: 999,
        ..first.clone()
    };
    assert_eq!(key(&keys, &first), key(&keys, &second));
}

#[test]
fn hooks_still_running_open_their_own_cell() {
    let keys = shipped();
    let overlapping = FaultObservations {
        hooks_started: 2,
        hooks_finished: 1,
        alive: 0b1,
        ..FaultObservations::default()
    };
    let alone = FaultObservations {
        hooks_started: 1,
        ..overlapping.clone()
    };
    assert_ne!(key(&keys, &overlapping).place(), key(&keys, &alone).place());
    let running = |hooks_started| {
        key(
            &keys,
            &FaultObservations {
                hooks_started,
                ..FaultObservations::default()
            },
        )
    };
    assert_eq!(running(8), running(100));
    assert_ne!(running(7), running(8));
}

#[test]
fn repeated_hooks_stop_opening_cells_past_the_cap() {
    let keys = shipped();
    let finished = |hooks_finished| key(&keys, &endpoint(&[1], hooks_finished, 0b1));
    assert_eq!(finished(8), finished(9));
    assert_eq!(finished(8), finished(100));
    assert_ne!(finished(7), finished(8));
    assert_eq!(
        crate::archive::milestones(&endpoint(&[1], 100, 0b1)).hooks_finished,
        100,
        "the milestone keeps the true count"
    );
}

#[test]
fn an_in_flight_check_opens_its_own_cell() {
    let keys = shipped();
    let completed = FaultObservations {
        checks_started: 1,
        checks_finished: 1,
        ..FaultObservations::default()
    };
    let running = FaultObservations {
        checks_started: 2,
        ..completed.clone()
    };
    assert_ne!(key(&keys, &completed).place(), key(&keys, &running).place());
}

#[test]
fn a_held_park_thread_and_an_armed_event_kill_each_open_their_own_place() {
    let keys = shipped();
    let quiet = FaultObservations {
        alive: 0b11,
        ..FaultObservations::default()
    };
    let held = FaultObservations {
        event_park_held: 0b10,
        ..quiet.clone()
    };
    let armed = FaultObservations {
        event_kill_armed: 0b01,
        ..quiet.clone()
    };
    let places = [&quiet, &held, &armed]
        .map(|observations| key(&keys, observations).place())
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(places.len(), 3);
    let other_node = FaultObservations {
        event_park_held: 0b01,
        ..quiet
    };
    assert_eq!(key(&keys, &other_node), key(&keys, &held));
}

#[test]
fn liveness_is_the_holder_identity_inside_one_place() {
    let keys = shipped();
    let survived = key(&keys, &endpoint(&[1], 2, 0b11));
    let lost_a_node = key(&keys, &endpoint(&[1], 2, 0b01));
    assert_ne!(survived.identity(), lost_a_node.identity());
    assert_eq!(survived.place(), lost_a_node.place());
}

fn edges(keys: &FaultKeys, edge_digest: u64) -> AssertionKey {
    key(
        keys,
        &FaultObservations {
            edge_crossings: 3,
            edge_digest,
            ..endpoint(&[1], 2, 0b11)
        },
    )
}

#[test]
fn edge_bucket_coverage_separates_identities_inside_one_place() {
    let keys = shipped();
    let before = edges(&keys, 0);
    for kept in [1 << 16, 0x5eed << 16, 1 << 63] {
        let crossed = edges(&keys, kept);
        assert_eq!(before.place(), crossed.place());
        assert_ne!(before.identity(), crossed.identity(), "{kept:#x}");
    }
}

#[test]
fn edge_digests_differing_only_below_the_kept_bits_share_an_identity() {
    let keys = shipped();
    let kept = 0x5eed_u64 << 40;
    for dropped in [1, 0x5eed, 0xffff] {
        assert_eq!(
            edges(&keys, kept),
            edges(&keys, kept | dropped),
            "{dropped:#x}"
        );
    }
}

#[test]
fn hook_progress_separates_places_inside_one_tier() {
    let keys = shipped();
    let early = key(&keys, &endpoint(&[1], 1, 0b1));
    let late = key(&keys, &endpoint(&[1], 4, 0b1));
    assert_eq!(early.progress(), late.progress());
    assert_ne!(early.place(), late.place());
}

#[test]
fn the_goals_reached_are_the_progress_tier() {
    let keys = shipped();
    let none = key(&keys, &endpoint(&[], 0, 0b1));
    let one = key(&keys, &endpoint(&[4], 0, 0b1));
    let other_one = key(&keys, &endpoint(&[7], 0, 0b1));
    let two = key(&keys, &endpoint(&[4, 7], 0, 0b1));
    assert!(none.progress() < one.progress());
    assert!(one.progress() < two.progress());
    assert_eq!(one.progress(), other_one.progress());
    assert_ne!(one.place(), other_one.place());
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TableRow {
    nodes: u16,
    index: usize,
    key: AssertionKey,
    context: AssertionKey,
    event_ready: u64,
}

fn table_rows(nodes: u16) -> Vec<TableRow> {
    let keys = FaultKeys::new(STRATEGY, nodes).unwrap();
    sample(TABLE_SEED, nodes, TABLE_STATES)
        .iter()
        .enumerate()
        .map(|(index, observations)| {
            let key = keys
                .key(observations)
                .expect("the shipped strategy keys every state");
            let context = keys.context(key);
            TableRow {
                nodes,
                index,
                key,
                context,
                event_ready: keys.context_event_ready(context),
            }
        })
        .collect()
}

#[test]
fn the_shipped_strategy_keys_the_table_states_as_recorded() {
    let recorded: Vec<TableRow> = serde_json::from_str(TABLE).unwrap();
    assert_eq!(recorded.len(), TABLE_NODES.len() * TABLE_STATES);
    let computed: Vec<TableRow> = TABLE_NODES.into_iter().flat_map(table_rows).collect();
    assert_eq!(computed, recorded);
}
