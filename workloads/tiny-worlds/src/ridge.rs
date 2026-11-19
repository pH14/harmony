// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    Keying,
    worlds::{State as WorldState, World},
};
use searcher::search::archive::{ArchiveKey, KeyShape};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, error::Error};

const PITCH: u8 = 3;
const RIDGE: u8 = 1;
const SUMMIT: u8 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub spacing: u8,
    pub ridge: u8,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub camp: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Key {
    pub tier: u8,
    pub position: u8,
    pub frontier: u8,
    pub camp: u8,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=31).contains(&self.length)
            || !(1..self.length).contains(&self.spacing)
            || !(1..self.length).contains(&self.ridge)
        {
            return Err("ridge needs length 2..=31 and spacing and ridge 1..length".into());
        }
        Ok(())
    }

    pub fn climb(&self, position: u8) -> u8 {
        match crate::pattern_action(self.pattern, position) {
            PITCH => 0,
            action => action,
        }
    }

    pub fn site(&self, position: u8) -> bool {
        position.is_multiple_of(self.spacing)
    }

    pub fn initial(&self) -> State {
        State {
            position: 0,
            camp: 0,
        }
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && s.position <= self.length
            && s.camp <= s.position
            && s.camp < self.length
            && self.site(s.camp)
    }

    pub fn goal(&self, s: State) -> bool {
        s.position == self.length
    }

    pub fn step(&self, s: State, action: u8) -> State {
        if action > PITCH || self.goal(s) {
            return s;
        }
        if action == PITCH {
            return if self.site(s.position) && s.position > s.camp {
                State {
                    camp: s.position,
                    ..s
                }
            } else {
                s
            };
        }
        if action == self.climb(s.position) {
            State {
                position: s.position + 1,
                ..s
            }
        } else {
            State {
                position: s.camp,
                ..s
            }
        }
    }

    pub fn tier(&self, s: State) -> u8 {
        if self.goal(s) {
            SUMMIT
        } else if s.position >= self.ridge {
            RIDGE
        } else {
            0
        }
    }

    pub fn key(&self, s: State) -> Key {
        Key {
            tier: self.tier(s),
            position: s.position,
            frontier: s.position,
            camp: s.camp,
        }
    }

    pub fn shared_key(&self, s: State) -> crate::Key {
        crate::Key {
            stock: 0,
            place: u16::from(s.position),
            context: 0,
            charge: 0,
            health: 0,
            goal: self.goal(s),
            tier: u16::from(self.tier(s)),
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }
}

impl ArchiveKey for Key {
    type Place = u8;
    type Progress = u8;
    type Identity = u8;
    type Lineage = ();
    fn place(self) -> u8 {
        self.position
    }
    fn progress(self) -> u8 {
        self.tier
    }
    fn identity(self) -> u8 {
        self.frontier
    }
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }
    fn preference_cmp(self, _: usize, other: Self) -> Ordering {
        if self.tier == other.tier {
            self.camp.cmp(&other.camp)
        } else {
            Ordering::Equal
        }
    }
    fn complete(self, parent: Option<(Self, &())>) -> Self {
        let Some((parent, ())) = parent else {
            return self;
        };
        Self {
            tier: self.tier.max(parent.tier),
            frontier: if parent.camp == self.camp {
                self.frontier.max(parent.frontier)
            } else {
                self.frontier
            },
            ..self
        }
    }
    fn record(_: &mut (), _: Self) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hand;

impl Keying for Hand {
    type Key = Key;
    type Lane = ();
    fn arrive(&self, _: &World, _: WorldState, _: bool, _: &mut ()) {}
    fn key(
        &self,
        world: &World,
        state: WorldState,
        _: bool,
        _: &(),
    ) -> Result<Key, Box<dyn Error>> {
        match (world, state) {
            (World::Ridge(w), WorldState::Ridge(s)) => Ok(w.key(s)),
            _ => Err("the ridge hand key needs a ridge world".into()),
        }
    }
    fn shape(&self) -> KeyShape {
        KeyShape::of::<Key>()
    }
    fn identity(&self) -> String {
        "ridge-hand-key".into()
    }
    fn policy(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Keep, KeyKind, SearchSettings, Workload, run_kept};

    fn config() -> Config {
        Config {
            length: 12,
            spacing: 4,
            ridge: 6,
            pattern: 0x00e4_e4e4,
        }
    }

    fn climbed(w: &Config, s: State) -> State {
        w.step(s, w.climb(s.position))
    }

    fn wrong(w: &Config, s: State) -> u8 {
        (w.climb(s.position) + 1) % 3
    }

    #[test]
    fn a_wrong_action_slides_back_to_the_camp_and_a_camp_is_pitched_only_at_a_new_site() {
        let w = config();
        assert!(w.validate().is_ok() && w.reachable().unwrap());
        let mut s = w.initial();
        assert_eq!(w.step(s, PITCH), s);
        for _ in 0..5 {
            s = climbed(&w, s);
        }
        assert_eq!(s.position, 5);
        assert_eq!(w.step(s, PITCH), s);
        assert_eq!(w.step(s, wrong(&w, s)), w.initial());
        s = State { position: 4, ..s };
        let camped = w.step(s, PITCH);
        assert_eq!(
            camped,
            State {
                position: 4,
                camp: 4
            }
        );
        let up = climbed(&w, climbed(&w, camped));
        assert_eq!(w.step(up, wrong(&w, up)), camped);
        assert_eq!(w.tier(up), 1);
        assert_eq!(w.tier(camped), 0);
        let top = State {
            position: w.length,
            camp: 8,
        };
        assert!(w.goal(top) && w.state_is_bounded(top));
        assert_eq!(w.step(top, 0), top);
        assert!(!w.state_is_bounded(State {
            position: 5,
            camp: 6
        }));
        assert!(!w.state_is_bounded(State {
            position: 5,
            camp: 3
        }));
        assert!(Config { ridge: 12, ..w }.validate().is_err());
        assert!(Config { length: 32, ..w }.validate().is_err());
    }

    #[test]
    fn the_hand_key_latches_the_ridge_and_keeps_the_frontier_until_the_camp_moves() {
        let w = config();
        let raw = |position, camp| w.key(State { position, camp });
        let high = raw(7, 4).complete(Some((raw(6, 4).complete(None), &())));
        assert_eq!((high.tier, high.frontier), (1, 7));
        let slid = raw(4, 4).complete(Some((high, &())));
        assert_eq!((slid.tier, slid.frontier, slid.position), (1, 7, 4));
        assert_ne!(slid, raw(4, 4));
        let moved = raw(8, 8).complete(Some((raw(8, 4).complete(Some((slid, &()))), &())));
        assert_eq!((moved.tier, moved.frontier), (1, 8));
        assert_eq!(slid.preference_cmp(0, moved), Ordering::Less);
        assert_eq!(raw(2, 0).preference_cmp(0, moved), Ordering::Equal);
    }

    #[test]
    fn the_strategy_reproduces_the_hand_decisions_and_the_shadow_agrees() {
        let workload = Workload {
            keying: crate::Hand::<false>,
            config: World::Ridge(config()),
            broken: false,
            scale: None,
        };
        let seed = crate::test_seed();
        let settings = SearchSettings {
            mixture: None,
            stop_on_objective: Some(false),
        };
        let report = |key, shadow| {
            run_kept(
                &workload,
                Keep::Portfolio,
                key,
                None,
                shadow,
                seed,
                20_000,
                true,
                1,
                settings,
            )
            .unwrap()
        };
        let hand = report(KeyKind::Hand, false);
        let strategy = report(KeyKind::Strategy, false);
        let shadowed = report(KeyKind::Hand, true);
        for field in [
            "decision_stream_sha256",
            "decision_stream_records",
            "work",
            "executions",
            "first_objective_work",
        ] {
            assert_eq!(hand[field], strategy[field], "{field}");
            assert_eq!(hand[field], shadowed[field], "{field}");
        }
        assert_eq!(hand["verified"], true);
        assert_ne!(hand["stream_sha256"], strategy["stream_sha256"]);
        let shadow = &shadowed["shadow"];
        assert_eq!(shadow["disagreements"]["absent"], 0, "{shadow}");
        for field in ["cell", "slot", "progress", "preference"] {
            assert_eq!(shadow["disagreements"][field], 0, "{field}: {shadow}");
        }
        assert_eq!(shadow["disagreements"]["preference_count"], false);
        assert_eq!(shadow["disagreements"]["capacity"], false);
        assert_eq!(shadow["unknown_restores"], 0);
        assert!(shadow["lineage_changes"].as_u64().unwrap() > 0, "{shadow}");
        assert_eq!(shadow["tiers"].as_array().unwrap().len(), 3, "{shadow}");
    }
}
