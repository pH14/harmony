// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};

pub const MAX_NODES: u32 = 1 << 22;
const LEVEL: u8 = 15;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub nodes: u32,
    pub places: u16,
    pub levels: u8,
    pub layout: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub node: u32,
    pub stock: u8,
    pub health: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(16..=MAX_NODES).contains(&self.nodes) {
            return Err(format!("graph nodes must be 16..={MAX_NODES}"));
        }
        if self.places == 0 || u32::from(self.places) > self.nodes {
            return Err("graph places must be 1..=nodes".into());
        }
        if self.nodes_per_place() > 1 << 16 {
            return Err("graph nodes per place must be at most 65536".into());
        }
        if !(1..=16).contains(&self.levels) {
            return Err("graph levels must be 1..=16".into());
        }
        Ok(())
    }

    fn nodes_per_place(&self) -> u32 {
        self.nodes.div_ceil(u32::from(self.places))
    }

    pub fn initial(&self) -> State {
        State {
            node: 0,
            stock: 0,
            health: LEVEL,
            goal: false,
        }
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.node < self.nodes
            && s.stock <= LEVEL
            && s.health <= LEVEL
            && s.goal == (s.node == self.nodes - 1)
    }

    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }

    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(s) || s.goal {
            return s;
        }
        let node = if action == 0 {
            s.node + 1
        } else {
            let jump = RomuDuoJrRand::with_seed(
                self.layout ^ (u64::from(s.node) << 2 | u64::from(action)),
            )
            .next_u64();
            let node = ((jump & 0xffff_ffff) * u64::from(self.nodes)) >> 32;
            return self.arrive(
                u32::try_from(node).expect("node below nodes"),
                (s.stock + (jump >> 32) as u8 % 16) % 16,
                (s.health + (jump >> 40) as u8 % 16) % 16,
            );
        };
        self.arrive(node, s.stock, s.health)
    }

    fn arrive(&self, node: u32, stock: u8, health: u8) -> State {
        State {
            node,
            stock,
            health,
            goal: node == self.nodes - 1,
        }
    }

    pub fn key(&self, s: State, broken: bool) -> Key {
        let per_place = self.nodes_per_place();
        let tier = u64::from(s.node) * u64::from(self.levels) / u64::from(self.nodes);
        Key {
            stock: s.stock,
            place: u16::try_from(s.node / per_place).expect("place fits"),
            context: u16::try_from(s.node % per_place).expect("identity fits"),
            charge: 0,
            health: s.health,
            goal: s.goal,
            tier: if broken {
                0
            } else {
                u16::try_from(tier).expect("tier fits")
            },
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(nodes: u32, places: u16) -> Config {
        Config {
            nodes,
            places,
            levels: 4,
            layout: crate::test_seed(),
        }
    }

    #[test]
    fn stepping_forward_reaches_the_goal_from_every_state() {
        let w = config(64, 4);
        for node in 0..w.nodes {
            let mut s = State {
                node,
                stock: (node % 16) as u8,
                health: 15 - (node % 16) as u8,
                goal: node == w.nodes - 1,
            };
            for _ in node..w.nodes - 1 {
                assert!(!w.goal(s));
                s = w.step(s, 0);
            }
            assert!(w.goal(s));
        }
    }

    #[test]
    fn exhaustive_reachability_agrees_with_the_construction() {
        let w = config(256, 8);
        assert!(w.reachable().unwrap());
        assert!(crate::reachable(w.initial(), |s| w.goal(s), |s, a| w.step(s, a)).unwrap());
    }

    #[test]
    fn steps_stay_in_bounds_and_keys_fit() {
        let w = config(MAX_NODES, 1024);
        let mut rand = RomuDuoJrRand::with_seed(crate::test_seed());
        let mut s = w.initial();
        for _ in 0..100_000 {
            s = w.step(s, (rand.next_u64() % 4) as u8);
            assert!(w.state_is_bounded(s));
            let key = w.key(s, false);
            assert!(key.place < 1024 && key.tier < 4);
            assert_eq!(
                u32::from(key.place) * w.nodes_per_place() + u32::from(key.context),
                s.node
            );
            assert_eq!(w.key(s, true).tier, 0);
            if s.goal {
                s = w.initial();
            }
        }
    }

    #[test]
    fn invalid_configs_and_states_are_rejected() {
        assert!(config(8, 1).validate().is_err());
        assert!(config(MAX_NODES + 1, 1024).validate().is_err());
        assert!(config(1 << 20, 8).validate().is_err());
        assert!(config(64, 65).validate().is_err());
        let w = config(64, 4);
        assert!(!w.state_is_bounded(State {
            node: 64,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            stock: 16,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            node: 63,
            ..w.initial()
        }));
    }

    #[test]
    fn a_scaled_campaign_fills_many_slots() {
        let workload = crate::Workload {
            config: crate::worlds::World::Graph(config(1 << 16, 64)),
            broken: false,
            scale: Some(crate::Scale {
                archive_entries: 1 << 16,
                ..crate::Scale::default()
            }),
        };
        let report =
            crate::run_scaled(&workload, crate::test_seed(), 20_000, &mut std::io::sink()).unwrap();
        assert!(report["live_entries"].as_u64().unwrap() > 5_000);
    }
}
