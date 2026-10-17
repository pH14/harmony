// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const SHARED: u8 = 0;
const DEAD: u8 = 1;
const LIVE: u8 = 2;
const LIVE_PLACES: u16 = 64;
const ROOM_PLACES: u16 = 128;
const ROOMS_PER_POSITION: u16 = 256;
const MAX_RUN_ON: u16 = 60_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub fork: u8,
    pub dead_len: u8,
    pub rooms: u8,
    #[serde(default)]
    pub room_run: u8,
    #[serde(default)]
    pub entry_run: u8,
    #[serde(default)]
    pub step_run: u8,
    #[serde(default)]
    pub goal_at_fork: bool,
    #[serde(default)]
    pub shared_rooms: u8,
    #[serde(default)]
    pub grades: u8,
    #[serde(default)]
    pub room_ramp: u8,
    #[serde(default)]
    pub run_on: u16,
    #[serde(default)]
    pub run_on_run: u8,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub route: u8,
    pub position: u8,
    pub room: u8,
    pub run: u8,
    pub explore: u8,
    pub grade: u8,
    #[serde(default)]
    pub distance: u16,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        let past_tip = if self.goal_at_fork {
            self.length == self.fork + 1 && self.tip() <= 63
        } else {
            self.tip() < self.length
        };
        if !(2..=63).contains(&self.length)
            || self.fork >= self.length
            || self.dead_len == 0
            || !past_tip
            || (self.room_run == 0 && self.rooms.max(self.shared_rooms) > 16)
            || self.room_run > 8
            || self.grades > 15
            || self.room_ramp > 16
            || self.entry_run > 8
            || self.step_run > 8
            || self.run_on > MAX_RUN_ON
            || self.run_on_run > 8
        {
            return Err(
                "fork needs length 2..=63, fork + dead_len < length, or length = fork + 1 and a tip \
                 of at most 63 with goal_at_fork, dead_len >= 1, room_run, entry_run and step_run \
                 <= 8, grades <= 15, room_ramp <= 16, run_on <= 60000, run_on_run <= 8, and rooms and shared_rooms <= 16 unless room_run is set"
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
        self.fork.saturating_add(self.dead_len)
    }
    pub fn step_presses(&self) -> u8 {
        self.step_run.max(1)
    }
    pub fn live_presses(&self) -> u8 {
        self.step_run.max(2)
    }
    pub fn initial(&self) -> State {
        State {
            route: SHARED,
            position: 0,
            room: 0,
            run: 0,
            explore: 0,
            grade: 0,
            distance: 0,
            goal: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.goal == (s.route == LIVE && s.position == self.length)
            && s.grade <= self.grades
            && (s.run == 0 || s.explore == 0)
            && s.explore < self.room_explores(s.position, s.room)
            && (s.distance == 0
                || (s.route == DEAD && s.position == self.tip() && s.distance <= self.run_on))
            && match s.route {
                SHARED => {
                    s.position <= self.fork
                        && (s.room == 0 || s.position < self.fork)
                        && s.room <= self.shared_rooms
                        && (s.explore == 0 || s.position < self.fork)
                        && s.run
                            < if s.position == self.fork {
                                self.entry_presses()
                            } else {
                                self.step_presses()
                            }
                }
                DEAD => {
                    s.position > self.fork
                        && s.position <= self.tip()
                        && s.room <= self.rooms
                        && (s.run == 0
                            || (s.position == self.tip() && s.run < self.run_on_presses()))
                }
                LIVE => {
                    s.position > self.fork
                        && s.position <= self.length
                        && s.room == 0
                        && s.explore == 0
                        && (s.run < self.live_presses() || s.position == self.length)
                        && (s.run == 0 || s.position < self.length)
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
        let next = self.transition(s, action);
        if next == self.initial()
            || (next.route, next.position, next.room, next.distance)
                == (s.route, s.position, s.room, s.distance)
        {
            return next;
        }
        State {
            grade: self.grade(s, action),
            ..next
        }
    }
    fn grade(&self, s: State, action: u8) -> u8 {
        if self.grades == 0 {
            return 0;
        }
        let mut h = (u32::from(s.position) * 0x9e37)
            ^ (u32::from(s.room) * 0x85eb)
            ^ (u32::from(s.route) * 0xc2b2)
            ^ (u32::from(s.run) * 0x27d4)
            ^ (u32::from(s.explore) * 0x1656)
            ^ (u32::from(s.grade) * 0x6a09)
            ^ (u32::from(s.distance) * 0x3c6e)
            ^ (u32::from(action) * 0xbb67);
        h ^= h >> 7;
        h = h.wrapping_mul(0x2545_f491);
        h ^= h >> 13;
        (h % (u32::from(self.grades) + 1)) as u8
    }
    fn transition(&self, s: State, action: u8) -> State {
        let right = action == self.action(s.position);
        let enter = State {
            position: s.position + 1,
            room: 0,
            run: 0,
            explore: 0,
            ..s
        };
        match s.route {
            SHARED
                if s.position < self.fork
                    && self.shared_rooms > 0
                    && action == self.explore_action(s) =>
            {
                self.explore(s, action, self.shared_rooms)
            }
            SHARED if s.position < self.fork && !right => self.initial(),
            SHARED if s.position < self.fork && s.run + 1 < self.step_presses() => State {
                run: s.run + 1,
                explore: 0,
                ..s
            },
            SHARED if s.position < self.fork => enter,
            SHARED if right && s.run + 1 < self.entry_presses() => State {
                run: s.run + 1,
                ..s
            },
            SHARED if right => State {
                route: LIVE,
                goal: self.goal_at_fork,
                ..enter
            },
            SHARED if action == self.advance(s.position) => State {
                route: DEAD,
                ..enter
            },
            DEAD if s.position == self.tip()
                && s.distance < self.run_on
                && action == self.run_on_action(s.distance) =>
            {
                if s.run + 1 < self.run_on_presses() {
                    State {
                        run: s.run + 1,
                        explore: 0,
                        ..s
                    }
                } else {
                    State {
                        room: 0,
                        run: 0,
                        explore: 0,
                        distance: s.distance + 1,
                        ..s
                    }
                }
            }
            DEAD if right && (self.run_on == 0 || s.position < self.tip()) => self.initial(),
            DEAD if s.run > 0 => State { run: 0, ..s },
            DEAD if action == self.advance(s.position) && s.position < self.tip() => enter,
            DEAD => self.explore(s, action, self.rooms),
            _ if !right => self.initial(),
            _ if s.run + 1 < self.live_presses() => State {
                run: s.run + 1,
                ..s
            },
            _ => State {
                goal: enter.position == self.length,
                ..enter
            },
        }
    }
    pub fn advance(&self, position: u8) -> u8 {
        (self.action(position) + 1) % 4
    }
    pub fn run_on_presses(&self) -> u8 {
        self.run_on_run.max(1)
    }
    pub fn run_on_action(&self, distance: u16) -> u8 {
        self.advance(self.tip().wrapping_add((distance % 32) as u8))
    }
    pub fn explore_action(&self, s: State) -> u8 {
        let bit = self.action(s.position.wrapping_add(s.room).wrapping_add(s.explore)) & 1;
        (self.action(s.position) + 2 + bit) % 4
    }
    pub fn entry_presses(&self) -> u8 {
        self.entry_run.max(1)
    }
    pub fn room_explores(&self, position: u8, room: u8) -> u8 {
        if self.room_run == 0 {
            return room.saturating_add(1);
        }
        match self
            .tip()
            .saturating_sub(position)
            .checked_div(self.room_ramp)
        {
            None => self.room_run,
            Some(below_tip) => self.room_run.saturating_sub(below_tip).max(1),
        }
    }
    fn explore(&self, s: State, action: u8, rooms: u8) -> State {
        if s.room == rooms || action != self.explore_action(s) {
            return State { explore: 0, ..s };
        }
        if s.explore + 1 == self.room_explores(s.position, s.room) {
            State {
                room: s.room + 1,
                run: 0,
                explore: 0,
                ..s
            }
        } else {
            State {
                run: 0,
                explore: s.explore + 1,
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
            health: s.grade,
            goal: s.goal,
            tier: u16::from(s.position) + s.distance,
        }
    }
    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(
            self.initial(),
            |s| self.goal(s),
            |s, a| State {
                grade: 0,
                ..self.step(s, a)
            },
        )
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
                next if next.route == DEAD => State { grade: 0, ..next },
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
            room_run: 0,
            entry_run: 0,
            step_run: 0,
            goal_at_fork: false,
            shared_rooms: 0,
            grades: 0,
            room_ramp: 0,
            run_on: 0,
            run_on_run: 0,
            pattern: 0b10_01_00_11_10_01_00_11,
        }
    }

    fn tape(w: &Config, actions: &[u8]) -> State {
        tape_from(w, w.initial(), actions)
    }

    fn tape_from(w: &Config, start: State, actions: &[u8]) -> State {
        actions.iter().fold(start, |s, &a| w.step(s, a))
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
                assert_eq!((s.room, s.explore), (room, run + 1));
            }
            let other = (0..4)
                .find(|&a| {
                    a != w.explore_action(s)
                        && a != w.action(s.position)
                        && a != w.advance(s.position)
                })
                .expect("a wrong explore action");
            assert_eq!(w.step(s, other).explore, 0);
            s = w.step(s, w.explore_action(s));
            assert_eq!((s.room, s.explore), (room + 1, 0));
            let place = ROOM_PLACES + u16::from(w.tip()) * ROOMS_PER_POSITION + u16::from(room + 1);
            assert_eq!(w.key(s, false).place, place);
            assert_eq!(w.key(s, true).place, u16::from(w.tip()));
            assert_eq!(w.key(s, false).tier, u16::from(w.tip()));
        }
        assert_eq!(w.step(s, w.explore_action(s)), s);
    }

    #[test]
    fn a_room_run_sets_every_rooms_explore_count_and_an_entry_run_hides_presses_at_the_fork() {
        let w = Config {
            rooms: 40,
            room_run: 3,
            entry_run: 3,
            ..config()
        };
        let mut s = dead_tip(&w);
        for room in 0..w.rooms {
            for run in 0..2 {
                s = w.step(s, w.explore_action(s));
                assert_eq!((s.room, s.explore), (room, run + 1));
            }
            s = w.step(s, w.explore_action(s));
            assert_eq!((s.room, s.explore), (room + 1, 0));
            assert!(w.state_is_bounded(s));
        }
        assert_eq!(w.step(s, w.explore_action(s)), s);
        let fork = tape(&w, &[w.action(0), w.action(1)]);
        let mut s = fork;
        for run in 1..3 {
            s = w.step(s, w.action(2));
            assert_eq!((s.route, s.position, s.run), (SHARED, 2, run));
            assert_eq!(w.key(s, false), w.key(fork, false));
        }
        let entered = w.step(s, w.action(2));
        assert_eq!((entered.route, entered.position), (LIVE, 3));
        let dead = w.step(s, w.advance(2));
        assert_eq!((dead.route, dead.position, dead.run), (DEAD, 3, 0));
        assert_eq!(w.step(s, (w.action(2) + 2) % 4), w.initial());
        assert!(w.reachable().unwrap());
        assert!(w.dead_route_is_dead().unwrap());
    }

    #[test]
    fn a_step_run_hides_presses_at_every_shared_and_live_position() {
        let w = Config {
            step_run: 3,
            ..config()
        };
        let mut s = w.initial();
        for run in 1..3 {
            s = w.step(s, w.action(0));
            assert_eq!((s.position, s.run), (0, run));
            assert_eq!(w.key(s, false), w.key(w.initial(), false));
        }
        assert_eq!(w.step(s, w.advance(0)), w.initial());
        s = w.step(s, w.action(0));
        assert_eq!((s.position, s.run), (1, 0));
        let fork = tape_from(&w, tape(&w, &[w.action(0); 3]), &[w.action(1); 3]);
        assert_eq!((fork.route, fork.position), (SHARED, 2));
        let mut live = w.step(fork, w.action(2));
        assert_eq!(live.route, LIVE);
        while !live.goal {
            let before = live;
            for _ in 1..3 {
                live = w.step(live, w.action(live.position));
                assert_eq!(live.position, before.position);
            }
            live = w.step(live, w.action(live.position));
            assert_eq!(live.position, before.position + 1);
        }
        assert!(w.goal(live));
        assert!(w.reachable().unwrap());
    }

    #[test]
    fn a_goal_at_the_fork_ends_the_campaign_on_entering_the_live_route() {
        let w = Config {
            length: 3,
            dead_len: 30,
            entry_run: 4,
            goal_at_fork: true,
            ..config()
        };
        assert!(w.validate().is_ok());
        let mut s = tape(&w, &[w.action(0), w.action(1)]);
        for _ in 1..4 {
            s = w.step(s, w.action(2));
            assert!(!s.goal);
        }
        s = w.step(s, w.action(2));
        assert!(w.goal(s));
        assert_eq!(w.key(s, false).tier, 3);
        let tip = dead_tip(&w);
        assert_eq!(w.key(tip, false).tier, 32);
        assert!(w.reachable().unwrap());
        assert!(w.dead_route_is_dead().unwrap());
        assert!(Config { length: 4, ..w }.validate().is_err());
    }

    #[test]
    fn a_run_on_keeps_raising_the_tier_past_the_dead_tip_without_reaching_the_goal() {
        let w = Config {
            length: 3,
            dead_len: 30,
            entry_run: 4,
            goal_at_fork: true,
            run_on: 200,
            run_on_run: 2,
            ..config()
        };
        assert!(w.validate().is_ok());
        let mut s = dead_tip(&w);
        let pressed = w.step(s, w.run_on_action(0));
        assert_eq!((pressed.run, pressed.distance), (1, 0));
        let wrong = (0..4)
            .find(|&a| a != w.run_on_action(0) && a != w.action(w.tip()))
            .unwrap();
        assert_eq!(w.step(pressed, wrong).run, 0);
        if w.action(w.tip()) != w.run_on_action(0) {
            assert_eq!(w.step(s, w.action(w.tip())).route, DEAD);
            assert_eq!(
                Config { run_on: 0, ..w }.step(s, w.action(w.tip())),
                w.initial()
            );
        }
        for distance in 1..=200 {
            s = w.step(s, w.run_on_action(s.distance));
            s = w.step(s, w.run_on_action(s.distance));
            assert_eq!(s.distance, distance);
            assert_eq!(w.key(s, false).tier, 32 + distance);
        }
        assert_eq!(w.step(s, w.run_on_action(s.distance)), s);
        assert!(w.reachable().unwrap());
        assert!(w.dead_route_is_dead().unwrap());
        assert!(!w.state_is_bounded(State {
            distance: 1,
            ..w.initial()
        }));
        assert!(
            Config {
                run_on: 60_001,
                ..w
            }
            .validate()
            .is_err()
        );
        assert!(Config { run_on_run: 9, ..w }.validate().is_err());
    }

    #[test]
    fn shared_rooms_open_beside_the_corridor_and_grades_vary_the_preference_by_route() {
        let w = Config {
            shared_rooms: 4,
            room_run: 2,
            grades: 7,
            ..config()
        };
        let start = w.initial();
        let mut s = w.step(start, w.explore_action(start));
        assert_eq!((s.position, s.room, s.explore), (0, 0, 1));
        s = w.step(s, w.explore_action(s));
        assert_eq!((s.position, s.room, s.explore), (0, 1, 0));
        assert_eq!(w.key(s, false).place, ROOM_PLACES + u16::from(s.room));
        assert_eq!(w.key(s, false).tier, 0);
        assert_eq!(w.key(s, false).health, s.grade);
        let wrong = (0..4)
            .find(|&a| a != w.action(0) && a != w.explore_action(s))
            .unwrap();
        assert_eq!(w.step(s, wrong), w.initial());
        let ahead = w.step(s, w.action(0));
        assert_eq!((ahead.position, ahead.room), (1, 0));
        let grades: std::collections::BTreeSet<u8> = (0..4)
            .flat_map(|a| (0..4).map(move |b| (a, b)))
            .map(|(a, b)| w.step(w.step(start, a), b).grade)
            .collect();
        assert!(grades.len() > 1, "{grades:?}");
        assert!(grades.iter().all(|&g| g <= w.grades));
        assert!(w.reachable().unwrap());
        assert!(Config { grades: 16, ..w }.validate().is_err());
        let ramp = Config {
            room_run: 5,
            room_ramp: 2,
            rooms: 40,
            ..w
        };
        assert_eq!(ramp.room_explores(ramp.tip(), 0), 5);
        assert_eq!(ramp.room_explores(ramp.tip() - 1, 0), 5);
        assert_eq!(ramp.room_explores(ramp.tip() - 2, 9), 4);
        assert_eq!(ramp.room_explores(0, 3), 3);
        assert!(ramp.reachable().unwrap());
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { length: 1, ..w }.validate().is_err());
        assert!(Config { dead_len: 0, ..w }.validate().is_err());
        assert!(Config { dead_len: 6, ..w }.validate().is_err());
        assert!(Config { rooms: 17, ..w }.validate().is_err());
        assert!(
            Config {
                rooms: 255,
                room_run: 4,
                ..w
            }
            .validate()
            .is_ok()
        );
        assert!(Config { room_run: 9, ..w }.validate().is_err());
        assert!(Config { entry_run: 9, ..w }.validate().is_err());
        assert!(Config { step_run: 9, ..w }.validate().is_err());
        assert!(Config { length: 63, ..w }.validate().is_ok());
        assert!(Config { length: 64, ..w }.validate().is_err());
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
            let runs = report["selector"]["tier_runs"].as_object().unwrap();
            let most = |field: &str| runs.values().filter_map(|r| r[field].as_u64()).max();
            assert!(runs.len() > 1, "{runs:?}");
            assert!(most("dry") > Some(0), "{runs:?}");
            assert!(most("cells") > Some(1), "{runs:?}");
        }
    }
}
