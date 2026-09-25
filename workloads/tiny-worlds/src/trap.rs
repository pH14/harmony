// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const ENTER: u8 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub pattern: u32,
    pub trap_len: u8,
    pub rooms: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub trap: u8,
    pub room: u8,
    pub item: bool,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.length)
            || !(1..=8).contains(&self.trap_len)
            || !(1..=16).contains(&self.rooms)
        {
            return Err("trap length 1..=16, trap_len 1..=8 and rooms 1..=16 are required".into());
        }
        if self.length < 16 && self.pattern >> (2 * u32::from(self.length)) != 0 {
            return Err("trap pattern has bits beyond the corridor length".into());
        }
        if self.route_action(0) == ENTER {
            return Err("the corridor's first action must differ from the trap entry".into());
        }
        Ok(())
    }
    pub fn route_action(&self, position: u8) -> u8 {
        crate::pattern_action(self.pattern.into(), position)
    }
    pub fn initial(&self) -> State {
        State {
            position: 0,
            trap: 0,
            room: 0,
            item: false,
            goal: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        if s.item {
            s.position == 0 && s.trap == self.trap_len && s.room < self.rooms && !s.goal
        } else if s.trap > 0 {
            s.position == 0 && s.trap < self.trap_len && s.room == 0 && !s.goal
        } else {
            s.position <= self.length && s.room == 0 && s.goal == (s.position == self.length)
        }
    }
    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }
    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(s) || s.goal {
            return s;
        }
        if s.item {
            return State {
                room: (s.room + action + 1) % self.rooms,
                ..s
            };
        }
        if s.trap > 0 || (s.position == 0 && action == ENTER) {
            if action != ENTER {
                return s;
            }
            let trap = s.trap + 1;
            return State {
                trap,
                item: trap == self.trap_len,
                ..s
            };
        }
        if action == self.route_action(s.position) {
            let position = s.position + 1;
            State {
                position,
                goal: position == self.length,
                ..s
            }
        } else {
            self.initial()
        }
    }
    pub fn key(&self, s: State, broken: bool) -> Key {
        let place = if s.item {
            64 + u16::from(s.room)
        } else if s.trap > 0 {
            32 + u16::from(s.trap)
        } else {
            u16::from(s.position)
        };
        Key {
            stock: 0,
            place,
            context: 0,
            charge: 0,
            health: 0,
            goal: s.goal,
            tier: u16::from(s.item && !broken),
        }
    }
    fn search(&self, from: State) -> Result<bool, String> {
        crate::reachable(from, |s| self.goal(s), |s, a| self.step(s, a))
    }
    pub fn reachable(&self) -> Result<bool, String> {
        if !self.trap_is_dead()? {
            return Err("the trap reaches the goal".into());
        }
        self.search(self.initial())
    }
    pub fn trap_is_dead(&self) -> Result<bool, String> {
        self.validate()?;
        for room in 0..self.rooms {
            let item = State {
                position: 0,
                trap: self.trap_len,
                room,
                item: true,
                goal: false,
            };
            if self.search(item)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            length: 6,
            pattern: 0b01_10_00_01_10_00,
            trap_len: 2,
            rooms: 4,
        }
    }

    fn tape(w: &Config, actions: &[u8]) -> State {
        actions.iter().fold(w.initial(), |s, &a| w.step(s, a))
    }

    #[test]
    fn goal_is_reachable_and_every_item_state_is_dead() {
        for rooms in 1..=16 {
            for trap_len in [1, 3, 8] {
                let w = Config {
                    rooms,
                    trap_len,
                    ..config()
                };
                assert!(w.reachable().unwrap());
                assert!(w.trap_is_dead().unwrap());
            }
        }
        let w = config();
        let route: Vec<_> = (0..w.length).map(|i| w.route_action(i)).collect();
        assert!(w.goal(tape(&w, &route)));
    }

    #[test]
    fn item_raises_the_tier_unless_the_control_hides_it() {
        let w = config();
        let item = tape(&w, &[ENTER, ENTER]);
        assert!(item.item);
        assert_eq!(w.key(item, false).tier, 1);
        assert_eq!(w.key(item, true).tier, 0);
        assert_eq!(w.key(tape(&w, &[ENTER]), false).tier, 0);
        let rooms: std::collections::BTreeSet<_> = (0..4)
            .map(|a| w.key(w.step(item, a), false).place)
            .collect();
        assert_eq!(rooms.len(), 4);
        for a in 0..4 {
            assert!(!w.step(item, a).goal && w.step(item, a).item);
        }
    }

    #[test]
    fn wrong_corridor_actions_return_to_the_hub_and_trap_waits_on_entry() {
        let w = config();
        let one = w.step(w.initial(), w.route_action(0));
        assert_eq!(one.position, 1);
        assert_eq!(w.step(one, (w.route_action(1) + 1) % 4), w.initial());
        let entered = w.step(w.initial(), ENTER);
        assert_eq!(w.step(entered, 0), entered);
        assert!(!w.state_is_bounded(State {
            item: true,
            trap: 1,
            ..entered
        }));
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { length: 0, ..w }.validate().is_err());
        assert!(Config { rooms: 17, ..w }.validate().is_err());
        assert!(Config { trap_len: 9, ..w }.validate().is_err());
        assert!(Config { pattern: 3, ..w }.validate().is_err());
        assert!(
            Config {
                pattern: 1 << 12,
                ..w
            }
            .validate()
            .is_err()
        );
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
    }

    #[test]
    fn campaign_replays_in_both_arms() {
        for broken in [false, true] {
            let workload = crate::Workload {
                config: crate::worlds::World::Trap(config()),
                broken,
            };
            let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
            assert_eq!(report["verified"], true);
        }
    }
}
