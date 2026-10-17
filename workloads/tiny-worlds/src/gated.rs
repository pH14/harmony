// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const LEGS: u8 = 0;
const BOSS: u8 = 1;
const CORRIDOR: u8 = 2;
const BOSS_PLACES: u16 = 128;
const CORRIDOR_PLACES: u16 = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub items: u8,
    pub leg: u8,
    pub walk: u8,
    pub barrier: u8,
    pub step_run: u8,
    pub fight: u8,
    pub fight_odds: u8,
    pub health: u8,
    pub hit_odds: u8,
    #[serde(default)]
    pub shortcut: u8,
    #[serde(default)]
    pub shortcut_run: u8,
    pub dense: bool,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub zone: u8,
    pub position: u8,
    pub items: u8,
    pub hits: u8,
    pub killed: bool,
    pub run: u8,
    pub aim: u8,
    pub side: u8,
    pub health: u8,
    pub dead: bool,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=6).contains(&self.items)
            || !(1..=16).contains(&self.leg)
            || !(2..=48).contains(&self.walk)
            || !(1..self.walk).contains(&self.barrier)
            || !(1..=8).contains(&self.step_run)
            || !(1..=16).contains(&self.fight)
            || !(1..=16).contains(&self.fight_odds)
            || !(1..=15).contains(&self.health)
            || self.hit_odds > 16
            || self.shortcut >= self.items
            || self.shortcut_run > 8
        {
            return Err(
                "gated needs items 1..=6, leg 1..=16, walk 2..=48, barrier 1..walk, step_run 1..=8, \
                 fight 1..=16, fight_odds 1..=16, health 1..=15, hit_odds 0..=16, shortcut below items and shortcut_run 0..=8"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn initial(&self) -> State {
        State {
            zone: LEGS,
            position: 0,
            items: 0,
            hits: 0,
            killed: false,
            run: 0,
            aim: 0,
            side: 0,
            health: self.health,
            dead: false,
            goal: false,
        }
    }
    fn legs_end(&self) -> u8 {
        self.items * self.leg
    }
    fn route_position(&self, s: State) -> u8 {
        match s.zone {
            LEGS => s.position,
            BOSS => self.legs_end(),
            _ => self.legs_end() + 1 + s.position,
        }
    }
    pub fn action(&self, s: State) -> u8 {
        crate::pattern_action(self.pattern, self.route_position(s) % 32)
    }
    pub fn shot(&self) -> u8 {
        crate::pattern_action(self.pattern, self.legs_end() % 32)
    }
    pub fn door(&self) -> u8 {
        (self.shot() + 1) % 4
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.run < self.step_run
            && s.aim < self.fight_odds
            && s.side < self.shortcut_presses()
            && (s.side == 0 || self.shortcut_door(s))
            && s.health <= self.health
            && s.items <= self.items
            && s.hits < self.fight
            && s.goal == (s.zone == CORRIDOR && s.position == self.walk)
            && !(s.dead && s.goal)
            && match s.zone {
                LEGS => {
                    s.position < self.legs_end() && s.items == s.position / self.leg && !s.killed
                }
                BOSS => s.position == 0 && s.items == self.items && s.run == 0,
                CORRIDOR => {
                    (s.items == self.items
                        || (s.items == self.shortcut && s.items > 0 && !s.killed))
                        && s.hits == 0
                        && s.position <= self.walk
                        && (s.killed || s.position < self.barrier)
                }
                _ => false,
            }
    }
    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }
    fn hit(&self, s: State, action: u8) -> bool {
        if self.hit_odds == 0 {
            return false;
        }
        let mut h = (u32::from(self.route_position(s)) * 0x9e37)
            ^ (u32::from(s.run) * 0x85eb)
            ^ (u32::from(action) * 0xc2b2)
            ^ (u32::from(s.health) * 0x27d4)
            ^ (u32::from(s.aim) * 0x1656);
        h ^= h >> 7;
        h = h.wrapping_mul(0x2545_f491);
        h ^= h >> 13;
        h % u32::from(self.hit_odds) == 0
    }
    fn hurt(&self, s: State) -> State {
        match s.health {
            0 => State { dead: true, ..s },
            h => State { health: h - 1, ..s },
        }
    }
    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(s) || s.goal || s.dead {
            return s;
        }
        match s.zone {
            BOSS => self.fight_step(s, action),
            _ => self.walk_step(s, action),
        }
    }
    pub fn shortcut_door(&self, s: State) -> bool {
        self.shortcut > 0 && s.zone == LEGS && s.position == self.shortcut * self.leg
    }
    pub fn shortcut_presses(&self) -> u8 {
        self.shortcut_run.max(1)
    }
    pub fn shortcut_action(&self, s: State) -> u8 {
        (self.action(s) + 2) % 4
    }
    fn walk_step(&self, s: State, action: u8) -> State {
        if self.shortcut_door(s) && action == self.shortcut_action(s) {
            if s.side + 1 < self.shortcut_presses() {
                return State {
                    side: s.side + 1,
                    run: 0,
                    ..s
                };
            }
            return State {
                zone: CORRIDOR,
                position: 0,
                run: 0,
                side: 0,
                ..s
            };
        }
        let s = State {
            side: 0,
            aim: (s.aim + action + 1) % self.fight_odds,
            ..s
        };
        if action != self.action(s) {
            let reset = State { run: 0, ..s };
            return if self.hit(s, action) {
                self.hurt(reset)
            } else {
                reset
            };
        }
        if s.run + 1 < self.step_run {
            return State {
                run: s.run + 1,
                ..s
            };
        }
        let s = State { run: 0, ..s };
        if s.zone == CORRIDOR {
            if s.position + 1 == self.barrier && !s.killed {
                return s;
            }
            let position = s.position + 1;
            return State {
                position,
                goal: position == self.walk,
                ..s
            };
        }
        let position = s.position + 1;
        if position == self.legs_end() {
            return State {
                zone: BOSS,
                position: 0,
                items: self.items,
                health: self.health,
                ..s
            };
        }
        let items = position / self.leg;
        State {
            position,
            items,
            health: if items > s.items {
                self.health
            } else {
                s.health
            },
            ..s
        }
    }
    fn fight_step(&self, s: State, action: u8) -> State {
        if action == self.door() {
            return State {
                zone: CORRIDOR,
                position: 0,
                hits: 0,
                aim: 0,
                ..s
            };
        }
        if s.killed || action != self.shot() {
            return State {
                aim: (s.aim + 1) % self.fight_odds,
                ..s
            };
        }
        if s.aim != 0 {
            return self.hurt(State {
                aim: (s.aim + 1) % self.fight_odds,
                ..s
            });
        }
        if s.hits + 1 == self.fight {
            return State {
                hits: 0,
                killed: true,
                health: self.health,
                aim: 1 % self.fight_odds,
                ..s
            };
        }
        State {
            hits: s.hits + 1,
            aim: 1 % self.fight_odds,
            ..s
        }
    }
    pub fn tier(&self, s: State) -> u16 {
        let engaged = u16::from(s.zone == BOSS && s.hits > 0);
        let items = u16::from(s.items);
        if !self.dense {
            return if s.killed {
                2 * items + 2
            } else {
                2 * items + engaged
            };
        }
        let base = u16::from(self.legs_end());
        let position = u16::from(s.position);
        match (s.killed, s.zone) {
            (false, LEGS) => position,
            (false, BOSS) => base + 1 + engaged,
            (false, _) if s.items == self.items => base + 1,
            (false, _) => u16::from(s.items * self.leg),
            (true, BOSS) => base + 3,
            (true, _) => base + 4 + position,
        }
    }
    pub fn key(&self, s: State, broken: bool) -> Key {
        let place = match s.zone {
            LEGS => u16::from(s.position),
            BOSS => BOSS_PLACES + u16::from(s.hits) + 32 * u16::from(s.killed),
            _ => CORRIDOR_PLACES + u16::from(s.position),
        };
        Key {
            stock: 0,
            place,
            context: 0,
            charge: 0,
            health: if s.dead { 0 } else { s.health },
            goal: s.goal,
            tier: if broken && s.killed {
                self.tier(State { killed: false, ..s })
            } else {
                self.tier(s)
            },
        }
    }
    pub fn witness(&self) -> Vec<u8> {
        let mut tape = Vec::new();
        let mut s = self.initial();
        while s.zone == LEGS {
            let a = self.action(s);
            tape.push(a);
            s = self.step(s, a);
        }
        while !s.killed {
            let a = if s.aim == 0 {
                self.shot()
            } else {
                (0..4)
                    .find(|&a| a != self.shot() && a != self.door())
                    .expect("a wait action")
            };
            tape.push(a);
            s = self.step(s, a);
        }
        tape.push(self.door());
        s = self.step(s, self.door());
        while !s.goal && !s.dead {
            let a = self.action(s);
            tape.push(a);
            s = self.step(s, a);
        }
        tape
    }
    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        let end = self
            .witness()
            .into_iter()
            .fold(self.initial(), |s, a| self.step(s, a));
        Ok(self.goal(end))
    }
    pub fn first_marks(&self, s: State) -> [bool; 4] {
        [
            s.zone != LEGS,
            s.killed,
            !s.killed && s.zone == CORRIDOR && s.position + 1 == self.barrier,
            s.killed && s.zone == CORRIDOR && s.position >= self.barrier,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            items: 2,
            leg: 3,
            walk: 6,
            barrier: 4,
            step_run: 2,
            fight: 3,
            fight_odds: 4,
            health: 5,
            hit_odds: 3,
            shortcut: 0,
            shortcut_run: 0,
            dense: false,
            pattern: 0x1b2d_4e87_a5c3_96f0,
        }
    }

    fn walk_to_boss(w: &Config) -> State {
        let mut s = w.initial();
        while s.zone == LEGS {
            s = w.step(s, w.action(s));
        }
        s
    }

    #[test]
    fn each_step_needs_a_run_of_pattern_presses_and_each_leg_ends_with_an_item() {
        let w = config();
        let s = w.initial();
        let half = w.step(s, w.action(s));
        assert_eq!((half.position, half.run), (0, 1));
        assert_eq!(w.key(half, false).place, w.key(s, false).place);
        let wrong = (w.action(half) + 1) % 4;
        assert_eq!(w.step(half, wrong).run, 0);
        let mut s = w.step(half, w.action(half));
        assert_eq!((s.position, s.run), (1, 0));
        while s.items == 0 {
            s = w.step(s, w.action(s));
        }
        assert_eq!((s.position, s.items, s.health), (w.leg, 1, w.health));
        assert_eq!(w.key(s, false).tier, 2);
        let boss = walk_to_boss(&w);
        assert_eq!((boss.zone, boss.items), (BOSS, w.items));
        assert_eq!(w.key(boss, false).tier, 4);
    }

    #[test]
    fn a_wrong_action_can_cost_health_and_a_hit_at_zero_health_kills() {
        let w = Config {
            hit_odds: 1,
            health: 1,
            ..config()
        };
        let s = w.initial();
        let wrong = (w.action(s) + 1) % 4;
        let hurt = w.step(s, wrong);
        assert_eq!(hurt.health, 0);
        let dead = w.step(hurt, wrong);
        assert!(dead.dead);
        assert_eq!(w.step(dead, w.action(dead)), dead);
        assert_eq!(w.key(dead, false).health, 0);
    }

    #[test]
    fn shots_land_only_at_aim_zero_and_the_kill_raises_the_tier_above_the_fight() {
        let w = config();
        let boss = walk_to_boss(&w);
        let wait = (0..4).find(|&a| a != w.shot() && a != w.door()).unwrap();
        let mut ready = boss;
        while ready.aim != 0 {
            ready = w.step(ready, wait);
        }
        assert_eq!(w.key(ready, false).place, w.key(boss, false).place);
        let aimed = w.step(ready, w.shot());
        assert_eq!(aimed.hits, 1);
        assert_eq!(w.key(aimed, false).tier, 5);
        let miss = w.step(aimed, w.shot());
        assert_eq!((miss.hits, miss.health), (1, aimed.health - 1));
        let mut s = aimed;
        while !s.killed {
            s = w.step(s, if s.aim == 0 { w.shot() } else { wait });
        }
        assert_eq!((s.hits, s.health), (0, w.health));
        assert_eq!(w.key(s, false).tier, 6);
        assert_eq!(w.key(s, true).tier, 4);
    }

    #[test]
    fn the_barrier_stops_a_line_without_the_kill_and_the_killed_line_reaches_the_goal() {
        let w = config();
        let boss = walk_to_boss(&w);
        let mut s = w.step(boss, w.door());
        assert_eq!((s.zone, s.position), (CORRIDOR, 0));
        for _ in 0..64 {
            s = w.step(s, w.action(s));
        }
        assert_eq!(s.position, w.barrier - 1);
        assert!(w.first_marks(s)[2]);
        assert!(!s.goal);
        assert!(w.reachable().unwrap());
        let end = w
            .witness()
            .into_iter()
            .fold(w.initial(), |s, a| w.step(s, a));
        assert!(w.goal(end));
        assert!(w.key(end, false).goal);
    }

    #[test]
    fn dense_progress_rises_with_every_position_and_ranks_the_kill_above_every_unkilled_place() {
        let w = Config {
            dense: true,
            ..config()
        };
        let tape = w.witness();
        let mut s = w.initial();
        let mut killed_low = u16::MAX;
        let mut unkilled_high = 0;
        for a in tape {
            let next = w.step(s, a);
            assert!(w.tier(next) >= w.tier(s), "{s:?} {next:?}");
            if next.killed {
                killed_low = killed_low.min(w.tier(next));
            } else {
                unkilled_high = unkilled_high.max(w.tier(next));
            }
            s = next;
        }
        let blocked = State {
            zone: CORRIDOR,
            position: w.barrier - 1,
            items: w.items,
            ..w.initial()
        };
        unkilled_high = unkilled_high.max(w.tier(blocked));
        assert!(killed_low > unkilled_high);
        assert!(w.goal(s));
    }

    #[test]
    fn a_shortcut_lets_a_line_with_fewer_items_walk_the_corridor_to_the_barrier() {
        let w = Config {
            shortcut: 1,
            ..config()
        };
        let mut s = w.initial();
        while s.items == 0 {
            s = w.step(s, w.action(s));
        }
        assert!(w.shortcut_door(s));
        let w = Config {
            shortcut_run: 3,
            ..w
        };
        let door = s;
        for side in 1..3 {
            s = w.step(s, w.shortcut_action(door));
            assert_eq!((s.zone, s.side), (LEGS, side));
            assert_eq!(w.key(s, false), w.key(door, false));
        }
        assert_eq!(w.step(s, w.action(s)).side, 0);
        let mut s = w.step(s, w.shortcut_action(door));
        assert_eq!((s.zone, s.position, s.items), (CORRIDOR, 0, 1));
        assert!(w.state_is_bounded(s));
        assert_eq!(w.key(s, false).tier, 2);
        for _ in 0..64 {
            s = w.step(s, w.action(s));
        }
        assert_eq!(s.position, w.barrier - 1);
        assert!(w.first_marks(s)[2]);
        assert!(Config { shortcut: 2, ..w }.validate().is_err());
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { items: 0, ..w }.validate().is_err());
        assert!(Config { barrier: 6, ..w }.validate().is_err());
        assert!(Config { step_run: 0, ..w }.validate().is_err());
        assert!(Config { hit_odds: 17, ..w }.validate().is_err());
        assert!(!w.state_is_bounded(State {
            run: 2,
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
                config: crate::worlds::World::Gated(config()),
                broken,
                scale: None,
            };
            let report = crate::run(&workload, crate::test_seed(), 3000, true).unwrap();
            assert_eq!(report["verified"], true);
        }
    }
}
