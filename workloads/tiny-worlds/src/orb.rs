// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const LIVE: u8 = 0;
const FLOOR: u8 = 1;
const FLOOR_PLACES: u16 = 64;
const ROOMS_PER_POSITION: u16 = 17;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub first: u8,
    pub orbs: u8,
    pub gap: u8,
    pub dead_len: u8,
    pub rooms: u8,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub route: u8,
    pub position: u8,
    pub held: bool,
    pub room: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.first == 0
            || self.orbs == 0
            || !(2..=8).contains(&self.gap)
            || self.dead_len == 0
            || self.rooms > 16
            || u16::from(self.first)
                + u16::from(self.orbs - 1) * u16::from(self.gap)
                + u16::from(self.dead_len)
                >= u16::from(self.length)
            || self.length > 63
        {
            return Err(
                "orb needs first, orbs and dead_len >= 1, gap 2..=8, rooms <= 16, and the last orb plus dead_len below length <= 63"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn last_orb(&self) -> u8 {
        self.first + (self.orbs - 1) * self.gap
    }
    pub fn tip(&self) -> u8 {
        self.last_orb() + self.dead_len
    }
    pub fn orb(&self, position: u8) -> bool {
        position >= self.first
            && position <= self.last_orb()
            && (position - self.first).is_multiple_of(self.gap)
    }
    pub fn live(&self, s: State) -> bool {
        s.route == LIVE
    }
    pub fn explore_action(&self, s: State) -> u8 {
        crate::pattern_action(self.pattern, (s.position + s.room) % 32)
    }
    pub fn initial(&self) -> State {
        State {
            route: LIVE,
            position: 0,
            held: false,
            room: 0,
            goal: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.goal == (s.route == LIVE && s.position == self.length)
            && match s.route {
                LIVE => s.position <= self.length && s.room == 0,
                FLOOR => {
                    self.first <= s.position && s.position <= self.tip() && s.room <= self.rooms
                }
                _ => false,
            }
    }
    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }
    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(s) || s.goal {
            return s;
        }
        let press = action & 1 == 1;
        let position = s.position + 1;
        match s.route {
            LIVE if !self.orb(position) || (press && !s.held) => State {
                position,
                held: press,
                goal: position == self.length,
                ..s
            },
            LIVE => State {
                route: FLOOR,
                position,
                held: press,
                ..s
            },
            _ if s.position == self.tip() => self.initial(),
            _ => State {
                position,
                held: press,
                room: if action == self.explore_action(s) {
                    (s.room + 1).min(self.rooms)
                } else {
                    s.room
                },
                ..s
            },
        }
    }
    pub fn key(&self, s: State, broken: bool) -> Key {
        let place = match s.route {
            LIVE => u16::from(s.position),
            _ => FLOOR_PLACES + u16::from(s.position) * ROOMS_PER_POSITION + u16::from(s.room),
        };
        Key {
            stock: 0,
            place,
            context: u16::from(s.held && !broken),
            charge: 0,
            health: 0,
            goal: s.goal,
            tier: u16::from(s.position),
        }
    }
    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            length: 16,
            first: 3,
            orbs: 3,
            gap: 2,
            dead_len: 4,
            rooms: 3,
            pattern: 0b10_01_00_11_10_01_00_11_10_01_00_11,
        }
    }

    fn walk(w: &Config, s: State, actions: &[u8]) -> State {
        actions.iter().fold(s, |s, &a| w.step(s, a))
    }

    #[test]
    fn each_orb_needs_a_press_after_a_release() {
        let w = config();
        let before = walk(&w, w.initial(), &[1, 1]);
        assert_eq!((before.position, before.held), (2, true));
        let missed = w.step(before, 1);
        assert_eq!((missed.route, missed.position), (FLOOR, 3));
        let released = walk(&w, w.initial(), &[1, 0]);
        let fired = w.step(released, 1);
        assert_eq!((fired.route, fired.position, fired.held), (LIVE, 3, true));
        assert_eq!(w.step(w.step(fired, 1), 1).route, FLOOR);
        let s = walk(&w, fired, &[0, 1, 2, 3]);
        assert_eq!((s.route, s.position), (LIVE, 7));
        let s = walk(&w, s, &[0; 9]);
        assert!(w.goal(s));
        assert!(w.reachable().unwrap());
    }

    #[test]
    fn the_hidden_key_ties_a_held_and_a_released_arrival() {
        let w = config();
        let held = walk(&w, w.initial(), &[1, 1]);
        let released = walk(&w, w.initial(), &[1, 0]);
        assert_eq!(w.key(held, true), w.key(released, true));
        assert_ne!(w.key(held, false), w.key(released, false));
        assert_eq!(w.key(held, false).place, w.key(released, false).place);
    }

    #[test]
    fn the_floor_runs_past_the_last_orb_opens_rooms_and_dies_at_its_tip() {
        let w = config();
        let mut s = walk(&w, w.initial(), &[1, 1, 1]);
        assert_eq!(s.route, FLOOR);
        let mut rooms = 0;
        while s.position < w.tip() {
            let action = w.explore_action(s);
            s = w.step(s, action);
            rooms += 1;
            assert_eq!(s.room, rooms.min(w.rooms));
            assert_eq!(w.key(s, false).tier, u16::from(s.position));
        }
        assert!(s.position > w.last_orb());
        assert_eq!(w.step(s, 0), w.initial());
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { gap: 1, ..w }.validate().is_err());
        assert!(Config { orbs: 0, ..w }.validate().is_err());
        assert!(Config { dead_len: 9, ..w }.validate().is_err());
        assert!(Config { rooms: 17, ..w }.validate().is_err());
        assert!(!w.state_is_bounded(State {
            route: FLOOR,
            ..w.initial()
        }));
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
    }

    #[test]
    fn campaign_replays_in_both_arms() {
        for broken in [false, true] {
            let workload = crate::Workload {
                config: crate::worlds::World::Orb(config()),
                broken,
                scale: None,
            };
            let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
            assert_eq!(report["verified"], true);
        }
    }
}
