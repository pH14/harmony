// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, VecDeque};

use searcher::search::{
    archive::Input,
    shrink::{
        ShrinkConfig, ShrinkOutcome, shrink,
        window::{WindowConfig, WindowStop, search_window},
    },
};
use tiny_worlds::{Workload, graph, maze, worlds::World};

const PATTERN: u16 = 0b1011_0010;
const LENGTH: u8 = 8;

fn maze() -> Workload {
    Workload {
        config: World::Maze(maze::Config {
            length: LENGTH,
            pattern: PATTERN,
            reverse_actions: false,
        }),
        broken: false,
        scale: None,
        keying: tiny_worlds::Hand::<false>,
    }
}

fn shortest() -> Vec<u8> {
    let mut actions = (0..LENGTH)
        .map(|bit| u8::from(PATTERN >> bit & 1 == 1))
        .collect::<Vec<_>>();
    actions.push(0);
    actions
}

fn wandering() -> Input<u8> {
    let mut actions = vec![3, 3, 3, 0, 1, 1, 0, 2, 3, 3, 1, 0, 0, 1, 1, 0, 1, 2];
    actions.extend(shortest());
    assert_eq!(actions.len(), 3 * shortest().len());
    Input { actions }
}

fn run(seed: u64, workers: usize) -> ShrinkOutcome<u8> {
    let mut config = ShrinkConfig::new((), seed);
    config.workers = workers;
    config.window = Some(8);
    config.job_budget = Some(4_000);
    shrink(&maze(), &config, &wandering(), |_| Ok(())).expect("shrink the maze tape")
}

#[test]
fn a_tape_three_times_the_shortest_path_shrinks_to_it_on_eight_seeds() {
    for seed in 1..=8 {
        let outcome = run(seed, 2);
        assert_eq!(outcome.input.actions, shortest(), "seed {seed}");
        assert_eq!(
            outcome.progress.initial_cost.map(|cost| cost.primary),
            Some(27)
        );
        assert_eq!(outcome.progress.cost.map(|cost| cost.primary), Some(9));
    }
}

#[test]
fn the_same_seed_gives_the_same_tape_at_one_and_four_workers() {
    let one = run(11, 1);
    let four = run(11, 4);
    assert_eq!(one, four);
}

#[test]
fn shortcut_snapshots_give_the_same_tape_from_shortcuts_alone() {
    let mut config = ShrinkConfig::new((), 5);
    config.window = Some(1);
    config.ordinary = false;
    config.until_shortcuts_exhausted = true;
    let plain = shrink(&maze(), &config, &wandering(), |_| Ok(())).expect("shrink");
    config.shortcut_snapshots = true;
    let snapshotted = shrink(&maze(), &config, &wandering(), |_| Ok(())).expect("shrink");
    assert_eq!(snapshotted.input, plain.input);
    assert_eq!(snapshotted.improvements.len(), plain.improvements.len());
    assert_eq!(snapshotted.progress.jobs, plain.progress.jobs);
    assert_eq!(plain.progress.jobs.ordinary, 0);
}

#[test]
fn primary_shortcuts_still_delete_actions_that_have_no_simpler_action() {
    let mut config = ShrinkConfig::new((), 3);
    config.window = Some(1);
    config.ordinary = false;
    config.primary_shortcuts = true;
    config.shortcut_snapshots = true;
    config.until_shortcuts_exhausted = true;
    let outcome = shrink(&maze(), &config, &wandering(), |_| Ok(())).expect("shrink");
    assert_eq!(outcome.input.actions, shortest());
    assert_eq!(
        outcome.stop,
        searcher::search::shrink::ShrinkStop::ShortcutsExhausted
    );
}

fn graph_world(layout: u64) -> (Workload, graph::Config) {
    let config = graph::Config {
        nodes: 256,
        places: 16,
        levels: 4,
        layout,
    };
    (
        Workload {
            config: World::Graph(config),
            broken: false,
            scale: None,
            keying: tiny_worlds::Hand::<false>,
        },
        config,
    )
}

fn shortest_from(config: &graph::Config, start: graph::State) -> Option<Vec<u8>> {
    let mut paths = BTreeMap::from([(start, Vec::new())]);
    let mut pending = VecDeque::from([start]);
    while let Some(state) = pending.pop_front() {
        let path = paths[&state].clone();
        if config.goal(state) {
            return Some(path);
        }
        for action in 0..4 {
            let next = config.step(state, action);
            if let std::collections::btree_map::Entry::Vacant(entry) = paths.entry(next) {
                let mut longer = path.clone();
                longer.push(action);
                entry.insert(longer);
                pending.push_back(next);
            }
        }
    }
    None
}

fn detour(config: &graph::Config) -> (usize, Vec<u8>) {
    let shortest = shortest_from(config, config.initial()).expect("the goal is reachable");
    let mut state = config.initial();
    for walked in 0..usize::try_from(config.nodes).expect("nodes fit") {
        if let Some(rest) = shortest_from(config, state)
            && walked + rest.len() == 3 * shortest.len()
        {
            let mut actions = vec![0; walked];
            actions.extend(rest);
            return (shortest.len(), actions);
        }
        state = config.step(state, 0);
    }
    panic!("no walk-then-shortest tape is three times the shortest path");
}

#[test]
fn a_graph_detour_three_times_the_shortest_path_shrinks_to_its_length_on_eight_seeds() {
    let (workload, config) = graph_world(0x5eed_0001);
    let (shortest, actions) = detour(&config);
    assert!(shortest >= 3, "the shortest path has {shortest} actions");
    let input = Input { actions };
    for seed in 1..=8 {
        let mut shrink_config = ShrinkConfig::new((), seed);
        shrink_config.workers = 2;
        shrink_config.window = Some(8);
        shrink_config.ordinary_length = Some(8);
        shrink_config.job_budget = Some(20_000);
        let outcome = shrink(&workload, &shrink_config, &input, |_| Ok(())).expect("shrink");
        assert_eq!(
            outcome.progress.cost.map(|cost| cost.primary),
            Some(u64::try_from(shortest).expect("length fits")),
            "seed {seed}: {:?}",
            outcome.progress
        );
        let mut state = config.initial();
        for action in &outcome.input.actions {
            state = config.step(state, *action);
        }
        assert!(config.goal(state));
    }
}

#[test]
fn a_graph_tape_that_stops_short_of_the_goal_searches_from_its_states_until_an_arrival() {
    let (workload, config) = graph_world(0x5eed_0001);
    let (shortest, mut actions) = detour(&config);
    let detour_length = u64::try_from(actions.len()).expect("length fits");
    actions.truncate(actions.len() - 2);
    let input = Input { actions };
    for seed in 1..=4 {
        let mut shrink_config = ShrinkConfig::new((), seed);
        shrink_config.workers = 2;
        shrink_config.window = Some(8);
        shrink_config.ordinary_length = Some(8);
        shrink_config.job_budget = Some(20_000);
        let outcome = shrink(&workload, &shrink_config, &input, |_| Ok(())).expect("shrink");
        assert_eq!(outcome.progress.initial_cost, None, "seed {seed}");
        let cost = outcome.progress.cost.expect("an arrival").primary;
        assert!(
            (u64::try_from(shortest).expect("length fits")..=detour_length).contains(&cost),
            "seed {seed}: {:?}",
            outcome.progress
        );
        let mut state = config.initial();
        for action in &outcome.input.actions {
            state = config.step(state, *action);
        }
        assert!(config.goal(state));
    }
}

#[test]
fn a_window_over_the_whole_tape_finds_the_shortest_path_and_proves_it() {
    let config = WindowConfig {
        start: 0,
        expansion_budget: None,
        frontier_limit: 1_000,
    };
    let outcome = search_window(&maze(), &(), &wandering(), &config, |_| Ok(()))
        .expect("search the maze window");
    assert_eq!(outcome.input.actions, shortest());
    assert_eq!(outcome.progress.cost.primary, 9);
    assert_eq!(outcome.progress.lower_bound.primary, 9);
    assert_eq!(outcome.stop, WindowStop::Complete);
}

#[test]
fn a_window_keeps_the_tape_before_its_start() {
    let config = WindowConfig {
        start: 18,
        expansion_budget: None,
        frontier_limit: 1_000,
    };
    let outcome = search_window(&maze(), &(), &wandering(), &config, |_| Ok(()))
        .expect("search the maze window");
    assert_eq!(outcome.input.actions[..18], wandering().actions[..18]);
    assert!(outcome.progress.cost.primary <= 27);
}
