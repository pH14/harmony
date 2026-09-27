// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

const TAKE: u8 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Placement {
    Tier,
    Identity,
    Preference,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub barriers: u8,
    pub segment: u8,
    pub pattern: u64,
    pub placement: Placement,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub items: u8,
    pub scouted: bool,
    pub goal: bool,
}

impl Config {
    pub fn length(&self) -> u8 {
        (self.barriers + 1) * self.segment
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=4).contains(&self.barriers) || !(1..=8).contains(&self.segment) {
            return Err("backtrack barriers 1..=4 and segment 1..=8 are required".into());
        }
        if self.length() > 32 {
            return Err("backtrack length (barriers + 1) * segment must be at most 32".into());
        }
        if self.length() < 32 && self.pattern >> (2 * u32::from(self.length())) != 0 {
            return Err("backtrack pattern has bits beyond the line length".into());
        }
        if self.route_action(0) == TAKE {
            return Err("the first route action must differ from the take action".into());
        }
        Ok(())
    }
    pub fn route_action(&self, position: u8) -> u8 {
        crate::pattern_action(self.pattern, position)
    }
    fn next_barrier(&self, items: u8) -> u8 {
        (items + 1) * self.segment
    }
    pub fn initial(&self) -> State {
        State {
            position: 0,
            items: 0,
            scouted: false,
            goal: false,
        }
    }
    pub fn state_is_bounded(&self, s: State) -> bool {
        s.items <= self.barriers
            && s.position <= self.next_barrier(s.items).min(self.length())
            && s.goal == (s.position == self.length())
            && !(s.scouted && s.items == self.barriers)
    }
    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }
    pub fn step(&self, s: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(s) || s.goal {
            return s;
        }
        if s.position == 0 && action == TAKE {
            return if s.scouted {
                State {
                    items: s.items + 1,
                    scouted: false,
                    ..s
                }
            } else {
                s
            };
        }
        if action != self.route_action(s.position) {
            return State { position: 0, ..s };
        }
        if s.items < self.barriers && s.position == self.next_barrier(s.items) {
            return s;
        }
        let position = s.position + 1;
        State {
            position,
            scouted: s.scouted
                || (s.items < self.barriers && position == self.next_barrier(s.items)),
            goal: position == self.length(),
            ..s
        }
    }
    pub fn key(&self, s: State, broken: bool) -> Key {
        let items = if broken { 0 } else { s.items };
        let scouted = u16::from(s.scouted);
        let (context, charge, tier) = match self.placement {
            Placement::Tier => (scouted, 0, u16::from(items)),
            Placement::Identity => (u16::from(items) * 2 + scouted, 0, 0),
            Placement::Preference => (scouted, items, 0),
        };
        Key {
            stock: 0,
            place: u16::from(s.position),
            context,
            charge,
            health: 0,
            goal: s.goal,
            tier,
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

    fn config(placement: Placement) -> Config {
        Config {
            barriers: 2,
            segment: 2,
            pattern: 0b01_10_00_01_10_00,
            placement,
        }
    }

    fn walk(w: &Config, s: State, from: u8, to: u8) -> State {
        (from..to).fold(s, |s, p| w.step(s, w.route_action(p)))
    }

    #[test]
    fn every_item_needs_a_scouted_barrier_and_a_return_to_the_hub() {
        let w = config(Placement::Tier);
        let at_barrier = walk(&w, w.initial(), 0, 2);
        assert_eq!((at_barrier.position, at_barrier.scouted), (2, true));
        assert_eq!(w.step(at_barrier, w.route_action(2)), at_barrier);
        assert_eq!(w.step(w.initial(), TAKE), w.initial());
        let home = w.step(at_barrier, (w.route_action(2) + 1) % 4);
        assert_eq!((home.position, home.scouted), (0, true));
        let one = w.step(home, TAKE);
        assert_eq!((one.items, one.scouted), (1, false));
        let past = walk(&w, one, 0, 4);
        assert_eq!((past.position, past.scouted), (4, true));
        let two = w.step(w.step(past, (w.route_action(4) + 1) % 4), TAKE);
        assert_eq!(two.items, 2);
        assert!(w.goal(walk(&w, two, 0, 6)));
        assert!(!w.goal(walk(&w, one, 0, 6)));
    }

    #[test]
    fn goal_is_reachable_for_every_size() {
        for barriers in 1..=4 {
            for segment in 1..=8 {
                let w = Config {
                    barriers,
                    segment,
                    pattern: 0,
                    placement: Placement::Tier,
                };
                if w.validate().is_ok() {
                    assert!(w.reachable().unwrap());
                }
            }
        }
    }

    #[test]
    fn placement_puts_items_in_tier_identity_or_preference_and_the_control_hides_them() {
        let one = State {
            position: 1,
            items: 1,
            scouted: true,
            goal: false,
        };
        let cases = [
            (Placement::Tier, (1, 0, 1)),
            (Placement::Identity, (3, 0, 0)),
            (Placement::Preference, (1, 1, 0)),
        ];
        for (placement, expected) in cases {
            let w = config(placement);
            let k = w.key(one, false);
            assert_eq!((k.context, k.charge, k.tier), expected);
            assert_eq!(k.place, 1);
            let hidden = w.key(one, true);
            assert_eq!((hidden.charge, hidden.tier), (0, 0));
            assert_eq!(hidden.context, 1);
        }
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config(Placement::Tier);
        assert!(Config { barriers: 0, ..w }.validate().is_err());
        assert!(Config { barriers: 5, ..w }.validate().is_err());
        assert!(Config { segment: 9, ..w }.validate().is_err());
        assert!(
            Config {
                barriers: 4,
                segment: 7,
                pattern: 0,
                ..w
            }
            .validate()
            .is_err()
        );
        assert!(Config { pattern: 3, ..w }.validate().is_err());
        assert!(
            Config {
                pattern: 1 << 12,
                ..w
            }
            .validate()
            .is_err()
        );
        assert!(!w.state_is_bounded(State {
            position: 3,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            items: 2,
            scouted: true,
            ..w.initial()
        }));
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
        let mut v = serde_json::to_value(w).unwrap();
        v["placement"] = "Tier".into();
        assert!(serde_json::from_value::<Config>(v).is_err());
    }

    #[test]
    fn chain_stage_replays() {
        let stage = crate::chain::Stage {
            world: crate::worlds::World::Backtrack(config(Placement::Tier)),
            refill_available: true,
        };
        let workload = crate::Workload {
            config: crate::worlds::World::Chain(crate::chain::Config {
                stages: vec![stage.clone(), stage],
                carry_charge: false,
                initial_charge: 0,
                ranked: true,
            }),
            broken: false,
            scale: None,
        };
        workload.config.validate().unwrap();
        assert!(workload.config.reachable().unwrap());
        let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
        assert_eq!(report["verified"], true);
    }

    #[test]
    fn campaign_replays_for_every_placement_in_both_arms() {
        for placement in [Placement::Tier, Placement::Identity, Placement::Preference] {
            for broken in [false, true] {
                let workload = crate::Workload {
                    config: crate::worlds::World::Backtrack(config(placement)),
                    broken,
                    scale: None,
                };
                let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
                assert_eq!(report["verified"], true);
            }
        }
    }
}
