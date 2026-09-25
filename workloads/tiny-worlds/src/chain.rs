// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::worlds::{State as WorldState, World};
use crate::{
    Key, actions, backtrack, deadline, deadline_actions, delayed, map, maze, resource, route, trap,
};
use serde::{Deserialize, Serialize};

const STAGE_TIERS: u16 = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub world: World,
    pub refill_available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub stages: Vec<Stage>,
    pub carry_charge: bool,
    pub initial_charge: u8,
    pub ranked: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum LocalState {
    Resource(resource::State),
    Maze(maze::State),
    Actions(actions::State),
    Deadline(deadline::State),
    Delayed(delayed::State),
    DeadlineActions(deadline_actions::State),
    Route(route::State),
    Trap(trap::State),
    Backtrack(backtrack::State),
    Map(map::State),
}

impl LocalState {
    fn from_world(state: WorldState) -> Self {
        match state {
            WorldState::Resource(s) => Self::Resource(s),
            WorldState::Maze(s) => Self::Maze(s),
            WorldState::Actions(s) => Self::Actions(s),
            WorldState::Deadline(s) => Self::Deadline(s),
            WorldState::Delayed(s) => Self::Delayed(s),
            WorldState::DeadlineActions(s) => Self::DeadlineActions(s),
            WorldState::Route(s) => Self::Route(s),
            WorldState::Trap(s) => Self::Trap(s),
            WorldState::Backtrack(s) => Self::Backtrack(s),
            WorldState::Map(s) => Self::Map(s),
            WorldState::Chain(_) | WorldState::Graph(_) => {
                unreachable!("validated non-nested, non-graph stage")
            }
        }
    }
    pub fn world_state(self) -> WorldState {
        match self {
            Self::Resource(s) => WorldState::Resource(s),
            Self::Maze(s) => WorldState::Maze(s),
            Self::Actions(s) => WorldState::Actions(s),
            Self::Deadline(s) => WorldState::Deadline(s),
            Self::Delayed(s) => WorldState::Delayed(s),
            Self::DeadlineActions(s) => WorldState::DeadlineActions(s),
            Self::Route(s) => WorldState::Route(s),
            Self::Trap(s) => WorldState::Trap(s),
            Self::Backtrack(s) => WorldState::Backtrack(s),
            Self::Map(s) => WorldState::Map(s),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub stage: u8,
    pub local: LocalState,
    pub charge: u8,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.stages.len()) {
            return Err("a chain requires 1..=16 stages".into());
        }
        if self.initial_charge > 31 || (!self.carry_charge && self.initial_charge != 0) {
            return Err("chain initial_charge must be 0..=31, and zero without carry".into());
        }
        let mut capacity = None;
        for stage in &self.stages {
            if matches!(stage.world, World::Chain(_)) {
                return Err("nested chains are unsupported".into());
            }
            if matches!(stage.world, World::Graph(_)) {
                return Err("graph stages are unsupported".into());
            }
            stage.world.validate()?;
            if let World::Resource(w) = &stage.world {
                if self.carry_charge {
                    if w.initial_charge != 0 || self.initial_charge > w.max_charge {
                        return Err("carrying stages use chain initial charge and require local initial_charge=0".into());
                    }
                    if capacity.is_some_and(|c| c != w.max_charge) {
                        return Err("carrying resource stages must share max_charge".into());
                    }
                    capacity = Some(w.max_charge);
                }
            } else if !stage.refill_available {
                return Err("refill_available=false requires a resource stage".into());
            }
        }
        if self.carry_charge && capacity.is_none() {
            return Err("charge carry requires a resource stage".into());
        }
        Ok(())
    }

    fn enter(&self, stage: u8, charge: u8) -> State {
        let mut local = self.stages[usize::from(stage)].world.initial();
        if self.carry_charge
            && let WorldState::Resource(s) = &mut local
        {
            s.charge = charge;
        }
        State {
            stage,
            local: LocalState::from_world(local),
            charge,
        }
    }

    pub fn initial(&self) -> State {
        self.enter(0, self.initial_charge)
    }

    pub fn valid_state(&self, state: State) -> bool {
        let Some(stage) = self.stages.get(usize::from(state.stage)) else {
            return false;
        };
        let local = state.local.world_state();
        if !stage.world.valid_state(local)
            || (stage.world.goal(local) && usize::from(state.stage) + 1 != self.stages.len())
        {
            return false;
        }
        if !self.carry_charge {
            return state.charge == 0;
        }
        let capacity = self
            .stages
            .iter()
            .find_map(|s| match &s.world {
                World::Resource(w) => Some(w.max_charge),
                _ => None,
            })
            .unwrap_or(0);
        state.charge <= capacity
            && match local {
                WorldState::Resource(s) => s.charge == state.charge,
                _ => true,
            }
    }

    pub fn goal(&self, state: State) -> bool {
        usize::from(state.stage) + 1 == self.stages.len()
            && self.valid_state(state)
            && self.stages[usize::from(state.stage)]
                .world
                .goal(state.local.world_state())
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if !self.valid_state(state) || self.goal(state) || action >= 4 {
            return state;
        }
        let stage = &self.stages[usize::from(state.stage)];
        let before = state.local.world_state();
        let local = if !stage.refill_available && action == 1 {
            before
        } else {
            stage.world.step(before, action)
        };
        let charge = if self.carry_charge {
            match local {
                WorldState::Resource(s) => s.charge,
                _ => state.charge,
            }
        } else {
            0
        };
        if stage.world.goal(local) && usize::from(state.stage) + 1 < self.stages.len() {
            self.enter(state.stage + 1, charge)
        } else {
            State {
                local: LocalState::from_world(local),
                charge,
                ..state
            }
        }
    }

    pub fn key(&self, state: State, broken: bool) -> Key {
        let mut key = self.stages[usize::from(state.stage)]
            .world
            .key(state.local.world_state(), false);
        let staged = self.carry_charge || !broken;
        if staged {
            key.place += u16::from(state.stage) * crate::STAGE_PLACES;
        }
        key.stock = if self.carry_charge && !broken {
            state.charge
        } else {
            0
        };
        key.tier = match (self.ranked, staged) {
            (true, true) => key.tier + u16::from(state.stage) * STAGE_TIERS,
            (true, false) => key.tier,
            (false, _) => 0,
        };
        key.goal = self.goal(state);
        key
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;
    use crate::{Workload, run};
    use searcher::search::archive::ArchiveKey;

    fn wait_stage() -> Stage {
        Stage {
            world: World::Delayed(delayed::Config {
                horizon: 2,
                distractions: 2,
                mode: delayed::Mode::Sequence,
                placement: delayed::Placement::Identity,
                sticky_credit: false,
                ammo: 0,
            }),
            refill_available: true,
        }
    }

    fn independent(n: usize) -> Config {
        Config {
            stages: vec![wait_stage(); n],
            carry_charge: false,
            initial_charge: 0,
            ranked: false,
        }
    }

    fn resource_stage(barrier_charge: u8, route_cost: u8, refill_available: bool) -> Stage {
        Stage {
            world: World::Resource(resource::Config {
                initial_charge: 0,
                initial_health: 3,
                barrier_charge,
                route_cost,
                health_cost: 0,
                refill_health_cost: 0,
                refill_amount: 2,
                max_charge: 10,
                corridor_len: 1,
            }),
            refill_available,
        }
    }

    fn carrying() -> Config {
        Config {
            stages: vec![
                resource_stage(1, 0, true),
                Stage {
                    world: World::Maze(maze::Config {
                        length: 2,
                        pattern: 1,
                        reverse_actions: false,
                    }),
                    refill_available: true,
                },
                resource_stage(5, 1, false),
            ],
            carry_charge: true,
            initial_charge: 0,
            ranked: false,
        }
    }

    fn execute(w: &Config, state: State, actions: &[u8]) -> State {
        actions.iter().fold(state, |s, &a| w.step(s, a))
    }

    #[test]
    fn whole_chain_requires_every_stage_on_one_trajectory() {
        let w = independent(4);
        assert!(w.reachable().unwrap());
        let mut state = w.initial();
        for i in 1..=8 {
            state = w.step(state, 0);
            assert_eq!(w.goal(state), i == 8);
            assert_eq!(state.stage, (i / 2).min(3));
            assert!(w.valid_state(state));
        }
        assert_eq!(w.step(state, 1), state);
    }

    #[test]
    fn stage_identity_does_not_rank_later_locations_higher() {
        let w = independent(2);
        let first = w.initial();
        let second = execute(&w, first, &[0, 0]);
        let a = w.key(first, false);
        let b = w.key(second, false);
        assert_ne!(a.place(), b.place());
        assert_eq!(a.progress(), b.progress());
        for preference in 0..Key::<false>::preferences() {
            assert!(a.preference_cmp(preference, b).is_eq());
        }
        assert_eq!(w.key(first, true), w.key(second, true));
        let ranked = Config {
            ranked: true,
            ..w.clone()
        };
        assert!(ranked.key(first, false).tier < ranked.key(second, false).tier);
        assert_eq!(ranked.key(first, true), ranked.key(second, true));
    }

    #[test]
    fn cross_stage_preferences_compare_only_persistent_stock() {
        let resource: Key = Key {
            stock: 7,
            place: 0,
            context: 0,
            charge: 7,
            health: 3,
            goal: false,
            tier: 0,
        };
        let maze = Key {
            place: crate::STAGE_PLACES,
            charge: 0,
            health: 0,
            ..resource
        };
        for preference in 0..Key::<false>::preferences() {
            assert!(resource.preference_cmp(preference, maze).is_eq());
            assert!(maze.preference_cmp(preference, resource).is_eq());
            assert!(
                resource
                    .preference_cmp(preference, Key { stock: 3, ..maze })
                    .is_gt()
            );
            assert!(
                resource
                    .preference_cmp(preference, Key { place: 1, ..maze })
                    .is_gt()
            );
        }
    }

    #[test]
    fn charge_survives_maze_and_controls_access_to_later_barrier() {
        let w = carrying();
        assert!(w.reachable().unwrap());
        let low = execute(&w, w.initial(), &[1, 0, 0]);
        let high = execute(&w, w.initial(), &[1, 1, 1, 1, 0, 0]);
        assert_eq!((low.stage, low.charge), (1, 1));
        assert_eq!((high.stage, high.charge), (1, 7));
        assert_eq!(low.local, high.local);
        let a = w.key(low, false);
        let b = w.key(high, false);
        assert_eq!(a.place(), b.place());
        assert_eq!(a.identity(), b.identity());
        assert!(b.preference_cmp(0, a).is_gt());
        assert_eq!(w.key(low, true), w.key(high, true));
        let suffix = [1, 0, 0, 0, 0];
        assert!(!w.goal(execute(&w, low, &suffix)));
        let finished = execute(&w, high, &suffix);
        assert!(w.goal(finished));
        assert_eq!(finished.charge, 1);
        let stranded = execute(&w, low, &[1, 0, 0]);
        assert_eq!(stranded.stage, 2);
        assert_eq!(w.step(stranded, 1), stranded);
    }

    #[test]
    fn composition_oracle_detects_missing_supply_and_capacity() {
        let mut w = carrying();
        w.stages[0].refill_available = false;
        assert!(!w.reachable().unwrap());
        let mut w = carrying();
        for stage in &mut w.stages {
            if let World::Resource(resource) = &mut stage.world {
                resource.max_charge = 5;
            }
        }
        assert!(!w.reachable().unwrap());
        w.initial_charge = 5;
        assert!(!w.reachable().unwrap());
    }

    #[test]
    fn snapshots_reject_invalid_stage_terminal_and_stock_states() {
        let w = independent(2);
        let initial = w.initial();
        assert!(!w.valid_state(State {
            stage: 2,
            ..initial
        }));
        assert!(!w.valid_state(State {
            charge: 1,
            ..initial
        }));
        let terminal = execute(&independent(1), initial, &[0, 0]);
        assert!(!w.valid_state(terminal));
        let w = carrying();
        assert!(!w.valid_state(State {
            charge: 3,
            ..w.initial()
        }));
        assert!(!w.valid_state(State {
            local: initial.local,
            ..w.initial()
        }));
    }

    #[test]
    fn chain_contract_rejects_nesting_unused_fields_and_inconsistent_capacity() {
        for n in [0, 17] {
            assert!(independent(n).validate().is_err());
        }
        let mut w = independent(1);
        w.stages[0].world = World::Chain(independent(1));
        assert!(w.validate().is_err());
        let mut w = independent(1);
        w.stages[0].refill_available = false;
        assert!(w.validate().is_err());
        let mut w = carrying();
        if let World::Resource(s) = &mut w.stages[2].world {
            s.max_charge = 9;
        }
        assert!(w.validate().is_err());
        let mut v = serde_json::to_value(carrying()).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
        let mut v = serde_json::to_value(carrying()).unwrap();
        v["stages"][0]
            .as_object_mut()
            .unwrap()
            .remove("refill_available");
        assert!(serde_json::from_value::<Config>(v).is_err());
    }

    #[test]
    fn long_composition_needs_more_than_128_actions() {
        let mut w = independent(16);
        for stage in &mut w.stages {
            if let World::Delayed(local) = &mut stage.world {
                local.horizon = 9;
            }
        }
        assert!(w.reachable().unwrap());
        let prefix = execute(&w, w.initial(), &[0; 128]);
        assert!(!w.goal(prefix));
        assert!(w.goal(execute(&w, prefix, &[0; 16])));
    }

    #[test]
    fn real_campaign_replays_whole_chain_and_accounts_for_parent_work() {
        for config in [independent(1), independent(4), carrying()] {
            for broken in [false, true] {
                let w = Workload {
                    config: World::Chain(config.clone()),
                    broken,
                    scale: None,
                };
                let report = run(&w, crate::test_seed(), 1000, true).unwrap();
                assert_eq!(report["verified"], true);
                assert!(
                    report["pre_objective_continuation_jobs"].as_u64().unwrap()
                        <= report["continuation_jobs"].as_u64().unwrap()
                );
                assert!(
                    report["pre_objective_continuation_work"].as_u64().unwrap()
                        <= report["first_objective_work"]
                            .as_u64()
                            .unwrap_or(report["work"].as_u64().unwrap())
                );
                let work: u64 = report["chain_work_by_parent_stage"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap())
                    .sum();
                assert_eq!(work, report["work"].as_u64().unwrap());
                assert_eq!(report["evidence"]["chain_first_reach_work"][0], 0);
            }
        }
    }

    fn ranked_shape() -> Config {
        let route = route::Config {
            length: 3,
            pattern: 0b00_01_10,
            attack: 2,
            shifted: false,
            upgrade_required: true,
            ranked_upgrade: true,
        };
        let trap = trap::Config {
            length: 2,
            pattern: 0b01_00,
            trap_len: 1,
            rooms: 2,
        };
        Config {
            stages: vec![
                resource_stage(1, 0, true),
                Stage {
                    world: World::Route(route),
                    refill_available: true,
                },
                Stage {
                    world: World::Trap(trap),
                    refill_available: true,
                },
                wait_stage(),
            ],
            carry_charge: true,
            initial_charge: 0,
            ranked: true,
        }
    }

    #[test]
    fn ranked_chain_ranks_stages_above_every_earlier_leaf_tier() {
        let w = ranked_shape();
        assert!(w.reachable().unwrap());
        let resource = execute(&w, w.initial(), &[1, 0, 0]);
        assert_eq!(resource.stage, 1);
        let blocked = execute(&w, resource, &[2, 1, 0]);
        let upgraded = execute(&w, blocked, &[2, 2]);
        assert_eq!(w.key(blocked, false).tier, STAGE_TIERS);
        assert_eq!(w.key(upgraded, false).tier, STAGE_TIERS + 1);
        let trap_stage = execute(&w, upgraded, &[2, 1, 0]);
        assert_eq!(trap_stage.stage, 2);
        let item = execute(&w, trap_stage, &[3]);
        assert_eq!(w.key(item, false).tier, 2 * STAGE_TIERS + 1);
        let last = execute(&w, trap_stage, &[0, 1]);
        assert_eq!(last.stage, 3);
        assert!(w.key(last, false).tier > w.key(item, false).tier);
        let unranked = Config {
            ranked: false,
            ..w.clone()
        };
        for state in [upgraded, item, last] {
            assert_eq!(unranked.key(state, false).tier, 0);
        }
        for broken in [false, true] {
            let workload = Workload {
                config: World::Chain(w.clone()),
                broken,
                scale: None,
            };
            let report = run(&workload, crate::test_seed(), 2000, true).unwrap();
            assert_eq!(report["verified"], true);
        }
    }
}
