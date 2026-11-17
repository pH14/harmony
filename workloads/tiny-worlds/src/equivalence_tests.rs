// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::worlds::{State, World};
use crate::{Hand, Keep, Keying, Strategic};
use searcher::search::archive::{ArchiveKey, KeyShape};
use searcher::search::assertion::AssertionKey;
use searcher::search::rand::RomuDuoJrRand;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::num::NonZeroUsize;

struct Case {
    name: String,
    world: World,
    broken: bool,
    keep: Keep,
}

fn pattern(rand: &mut RomuDuoJrRand, length: u32) -> u64 {
    loop {
        let value = rand.next_u64() & u64::MAX.checked_shr(64 - 2 * length).unwrap_or(0);
        if value & 3 != 3 {
            return value;
        }
    }
}

fn delayed(placement: &str, ammo: u8, sticky: bool) -> Value {
    json!({"family": "delayed", "parameters": {
        "horizon": 16, "distractions": 32, "mode": "sequence",
        "placement": placement, "sticky_credit": sticky, "ammo": ammo}})
}

fn chain(
    rand: &mut RomuDuoJrRand,
    ranked: bool,
    upgrade: bool,
    placement: &str,
    sticky: bool,
    count: usize,
) -> Value {
    let mut stages = Vec::new();
    for _ in 0..count {
        stages.push(json!({"world": {"family": "resource", "parameters": {
            "initial_charge": 0, "initial_health": 3, "barrier_charge": 12, "route_cost": 1,
            "health_cost": 0, "refill_amount": 1, "max_charge": 20, "corridor_len": 6,
            "refill_health_cost": 0}}, "refill_available": true}));
        stages.push(json!({"world": {"family": "route", "parameters": {
            "length": 16, "pattern": pattern(rand, 16), "attack": 2, "shifted": true,
            "upgrade_required": true, "ranked_upgrade": upgrade}}, "refill_available": true}));
        stages.push(json!({"world": delayed(placement, 0, sticky), "refill_available": true}));
        stages.push(json!({"world": {"family": "trap", "parameters": {
            "length": 12, "pattern": pattern(rand, 12), "trap_len": 1, "rooms": 16}},
            "refill_available": true}));
    }
    json!({"family": "chain", "parameters": {
        "stages": stages, "carry_charge": true, "initial_charge": 0, "ranked": ranked}})
}

fn map_fields() -> Vec<(&'static str, Value)> {
    let shielded = json!({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 24, "shield": 8,
        "shield_odds": 4, "hit_tier": true, "tail_slots": true});
    vec![
        ("map/ranked", json!({"inner": 20})),
        ("shield/tier", shielded.clone()),
        ("shield/hidden", {
            let mut hidden = shielded;
            hidden["hit_tier"] = json!(false);
            hidden
        }),
        (
            "farm/farms",
            json!({"inner": 20, "farms": 4, "farm_cap": 63}),
        ),
        ("whole-map re-walk", json!({"inner": 4, "items": 9})),
        (
            "boss needing far stock",
            json!({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 24}),
        ),
        (
            "boss needing stock and health",
            json!({"inner": 20, "farms": 4, "farm_cap": 63,
            "boss_stock": 6, "boss_hits_back": true}),
        ),
        (
            "boss after a draining approach",
            json!({"inner": 20, "farms": 4, "farm_cap": 14,
            "boss_stock": 8, "boss_hits_back": true, "approach_drain": true}),
        ),
        ("off-path item", json!({"inner": 6, "item_optional": true})),
        (
            "off-path item with farms",
            json!({"inner": 6, "item_optional": true, "farms": 2,
            "farm_cap": 63}),
        ),
        ("locked item", json!({"inner": 4, "locked": true})),
        (
            "locked item with hidden timing",
            json!({"inner": 4, "locked": true, "timing": 5}),
        ),
        (
            "gauntlet",
            json!({"inner": 20, "items": 2, "farms": 2, "farm_cap": 1, "gauntlet": true}),
        ),
        (
            "gauntlet with hidden timing",
            json!({"inner": 20, "items": 2, "farms": 2, "farm_cap": 1,
            "gauntlet": true, "timing": 5}),
        ),
        (
            "boss by the door",
            json!({"inner": 2, "farms": 4, "farm_cap": 14, "boss_stock": 8,
            "boss_hits_back": true, "approach_drain": true, "boss_by_door": true, "tail_slots": true}),
        ),
        (
            "boss beside a late item",
            json!({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 8,
            "boss_hits_back": true, "late_item": true}),
        ),
        (
            "boss past an item at the entry",
            json!({"inner": 20, "farms": 4, "farm_cap": 63,
            "boss_stock": 8, "boss_hits_back": true, "late_item": true, "item_at_entry": true,
            "tail_slots": true}),
        ),
    ]
}

fn arena() -> Value {
    json!({"loops": 0, "corridor": 4, "shaft": 4, "inner": 48, "farms": 8, "farm_cap": 16,
        "boss_stock": 6, "shield": 6, "shield_odds": 4, "tail_slots": true, "arena": 8})
}

fn valid(rand: &mut RomuDuoJrRand, build: &mut dyn FnMut(&mut RomuDuoJrRand) -> Value) -> World {
    loop {
        let world: World = serde_json::from_value(build(rand)).expect("panel world parses");
        if world.validate().is_ok() {
            return world;
        }
    }
}

fn map(rand: &mut RomuDuoJrRand, fields: &Value, grid: bool) -> World {
    valid(rand, &mut |rand| {
        let mut parameters = json!({"width": 8, "height": 8, "layout": rand.next_u64()});
        if grid {
            parameters["loops"] = json!(7);
            parameters["corridor"] = json!(2);
            parameters["shaft"] = json!(3);
        }
        for (key, value) in fields.as_object().expect("fields") {
            parameters[key] = value.clone();
        }
        json!({"family": "map", "parameters": parameters})
    })
}

fn cases(rand: &mut RomuDuoJrRand) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut add = |name: &str, world: World, broken: bool, keep: Keep| {
        cases.push(Case {
            name: name.to_owned(),
            world,
            broken,
            keep,
        });
    };
    for (name, placement, ammo, sticky) in [
        ("boss/engaged/tight", "engaged", 16, false),
        ("boss/tier/tight", "tier", 16, false),
        ("boss/place/tight", "place", 16, false),
        ("boss/engaged/ample", "engaged", 31, false),
        ("boss/tier/ample", "tier", 31, false),
        ("credit/engaged", "engaged", 0, false),
        ("credit/engaged/sticky", "engaged", 0, true),
    ] {
        let world = serde_json::from_value(delayed(placement, ammo, sticky)).expect("delayed");
        add(name, world, false, Keep::Portfolio);
    }
    let trap: World = serde_json::from_value(json!({"family": "trap", "parameters": {
        "length": 12, "pattern": pattern(rand, 12), "trap_len": 2, "rooms": 1}}))
    .expect("trap");
    add("trap/ranked", trap.clone(), false, Keep::Portfolio);
    add("trap/control", trap, true, Keep::Portfolio);
    for (name, ranked, upgrade, placement, sticky) in [
        ("chain/flat", false, false, "place", false),
        ("chain/ranked", true, false, "place", false),
        ("chain/ranked_upgrade", true, true, "engaged", false),
        ("chain/sticky", true, true, "engaged", true),
    ] {
        let world: World =
            serde_json::from_value(chain(rand, ranked, upgrade, placement, sticky, 4))
                .expect("chain");
        add(name, world.clone(), false, Keep::Portfolio);
        add(&format!("{name}/broken"), world, true, Keep::Portfolio);
    }
    let short: World =
        serde_json::from_value(chain(rand, true, true, "engaged", false, 2)).expect("chain");
    add("keep/portfolio", short.clone(), false, Keep::Portfolio);
    add("keep/capacity_two", short, false, Keep::CapacityTwo);
    let line = pattern(rand, 32);
    for (name, placement, broken) in [
        ("backtrack/tier", "tier", false),
        ("backtrack/identity", "identity", false),
        ("backtrack/preference", "preference", false),
        ("backtrack/control", "tier", true),
    ] {
        let world = serde_json::from_value(json!({"family": "backtrack", "parameters": {
            "barriers": 3, "segment": 8, "pattern": line, "placement": placement}}))
        .expect("backtrack");
        add(name, world, broken, Keep::Portfolio);
    }
    for (name, fields) in map_fields() {
        let world = map(rand, &fields, true);
        add(name, world.clone(), false, Keep::Portfolio);
        if name == "map/ranked" {
            add("map/control", world, true, Keep::Portfolio);
        }
    }
    add("arena", map(rand, &arena(), false), false, Keep::Portfolio);
    add(
        "arena/capacity_two",
        map(rand, &arena(), false),
        false,
        Keep::CapacityTwo,
    );
    let fork = valid(rand, &mut |rand| {
        json!({"family": "fork", "parameters": {"length": 14, "fork": 2, "dead_len": 6, "rooms": 4,
            "pattern": pattern(rand, 14)}})
    });
    add("dead-end fork", fork.clone(), false, Keep::Portfolio);
    add("dead-end fork/broken", fork, true, Keep::Portfolio);
    for (name, cells, length, rooted, denominator) in [
        ("flat archive crossing", 1024, 4, false, None),
        ("fresh crossing", 1024, 4, true, None),
        ("rare flat archive crossing", 256, 8, false, Some(1024)),
        ("rare fresh crossing", 256, 8, true, Some(1024)),
    ] {
        let mut parameters = json!({"cells": cells, "length": length, "rooted": rooted,
            "layout": rand.next_u64(), "pattern": pattern(rand, length)});
        if let Some(denominator) = denominator {
            parameters["action_denominator"] = json!(denominator);
        }
        let world: World =
            serde_json::from_value(json!({"family": "crossing", "parameters": parameters}))
                .expect("crossing");
        add(name, world.clone(), false, Keep::Portfolio);
        add(&format!("{name}/broken"), world, true, Keep::Portfolio);
    }
    for (name, visible) in [
        ("passive clock hidden gap", false),
        ("passive clock visible progress", true),
    ] {
        let end = 15 + 960 + rand.below(NonZeroUsize::new(271).unwrap()) as u16;
        let mut events: BTreeSet<u16> = [12, 13, end - 2, end].into();
        if visible {
            events.extend((53..end).step_by(40));
        }
        let world: World =
            serde_json::from_value(json!({"family": "passive_clock", "parameters": {
            "events": events.into_iter().collect::<Vec<_>>(), "holds": [
                {"minimum": 2, "maximum": 7, "weight": 2},
                {"minimum": 2, "maximum": 12, "weight": 11},
                {"minimum": 48, "maximum": 120, "weight": 11}]}}))
            .expect("passive clock");
        add(name, world.clone(), false, Keep::Portfolio);
        add(&format!("{name}/broken"), world, true, Keep::Portfolio);
    }
    cases
}

fn action(world: &World, broken: bool, rand: &mut RomuDuoJrRand) -> u8 {
    match world {
        World::PassiveClock(w) => w.sample(rand),
        World::Crossing(w) => w.sample(rand),
        _ => world.sample_action(rand.below(NonZeroUsize::new(4).unwrap()) as u8, broken),
    }
}

fn play(world: &World, broken: bool, samples: usize, rand: &mut RomuDuoJrRand) -> Vec<State> {
    let mut pool = vec![world.initial()];
    if let World::Chain(w) = world {
        for stage in 0..u8::try_from(w.stages.len()).expect("chain stages fit") {
            for charge in 0..=20 {
                pool.push(State::Chain(w.enter(stage, charge)));
            }
        }
    }
    let mut seen: BTreeSet<State> = pool.iter().copied().collect();
    let mut states = Vec::with_capacity(samples);
    while states.len() < samples {
        let mut state = pool[rand.below(NonZeroUsize::new(pool.len()).unwrap())];
        for _ in 0..1 + rand.below(NonZeroUsize::new(32).unwrap()) {
            state = world.step(state, action(world, broken, rand));
            states.push(state);
            if seen.insert(state) {
                pool.push(state);
            }
        }
    }
    states.truncate(samples);
    states
}

fn agree<H: ArchiveKey + Ord + Copy + std::fmt::Debug>(
    name: &str,
    hand: &[H],
    hand_shape: KeyShape,
    compiled: &[AssertionKey],
    compiled_shape: KeyShape,
) -> usize {
    assert_eq!(hand_shape, compiled_shape, "{name}: key shapes differ");
    let pairs: Vec<(H, AssertionKey)> = hand
        .iter()
        .copied()
        .zip(compiled.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for (index, &(h1, c1)) in pairs.iter().enumerate() {
        for &(h2, c2) in &pairs[index..] {
            let hand_cell = (h1.progress(), h1.place()) == (h2.progress(), h2.place());
            let compiled_cell = (c1.progress(), c1.place()) == (c2.progress(), c2.place());
            assert_eq!(
                hand_cell, compiled_cell,
                "{name}: cell differs for {h1:?} {h2:?} / {c1:?} {c2:?}"
            );
            assert_eq!(
                hand_cell && h1.identity() == h2.identity(),
                compiled_cell && c1.identity() == c2.identity(),
                "{name}: slot differs for {h1:?} {h2:?} / {c1:?} {c2:?}"
            );
            assert_eq!(
                h1.progress().cmp(&h2.progress()),
                c1.progress().cmp(&c2.progress()),
                "{name}: progress order differs for {h1:?} {h2:?} / {c1:?} {c2:?}"
            );
            for preference in 0..hand_shape.preferences {
                assert_eq!(
                    h1.preference_cmp(preference, h2),
                    c1.preference_cmp(preference, c2),
                    "{name}: preference {preference} differs for {h1:?} {h2:?} / {c1:?} {c2:?}"
                );
            }
        }
    }
    pairs.len()
}

fn keyed<K: Keying>(keying: &K, case: &Case, states: &[State]) -> Vec<K::Key> {
    states
        .iter()
        .map(|&state| {
            keying
                .key(&case.world, state, case.broken)
                .expect("keyed state")
        })
        .collect()
}

fn check(case: &Case, samples: usize, rand: &mut RomuDuoJrRand) -> (usize, usize) {
    let states = play(&case.world, case.broken, samples, rand);
    let strategic =
        Strategic::shipped(&case.world, case.broken, case.keep).expect("shipped strategy");
    let catalog = crate::assertions::catalog(&case.world, case.broken).expect("catalog");
    for &state in &states {
        crate::assertions::checked_record(&catalog, &case.world, state, case.broken)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
    }
    let compiled = keyed(&strategic, case, &states);
    let distinct = states.iter().collect::<BTreeSet<_>>().len();
    let pairs = match case.keep {
        Keep::Portfolio => {
            let hand = Hand::<false>;
            agree(
                &case.name,
                &keyed(&hand, case, &states),
                hand.shape(),
                &compiled,
                strategic.shape(),
            )
        }
        Keep::CapacityTwo => {
            let hand = Hand::<true>;
            agree(
                &case.name,
                &keyed(&hand, case, &states),
                hand.shape(),
                &compiled,
                strategic.shape(),
            )
        }
    };
    (distinct, pairs)
}

fn run(samples: usize) {
    let mut rand = RomuDuoJrRand::with_seed(crate::test_seed());
    for case in cases(&mut rand) {
        let (distinct, pairs) = check(&case, samples, &mut rand);
        println!(
            "{}: {samples} sampled states, {distinct} distinct, {pairs} distinct key pairs agree",
            case.name
        );
    }
}

#[test]
fn compiled_keys_match_hand_keys() {
    run(2_000);
}

#[test]
#[ignore = "the ladder sample; run in release"]
fn compiled_keys_match_hand_keys_on_the_ladder() {
    run(20_000);
}

fn recorded(workload: &crate::Workload<Strategic>, seed: u64) -> Vec<u8> {
    let config = crate::campaign_config(workload, seed, 20_000, 1);
    let mut stream = crate::BoundedStream(Vec::new());
    searcher::search::campaign::run_campaign_checkpointed_with_options(
        workload,
        &config,
        &searcher::search::campaign::CampaignOrigin::Genesis,
        &mut stream,
        None,
        searcher::search::campaign::CampaignExecutionOptions {
            work_budget: Some(20_000),
            ..Default::default()
        },
    )
    .expect("strategy campaign runs");
    stream.0
}

#[test]
fn two_strategies_replay_their_own_recordings_in_one_process() {
    let mut rand = RomuDuoJrRand::with_seed(crate::test_seed());
    let world: World =
        serde_json::from_value(chain(&mut rand, true, true, "engaged", false, 2)).expect("chain");
    let shipped = Strategic::shipped(&world, false, Keep::Portfolio).expect("shipped strategy");
    let flat = Strategic::new(
        &world,
        false,
        "capacity = 2\ntier = \"goal\"\nspread = \"chain.stage\"\nprefer = [[\"chain.charge: more\"]]\n",
    )
    .expect("flat strategy");
    assert_ne!(shipped.policy(), flat.policy());
    let workloads = [shipped, flat].map(|keying| crate::Workload {
        config: world.clone(),
        broken: false,
        scale: None,
        keying,
    });
    let seed = rand.next_u64();
    let streams: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let runs: Vec<_> = workloads
            .iter()
            .map(|workload| scope.spawn(move || recorded(workload, seed)))
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("campaign thread"))
            .collect()
    });
    assert_ne!(streams[0], streams[1]);
    for (workload, stream) in workloads.iter().zip(&streams) {
        searcher::search::campaign::replay_campaign_checkpointed(workload, stream, None, None)
            .expect("a recording replays under its own strategy");
        assert_eq!(&recorded(workload, seed), stream);
    }
    for (workload, stream) in workloads.iter().zip(streams.iter().rev()) {
        assert!(
            searcher::search::campaign::replay_campaign_checkpointed(workload, stream, None, None)
                .is_err(),
            "a recording replays only under the strategy that wrote it"
        );
    }
}
