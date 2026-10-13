// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

const FIRST_TARGET: u8 = 3;
const HISTORIES: u8 = 64;
const AIM: u64 = 1;
const DRAIN: u64 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub width: u8,
    pub layout: u64,
    pub cannons: u8,
    pub guarded: u8,
    pub decoys: u8,
    pub bombs: u8,
    pub health: u8,
    pub aims: u8,
    pub double_aims: u8,
    pub drain_odds: u8,
    #[serde(default)]
    pub posture_actions: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub tail: u8,
    pub stock: u8,
    pub health: u8,
    pub cannons: u8,
    pub barriers: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Layout {
    pub cannons: Vec<u8>,
    pub decoys: Vec<u8>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub first_down_execution: Vec<Option<u64>>,
    pub first_down_work: Vec<Option<u64>>,
    pub shots: u64,
    pub landed: u64,
    pub double_kills: u64,
    pub decoy_hits: u64,
}

fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(4..=16).contains(&self.width) {
            return Err("targets width must be 4..=16".into());
        }
        if !(1..=5).contains(&self.cannons)
            || self.guarded > self.cannons
            || self.decoys > 3
            || self.guarded + self.decoys > 6
        {
            return Err(
                "targets need 1..=5 cannons, at most that many guarded, at most 3 decoys and at most 6 barriers"
                    .into(),
            );
        }
        if self.cannons + self.decoys > self.width - FIRST_TARGET {
            return Err("targets need a free position for every cannon and decoy".into());
        }
        if !(1..=15).contains(&self.bombs) || !(1..=63).contains(&self.health) {
            return Err("targets need 1..=15 bombs and 1..=63 health".into());
        }
        if self.posture_actions > 2 {
            return Err("targets posture_actions must be 0..=2".into());
        }
        if !(1..=HISTORIES).contains(&self.aims) || self.double_aims > self.aims {
            return Err("targets aims must be 1..=64 and double_aims at most aims".into());
        }
        Ok(())
    }

    pub fn layout(&self) -> Layout {
        let mut rand = RomuDuoJrRand::with_seed(self.layout);
        let mut free: Vec<u8> = (FIRST_TARGET..self.width).collect();
        let mut picked = Vec::new();
        for _ in 0..self.cannons + self.decoys {
            let at = rand.below(NonZeroUsize::new(free.len()).expect("validated free positions"));
            picked.push(free.swap_remove(at));
        }
        let decoys = picked.split_off(usize::from(self.cannons));
        Layout {
            cannons: picked,
            decoys,
        }
    }

    fn roll(&self, salt: u64, position: u8, tail: u8) -> u64 {
        mix(self.layout ^ mix(salt << 16 | u64::from(position) << 8 | u64::from(tail)))
    }

    fn rank(&self, position: u8, tail: u8) -> u8 {
        let h = self.roll(AIM, position, 0);
        let scale = 2 * (h & 31) + 1;
        let offset = (h >> 5) & 63;
        ((u64::from(tail) * scale + offset) & 63) as u8
    }

    fn barrier_bits(&self) -> u8 {
        self.guarded + self.decoys
    }

    pub fn initial(&self) -> State {
        State {
            position: 0,
            tail: 0,
            stock: self.bombs,
            health: self.health,
            cannons: (1 << self.cannons) - 1,
            barriers: (1 << self.barrier_bits()) - 1,
        }
    }

    pub fn down(&self, s: State) -> u8 {
        self.cannons - s.cannons.count_ones() as u8
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        let guards = (1 << self.guarded) - 1;
        self.validate().is_ok()
            && s.position < self.width
            && s.tail < HISTORIES
            && s.stock <= self.bombs
            && s.health <= self.health
            && s.cannons < 1 << self.cannons
            && s.barriers < 1 << self.barrier_bits()
            && s.barriers & guards & !s.cannons == 0
    }

    pub fn goal(&self, s: State) -> bool {
        s.cannons == 0 && s.health > 0
    }

    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || self.goal(s) || s.health == 0 {
            return s;
        }
        let tail = ((s.tail << 2) | action) & (HISTORIES - 1);
        let mut next = State { tail, ..s };
        match action {
            0 => next.position = (s.position + 1).min(self.width - 1),
            1 => next.position = s.position.saturating_sub(1),
            3 if s.stock > 0 => {
                next.stock -= 1;
                self.shoot(&self.layout(), &mut next, s.position, s.tail);
            }
            _ => {}
        }
        let live = u64::from(next.cannons.count_ones());
        if live > 0
            && self.drain_odds > 0
            && self.roll(DRAIN, next.position, tail) % u64::from(self.drain_odds) < live
        {
            next.health -= 1;
        }
        next
    }

    fn shoot(&self, layout: &Layout, s: &mut State, position: u8, history: u8) {
        let rank = self.rank(position, history);
        if rank >= self.aims {
            return;
        }
        if let Some(i) = layout.cannons.iter().position(|&p| p == position) {
            let bit = 1 << i;
            if s.cannons & bit == 0 {
                return;
            }
            if i < usize::from(self.guarded) && s.barriers & bit != 0 {
                s.barriers &= !bit;
                if rank < self.double_aims {
                    s.cannons &= !bit;
                }
            } else {
                s.cannons &= !bit;
            }
        } else if let Some(j) = layout.decoys.iter().position(|&p| p == position) {
            s.barriers &= !(1 << (usize::from(self.guarded) + j));
        }
    }

    pub fn shot_outcome(&self, before: State, after: State) -> (bool, bool, bool) {
        let barrier = before.barriers & !after.barriers;
        let cannon = before.cannons & !after.cannons;
        let guard = barrier & ((1 << self.guarded) - 1);
        (
            barrier != 0 || cannon != 0,
            guard != 0 && cannon != 0,
            barrier & !guard != 0,
        )
    }

    pub fn key(&self, s: State, broken: bool) -> Key {
        let down = if broken { 0 } else { self.down(s) };
        let buckets = u16::from(self.width.div_ceil(2));
        let place = ((((u16::from(down) << self.barrier_bits()) | u16::from(s.barriers))
            * buckets
            + u16::from(s.position / 2))
            << (2 * self.posture_actions))
            | u16::from(s.tail & ((1 << (2 * self.posture_actions)) - 1));
        Key {
            stock: s.stock,
            place,
            context: u16::from(s.position % 2),
            charge: 0,
            health: s.health,
            goal: self.goal(s),
            tier: 0,
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        let guards = (1_u8 << self.guarded) - 1;
        let stocks = usize::from(self.bombs) + 1;
        let index = |s: State| {
            ((((usize::from(s.position) * usize::from(HISTORIES) + usize::from(s.tail))
                << self.cannons)
                | usize::from(s.cannons))
                << self.guarded
                | usize::from(s.barriers & guards))
                * stocks
                + usize::from(s.stock)
        };
        let start = self.initial();
        let mut best = std::collections::BTreeMap::from([(index(start), start.health)]);
        let needed = |s: State| {
            let shielded = if self.double_aims == 0 {
                (s.barriers & guards & s.cannons).count_ones()
            } else {
                0
            };
            s.cannons.count_ones() + shielded
        };
        let mut pending = std::collections::BinaryHeap::from([(self.down(start), start)]);
        while let Some((_, s)) = pending.pop() {
            if s.health < best[&index(s)] || u32::from(s.stock) < needed(s) {
                continue;
            }
            for action in 0..4 {
                let next = self.step(s, action);
                if self.goal(next) {
                    return Ok(true);
                }
                if best
                    .get(&index(next))
                    .is_none_or(|&kept| next.health > kept)
                {
                    best.insert(index(next), next.health);
                    if best.len() > crate::MAX_REACHABLE_STATES {
                        return Err(format!(
                            "reachability search exceeded {} states",
                            crate::MAX_REACHABLE_STATES
                        ));
                    }
                    pending.push((self.down(next), next));
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: u64 = 0x5eed_b00b_ea11;

    fn config() -> Config {
        Config {
            width: 16,
            layout: LAYOUT,
            cannons: 5,
            guarded: 3,
            decoys: 2,
            bombs: 7,
            health: 28,
            aims: 8,
            double_aims: 2,
            drain_odds: 0,
            posture_actions: 0,
        }
    }

    fn fire(w: &Config, s: State, position: u8, wanted: impl Fn(u8) -> bool) -> State {
        let tail = (0..HISTORIES)
            .find(|&tail| wanted(w.rank(position, tail)))
            .expect("a history with the wanted rank");
        w.step(
            State {
                position,
                tail,
                ..s
            },
            3,
        )
    }

    #[test]
    fn a_placed_bomb_can_destroy_a_barrier_and_the_cannon_behind_it() {
        let w = config();
        let l = w.layout();
        let start = w.initial();
        let double = fire(&w, start, l.cannons[0], |r| r < w.double_aims);
        assert_eq!(double.stock, start.stock - 1);
        assert_eq!((double.barriers & 1, double.cannons & 1), (0, 0));
        assert_eq!(w.down(double), 1);
        assert_eq!(w.shot_outcome(start, double), (true, true, false));
        let single = fire(&w, start, l.cannons[0], |r| {
            (w.double_aims..w.aims).contains(&r)
        });
        assert_eq!((single.barriers & 1, single.cannons & 1), (0, 1));
        assert_eq!(w.shot_outcome(start, single), (true, false, false));
        let exposed = fire(&w, single, l.cannons[0], |r| {
            (w.double_aims..w.aims).contains(&r)
        });
        assert_eq!((exposed.cannons & 1, w.down(exposed)), (0, 1));
        let bit = 1 << w.guarded;
        let open = fire(&w, start, l.cannons[usize::from(w.guarded)], |r| r < w.aims);
        assert_eq!((open.cannons & bit, open.barriers), (0, start.barriers));
        let decoy = fire(&w, start, l.decoys[0], |r| r < w.aims);
        assert_eq!(decoy.cannons, start.cannons);
        assert_eq!(decoy.barriers, start.barriers & !bit);
        assert_eq!(w.shot_outcome(start, decoy), (true, false, true));
    }

    #[test]
    fn misplaced_and_mistimed_bombs_are_spent_without_effect() {
        let w = config();
        let l = w.layout();
        let start = w.initial();
        let missed = fire(&w, start, l.cannons[0], |r| r >= w.aims);
        assert_eq!(
            (missed.stock, missed.cannons, missed.barriers),
            (start.stock - 1, start.cannons, start.barriers)
        );
        let empty = (0..w.width)
            .find(|p| !l.cannons.contains(p) && !l.decoys.contains(p))
            .unwrap();
        let wasted = fire(&w, start, empty, |r| r < w.double_aims);
        assert_eq!(
            (wasted.stock, wasted.cannons, wasted.barriers),
            (start.stock - 1, start.cannons, start.barriers)
        );
        let gone = fire(&w, wasted, l.cannons[usize::from(w.guarded)], |r| {
            r < w.aims
        });
        let again = fire(&w, gone, l.cannons[usize::from(w.guarded)], |r| r < w.aims);
        assert_eq!((again.stock, again.cannons), (gone.stock - 1, gone.cannons));
        let dry = fire(&w, State { stock: 0, ..start }, l.cannons[0], |r| {
            r < w.double_aims
        });
        assert_eq!(
            (dry.stock, dry.cannons, dry.barriers),
            (0, start.cannons, start.barriers)
        );
        for action in 0..3 {
            assert_eq!(w.step(start, action).stock, start.stock);
        }
        assert_eq!(w.step(start, 1).position, 0);
        assert_eq!(w.step(start, 0).position, 1);
    }

    #[test]
    fn live_cannons_drain_health_and_a_drained_player_stops() {
        let w = Config {
            drain_odds: 1,
            ..config()
        };
        let start = w.initial();
        assert_eq!(w.step(start, 2).health, start.health - 1);
        assert_eq!(config().step(start, 2).health, start.health);
        let dead = State { health: 0, ..start };
        assert_eq!(w.step(dead, 0), dead);
        assert!(!w.goal(dead));
        let cleared = State {
            cannons: 0,
            barriers: 0,
            ..start
        };
        assert!(w.goal(cleared) && w.state_is_bounded(cleared));
        assert_eq!(w.step(cleared, 0), cleared);
        assert!(!w.state_is_bounded(State {
            cannons: start.cannons & !1,
            ..start
        }));
    }

    #[test]
    fn the_key_reads_cannons_down_in_the_place() {
        let base = config();
        let start = base.initial();
        let down = State {
            cannons: start.cannons & !1,
            barriers: start.barriers & !1,
            stock: start.stock - 1,
            ..start
        };
        let exposed = State {
            cannons: start.cannons,
            ..down
        };
        assert_ne!(base.key(down, false).place, base.key(exposed, false).place);
        assert_eq!(base.key(down, true).place, base.key(exposed, true).place);
        assert_eq!(
            (base.key(down, false).stock, base.key(down, false).health),
            (start.stock - 1, start.health)
        );
        let posed = Config {
            posture_actions: 2,
            ..base
        };
        let places: std::collections::BTreeSet<u16> = (0..HISTORIES)
            .map(|tail| posed.key(State { tail, ..start }, false).place)
            .collect();
        assert_eq!(places.len(), 16);
        let widest = Config { decoys: 3, ..posed };
        assert!(widest.validate().is_ok());
        assert!(Config { decoys: 4, ..posed }.validate().is_err());
        assert!(
            Config {
                posture_actions: 3,
                ..base
            }
            .validate()
            .is_err()
        );
        let last = State {
            position: widest.width - 1,
            tail: HISTORIES - 1,
            cannons: 0,
            barriers: (1 << 6) - 1,
            ..start
        };
        assert!(widest.key(last, false).place > widest.key(start, false).place);
    }

    #[test]
    fn a_win_needs_a_double_kill_when_bombs_cover_one_fewer_than_the_objects() {
        let w = config();
        assert!(w.reachable().unwrap());
        assert!(
            !Config {
                double_aims: 0,
                ..w
            }
            .reachable()
            .unwrap()
        );
        assert!(
            Config {
                double_aims: 0,
                bombs: 8,
                ..w
            }
            .reachable()
            .unwrap()
        );
        assert!(!Config { bombs: 4, ..w }.reachable().unwrap());
        assert!(Config { decoys: 4, ..w }.validate().is_err());
        assert!(
            Config {
                double_aims: 9,
                ..w
            }
            .validate()
            .is_err()
        );
        assert!(Config { width: 9, ..w }.validate().is_err());
    }

    #[test]
    fn a_campaign_replays() {
        let w = Config {
            width: 8,
            cannons: 2,
            guarded: 1,
            decoys: 1,
            bombs: 2,
            aims: 32,
            double_aims: 16,
            drain_odds: 32,
            ..config()
        };
        assert!(w.reachable().unwrap());
        let report = crate::run(
            &crate::Workload {
                config: crate::worlds::World::Targets(w),
                broken: false,
                scale: None,
            },
            crate::test_seed(),
            100_000,
            true,
        )
        .unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["success"], true);
        let evidence: Evidence =
            serde_json::from_value(report["evidence"]["targets"].clone()).unwrap();
        assert_eq!(evidence.first_down_execution.len(), 3);
        for found in [&evidence.first_down_execution, &evidence.first_down_work] {
            let found: Vec<u64> = found.iter().map(|at| at.unwrap()).collect();
            assert_eq!(found[0], 0);
            assert!(found[1] > 0);
            assert!(found.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        assert!(evidence.landed > 0);
        assert!(evidence.landed <= evidence.shots);
        assert!(evidence.double_kills > 0);
        assert!(evidence.double_kills + evidence.decoy_hits <= evidence.landed);
    }
}
