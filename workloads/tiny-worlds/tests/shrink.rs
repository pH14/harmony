// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, VecDeque};

use searcher::search::{
    archive::Input,
    shrink::{ShrinkConfig, ShrinkOutcome, shrink},
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
    config.batch = 8;
    config.job_budget = Some(4_000);
    shrink(&maze(), &config, &wandering(), |_| Ok(())).expect("shrink the maze tape")
}

#[test]
fn a_tape_three_times_the_shortest_path_shrinks_to_it_on_eight_seeds() {
    for seed in 1..=8 {
        let outcome = run(seed, 2);
        assert_eq!(outcome.input.actions, shortest(), "seed {seed}");
        assert_eq!(outcome.progress.initial_cost.primary, 27);
        assert_eq!(outcome.progress.cost.primary, 9);
    }
}

#[test]
fn the_same_seed_gives_the_same_tape_at_one_and_four_workers() {
    let one = run(11, 1);
    let four = run(11, 4);
    assert_eq!(one, four);
}

#[test]
fn a_tape_that_misses_the_goal_is_refused() {
    let config = ShrinkConfig::new((), 1);
    let input = Input {
        actions: vec![3, 3, 0],
    };
    assert!(shrink(&maze(), &config, &input, |_| Ok(())).is_err());
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
        shrink_config.batch = 8;
        shrink_config.redraw_length = 8;
        shrink_config.job_budget = Some(20_000);
        let outcome = shrink(&workload, &shrink_config, &input, |_| Ok(())).expect("shrink");
        assert_eq!(
            outcome.progress.cost.primary,
            u64::try_from(shortest).expect("length fits"),
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
