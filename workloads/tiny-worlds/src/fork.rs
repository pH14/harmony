// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const SHARED: u8 = 0;
const DEAD: u8 = 1;
const LIVE: u8 = 2;
const LIVE_PLACES: u16 = 64;
const ROOM_PLACES: u16 = 128;
const ROOMS_PER_POSITION: u16 = 17;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub fork: u8,
    pub dead_len: u8,
    pub rooms: u8,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub route: u8,
    pub position: u8,
    pub room: u8,
    pub run: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=31).contains(&self.length)
            || self.fork >= self.length
            || self.dead_len == 0
            || self.fork + self.dead_len >= self.length
            || self.rooms > 16
        {
            return Err(
                "fork needs length 2..=31, fork + dead_len < length, dead_len >= 1 and rooms <= 16"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn action(&self, position: u8) -> u8 {
        crate::pattern_action(self.pattern, position % 32)
    }
    pub fn live(&self, s: State) -> bool {
        s.route == LIVE
    }
    pub fn tip(&self) -> u8 {
        self.fork + self.dead_len
    }
    pub fn initial(&self) -> State {
        State {
            route: SHARED,
            position: 0,
            room: 0,
            run: 0,
            goal: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.goal == (s.route == LIVE && s.position == self.length)
            && match s.route {
                SHARED => s.position <= self.fork && s.room == 0 && s.run == 0,
                DEAD => {
                    s.position > self.fork
                        && s.position <= self.tip()
                        && s.room <= self.rooms
                        && s.run <= s.room
                }
                LIVE => {
                    s.position > self.fork
                        && s.position <= self.length
                        && s.room == 0
                        && s.run <= u8::from(s.position < self.length)
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
        let right = action == self.action(s.position);
        let enter = State {
            position: s.position + 1,
            room: 0,
            run: 0,
            ..s
        };
        match s.route {
            SHARED if s.position < self.fork => {
                if right {
                    enter
                } else {
                    self.initial()
                }
            }
            SHARED if right => State {
                route: LIVE,
                ..enter
            },
            SHARED if action == self.advance(s.position) => State {
                route: DEAD,
                ..enter
            },
            DEAD if right => self.initial(),
            DEAD if action == self.advance(s.position) && s.position < self.tip() => enter,
            DEAD => self.explore(s, action),
            _ if !right => self.initial(),
            _ if s.run == 0 => State { run: 1, ..s },
            _ => State {
                goal: enter.position == self.length,
                ..enter
            },
        }
    }
    pub fn advance(&self, position: u8) -> u8 {
        (self.action(position) + 1) % 4
    }
    pub fn explore_action(&self, s: State) -> u8 {
        let bit = self.action(s.position + s.room + s.run) & 1;
        (self.action(s.position) + 2 + bit) % 4
    }
    fn explore(&self, s: State, action: u8) -> State {
        if s.room == self.rooms || action != self.explore_action(s) {
            return State { run: 0, ..s };
        }
        if s.run == s.room {
            State {
                room: s.room + 1,
                run: 0,
                ..s
            }
        } else {
            State {
                run: s.run + 1,
                ..s
            }
        }
    }
    pub fn key(&self, s: State, broken: bool) -> Key {
        let place = match s.route {
            LIVE => LIVE_PLACES + u16::from(s.position),
            _ if s.room > 0 && !broken => {
                ROOM_PLACES + u16::from(s.position) * ROOMS_PER_POSITION + u16::from(s.room)
            }
            _ => u16::from(s.position),
        };
        Key {
            stock: 0,
            place,
            context: 0,
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
    pub fn dead_route_is_dead(&self) -> Result<bool, String> {
        self.validate()?;
        let dead = State {
            route: DEAD,
            position: self.fork + 1,
            ..self.initial()
        };
        let reached = crate::reachable(
            dead,
            |s| self.goal(s),
            |s, a| match self.step(s, a) {
                next if next.route == DEAD => next,
                _ => s,
            },
        )?;
        Ok(!reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            length: 8,
            fork: 2,
            dead_len: 3,
            rooms: 3,
            pattern: 0b10_01_00_11_10_01_00_11,
        }
    }

    fn tape(w: &Config, actions: &[u8]) -> State {
        actions.iter().fold(w.initial(), |s, &a| w.step(s, a))
    }

    fn dead_tip(w: &Config) -> State {
        let mut s = tape(w, &[w.action(0), w.action(1), w.advance(2)]);
        for p in 3..w.tip() {
            s = w.step(s, w.advance(p));
        }
        s
    }

    #[test]
    fn the_live_route_needs_each_pattern_action_twice_after_the_fork() {
        let w = config();
        let fork = tape(&w, &[w.action(0), w.action(1)]);
        assert_eq!((fork.route, fork.position), (SHARED, 2));
        let mut s = w.step(fork, w.action(2));
        assert_eq!((s.route, s.position), (LIVE, 3));
        assert_eq!(w.key(s, false).tier, 3);
        for p in 3..w.length {
            assert_eq!(w.step(s, w.advance(p)), w.initial());
            let half = w.step(s, w.action(p));
            assert_eq!((half.position, half.run), (p, 1));
            assert_eq!(w.key(half, false), w.key(s, false));
            s = w.step(half, w.action(p));
            assert_eq!((s.position, s.run), (p + 1, 0));
        }
        assert!(w.goal(s));
        assert!(w.reachable().unwrap());
    }

    #[test]
    fn the_advance_action_at_the_fork_enters_the_dead_route_which_never_reaches_the_goal() {
        let w = config();
        let fork = tape(&w, &[w.action(0), w.action(1)]);
        let dead = w.step(fork, w.advance(2));
        assert_eq!((dead.route, dead.position), (DEAD, 3));
        assert_eq!(w.step(dead, w.action(3)), w.initial());
        let tip = dead_tip(&w);
        assert_eq!((tip.route, tip.position), (DEAD, w.tip()));
        assert_eq!(w.step(tip, w.advance(w.tip())), tip);
        assert_eq!(w.key(tip, false).tier, u16::from(w.tip()));
        assert!(w.dead_route_is_dead().unwrap());
    }

    #[test]
    fn each_room_on_the_dead_route_needs_one_more_consecutive_explore_action() {
        let w = config();
        let mut s = dead_tip(&w);
        for room in 0..w.rooms {
            for run in 0..room {
                s = w.step(s, w.explore_action(s));
                assert_eq!((s.room, s.run), (room, run + 1));
            }
            let other = (0..4)
                .find(|&a| {
                    a != w.explore_action(s)
                        && a != w.action(s.position)
                        && a != w.advance(s.position)
                })
                .expect("a wrong explore action");
            assert_eq!(w.step(s, other).run, 0);
            s = w.step(s, w.explore_action(s));
            assert_eq!((s.room, s.run), (room + 1, 0));
            let place = ROOM_PLACES + u16::from(w.tip()) * ROOMS_PER_POSITION + u16::from(room + 1);
            assert_eq!(w.key(s, false).place, place);
            assert_eq!(w.key(s, true).place, u16::from(w.tip()));
            assert_eq!(w.key(s, false).tier, u16::from(w.tip()));
        }
        assert_eq!(w.step(s, w.explore_action(s)), s);
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { length: 1, ..w }.validate().is_err());
        assert!(Config { dead_len: 0, ..w }.validate().is_err());
        assert!(Config { dead_len: 6, ..w }.validate().is_err());
        assert!(Config { rooms: 17, ..w }.validate().is_err());
        assert!(!w.state_is_bounded(State {
            run: 1,
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
                config: crate::worlds::World::Fork(config()),
                broken,
                scale: None,
            };
            let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
            assert_eq!(report["verified"], true);
        }
    }
}
