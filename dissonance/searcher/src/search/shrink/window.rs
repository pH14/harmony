// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use serde::{Deserialize, Serialize};

use super::{ShrinkCost, ShrinkPolicy};
use crate::search::{archive::Input, rollout::classify};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WindowConfig {
    pub start: usize,
    pub expansion_budget: Option<u64>,
    pub frontier_limit: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowStop {
    Complete,
    FrontierEmpty,
    ExpansionBudget,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WindowProgress {
    pub expansions: u64,
    pub children: u64,
    pub merged: u64,
    pub pruned: u64,
    pub terminal: u64,
    pub dropped: u64,
    pub goals: u64,
    pub frontier_peak: usize,
    pub start_cost: ShrinkCost,
    pub initial_cost: ShrinkCost,
    pub cost: ShrinkCost,
    pub lower_bound: ShrinkCost,
}

#[derive(Clone, Debug)]
pub struct WindowOutcome<A: Ord> {
    pub input: Input<A>,
    pub progress: WindowProgress,
    pub stop: WindowStop,
}

pub enum WindowEvent<'a, A: Ord> {
    Improved {
        input: &'a Input<A>,
        progress: &'a WindowProgress,
    },
    Expanded {
        progress: &'a WindowProgress,
    },
}

struct Node<A> {
    parent: Option<usize>,
    action: Option<A>,
}

struct Open<G: ShrinkPolicy> {
    snapshot: G::Snapshot,
    milestones: G::Milestones,
}

fn path<A: Copy>(nodes: &[Node<A>], mut index: usize) -> Vec<A> {
    let mut actions = Vec::new();
    while let Some(action) = nodes[index].action {
        actions.push(action);
        match nodes[index].parent {
            Some(parent) => index = parent,
            None => break,
        }
    }
    actions.reverse();
    actions
}

pub fn search_window<G>(
    workload: &G,
    run: &G::Run,
    incumbent: &Input<G::Action>,
    config: &WindowConfig,
    mut observe: impl FnMut(WindowEvent<'_, G::Action>) -> Result<(), Box<dyn Error>>,
) -> Result<WindowOutcome<G::Action>, Box<dyn Error>>
where
    G: ShrinkPolicy,
{
    let actions = workload.window_actions(run);
    if actions.is_empty() {
        return Err("the workload declares no window actions".into());
    }
    let mut target = workload
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    workload.reset(&mut target);
    let mut milestones = G::Milestones::default();
    let mut cost = ShrinkCost::ZERO;
    let mut start = None;
    let mut incumbent_cost = None;
    for (index, action) in incumbent.actions.iter().enumerate() {
        if index == config.start {
            start = Some((workload.snapshot(&mut target)?, milestones, cost));
        }
        workload.apply_action(&mut target, action, &mut milestones)?;
        cost = cost + workload.shrink_cost(action, &target);
        let outcome = classify(workload, run, &target)?;
        if outcome.disposition.is_failed() {
            return Err("the window incumbent failed to execute".into());
        }
        if outcome.objective_reached {
            incumbent_cost = Some((index + 1, cost));
            break;
        }
        if outcome.disposition.is_terminal() {
            break;
        }
    }
    let (goal_length, initial_cost) =
        incumbent_cost.ok_or("the window incumbent does not reach the goal")?;
    let (snapshot, milestones, start_cost) =
        start.ok_or("the window starts at or after the incumbent reaches the goal")?;
    let prefix = &incumbent.actions[..config.start];
    let mut best = initial_cost;
    let mut best_input = Input {
        actions: incumbent.actions[..goal_length].to_vec(),
    };
    workload.restore(&mut target, &snapshot)?;
    let start_bound = workload.remaining_cost_bound(run, &target);
    let mut progress = WindowProgress {
        start_cost,
        initial_cost,
        cost: initial_cost,
        lower_bound: start_cost + start_bound,
        ..WindowProgress::default()
    };
    let mut nodes = vec![Node {
        parent: None,
        action: None,
    }];
    let mut open: BTreeMap<usize, Open<G>> = BTreeMap::new();
    open.insert(
        0,
        Open {
            snapshot,
            milestones,
        },
    );
    let mut frontier: BTreeSet<(ShrinkCost, ShrinkCost, usize)> = BTreeSet::new();
    frontier.insert((start_cost + start_bound, start_cost, 0));
    let mut seen: BTreeMap<u64, ShrinkCost> = workload
        .window_fingerprint(&target)
        .map(|fingerprint| (fingerprint, start_cost))
        .into_iter()
        .collect();
    let stop = loop {
        let Some((estimate, cost, index)) = frontier.pop_first() else {
            if progress.dropped > 0 {
                break WindowStop::FrontierEmpty;
            }
            progress.lower_bound = best;
            break WindowStop::Complete;
        };
        if estimate >= best {
            progress.lower_bound = best;
            break WindowStop::Complete;
        }
        progress.lower_bound = estimate;
        if config
            .expansion_budget
            .is_some_and(|budget| progress.expansions >= budget)
        {
            break WindowStop::ExpansionBudget;
        }
        let parent = open
            .remove(&index)
            .ok_or("a frontier state has no snapshot")?;
        progress.expansions += 1;
        for action in &actions {
            progress.children += 1;
            workload.restore(&mut target, &parent.snapshot)?;
            let mut milestones = parent.milestones;
            workload.apply_action(&mut target, action, &mut milestones)?;
            let child_cost = cost + workload.shrink_cost(action, &target);
            let outcome = classify(workload, run, &target)?;
            if outcome.disposition.is_failed() {
                return Err("a window expansion failed to execute".into());
            }
            if outcome.objective_reached {
                progress.goals += 1;
                if child_cost < best {
                    best = child_cost;
                    nodes.push(Node {
                        parent: Some(index),
                        action: Some(*action),
                    });
                    let mut tape = prefix.to_vec();
                    tape.extend(path(&nodes, nodes.len() - 1));
                    best_input = Input { actions: tape };
                    progress.cost = best;
                    observe(WindowEvent::Improved {
                        input: &best_input,
                        progress: &progress,
                    })?;
                }
                continue;
            }
            if outcome.disposition.is_terminal() {
                progress.terminal += 1;
                continue;
            }
            let fingerprint = workload.window_fingerprint(&target);
            if fingerprint
                .and_then(|fingerprint| seen.get(&fingerprint))
                .is_some_and(|&earlier| earlier <= child_cost)
            {
                progress.merged += 1;
                continue;
            }
            let bound = workload.remaining_cost_bound(run, &target);
            if child_cost + bound >= best {
                progress.pruned += 1;
                continue;
            }
            if let Some(fingerprint) = fingerprint {
                seen.insert(fingerprint, child_cost);
            }
            let snapshot = workload.snapshot(&mut target)?;
            nodes.push(Node {
                parent: Some(index),
                action: Some(*action),
            });
            let child = nodes.len() - 1;
            open.insert(
                child,
                Open {
                    snapshot,
                    milestones,
                },
            );
            frontier.insert((child_cost + bound, child_cost, child));
        }
        while frontier.len() > config.frontier_limit.max(1) {
            if let Some((_, _, dropped)) = frontier.pop_last() {
                open.remove(&dropped);
                progress.dropped += 1;
            }
        }
        progress.frontier_peak = progress.frontier_peak.max(frontier.len());
        observe(WindowEvent::Expanded {
            progress: &progress,
        })?;
    };
    Ok(WindowOutcome {
        input: best_input,
        progress,
        stop,
    })
}
