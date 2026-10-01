// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub cells: u16,
    pub length: u8,
    pub pattern: u32,
    pub layout: u64,
    pub rooted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_denominator: Option<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u16,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=60_000).contains(&self.cells) {
            return Err("crossing cells must be 2..=60000".into());
        }
        if !(1..=8).contains(&self.length) {
            return Err("crossing length must be 1..=8".into());
        }
        if self.action_denominator.is_some_and(|choices| choices < 4) {
            return Err("crossing action_denominator must be 4..=65535".into());
        }
        if self.pattern >= 1 << (2 * u32::from(self.length)) {
            return Err("crossing pattern exceeds its length".into());
        }
        Ok(())
    }

    pub fn sample(&self, rand: &mut RomuDuoJrRand) -> u8 {
        let choices = usize::from(self.action_denominator.unwrap_or(4));
        rand.below(NonZeroUsize::new(choices).expect("validated action denominator"))
            .min(4) as u8
    }

    pub fn initial(&self) -> State {
        State {
            position: if self.rooted { self.cells } else { 0 },
        }
    }

    pub fn state_is_bounded(&self, state: State) -> bool {
        self.validate().is_ok() && state.position <= self.cells + u16::from(self.length)
    }

    pub fn goal(&self, state: State) -> bool {
        self.state_is_bounded(state) && state.position == self.cells + u16::from(self.length)
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if action > 3 || !self.state_is_bounded(state) || self.goal(state) {
            return state;
        }
        if state.position < self.cells {
            let position = match action {
                0 => state.position + 1,
                1 | 2 => {
                    let mut rng = RomuDuoJrRand::with_seed(
                        self.layout ^ (u64::from(state.position) << 2 | u64::from(action)),
                    );
                    (rng.next_u64() % u64::from(self.cells)) as u16
                }
                _ => state.position,
            };
            return State { position };
        }
        let offset = state.position - self.cells;
        let correct = ((self.pattern >> (2 * u32::from(offset))) & 3) as u8;
        State {
            position: if action == correct {
                state.position + 1
            } else {
                self.cells
            },
        }
    }

    pub fn key(&self, state: State, broken: bool) -> Key {
        Key {
            place: if broken && state.position >= self.cells {
                self.cells
            } else {
                state.position
            },
            context: 0,
            tier: 0,
            stock: 0,
            charge: 0,
            health: 1,
            goal: self.goal(state),
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
            cells: 19,
            length: 3,
            pattern: 36,
            layout: 73,
            rooted: false,
            action_denominator: None,
        }
    }

    #[test]
    fn action_denominator_preserve_default_draws_and_add_neutral_actions() {
        let default = config();
        let wide = Config {
            action_denominator: Some(1024),
            ..default
        };
        let mut expected = RomuDuoJrRand::with_seed(97);
        let mut actual = expected;
        let mut wide_rand = actual;
        let mut seen = [false; 5];
        for _ in 0..100_000 {
            assert_eq!(
                default.sample(&mut actual),
                expected.below(NonZeroUsize::new(4).unwrap()) as u8
            );
            let action = wide.sample(&mut wide_rand);
            seen[usize::from(action)] = true;
            if action >= 4 {
                let states = [
                    wide.initial(),
                    State {
                        position: wide.cells + 1,
                    },
                ];
                for state in states {
                    assert_eq!(wide.step(state, action), state);
                }
            }
        }
        assert!(seen.into_iter().all(|seen| seen));
        for _ in 0..256 {
            assert_eq!(actual.next_u64(), expected.next_u64());
        }
        for choices in [0, 3] {
            assert!(
                Config {
                    action_denominator: Some(choices),
                    ..default
                }
                .validate()
                .is_err()
            );
        }
        for choices in [4, 64, 1024, 65535] {
            assert!(
                Config {
                    action_denominator: Some(choices),
                    ..default
                }
                .reachable()
                .unwrap()
            );
        }
    }

    #[test]
    fn sampled_choices_replay_with_an_independent_entry_execution() {
        for rooted in [false, true] {
            let workload = crate::Workload {
                config: crate::worlds::World::Crossing(Config {
                    cells: 11,
                    rooted,
                    action_denominator: Some(64),
                    ..config()
                }),
                broken: false,
                scale: None,
            };
            let seed = crate::test_seed();
            let report = crate::run(&workload, seed, 120_000, true).unwrap();
            assert_eq!(report["verified"], true);
            assert_eq!(report["success"], true);
            let entry = report["evidence"]["crossing_first_entry_execution"]
                .as_u64()
                .unwrap();
            let objective = report["first_objective_execution"].as_u64().unwrap();
            assert!(entry <= objective);
            assert_eq!(entry == 0, rooted);
            let scaled = crate::run_scaled(
                &crate::Workload {
                    config: workload.config.clone(),
                    broken: false,
                    scale: Some(crate::Scale {
                        memory_budget_mib: 8192,
                        ..crate::Scale::default()
                    }),
                },
                seed,
                120_000,
                &mut Vec::new(),
                crate::SearchSettings {
                    stop_on_objective: Some(true),
                    ..crate::SearchSettings::default()
                },
            )
            .unwrap();
            assert_eq!(
                scaled["first_objective_execution"],
                report["first_objective_execution"]
            );
            assert_eq!(scaled["crossing"]["first_entry_execution"], entry);
            assert_eq!(scaled["stream_sha256"].as_str().unwrap().len(), 64);
            assert_eq!(
                scaled["crossing"]["first_entry_work"],
                report["evidence"]["crossing_first_entry_work"]
            );
            assert_eq!(
                report["evidence"]["crossing_first_entry_work"]
                    .as_u64()
                    .unwrap()
                    == 0,
                rooted
            );
        }
    }

    #[test]
    fn every_state_has_a_constructed_exit() {
        let w = config();
        for position in 0..=w.cells + u16::from(w.length) {
            let mut s = State { position };
            while s.position < w.cells {
                s = w.step(s, 0);
            }
            while !w.goal(s) {
                let offset = s.position - w.cells;
                s = w.step(s, ((w.pattern >> (2 * u32::from(offset))) & 3) as u8);
            }
            assert!(w.goal(s));
        }
        assert!(w.reachable().unwrap());
    }

    #[test]
    fn root_changes_only_the_initial_state() {
        let full = config();
        let root = Config {
            rooted: true,
            ..full
        };
        assert_ne!(full.initial(), root.initial());
        for position in 0..=full.cells + u16::from(full.length) {
            let s = State { position };
            assert_eq!(full.key(s, false), root.key(s, false));
            for action in 0..=4 {
                assert_eq!(full.step(s, action), root.step(s, action));
            }
        }
    }

    #[test]
    fn exits_and_failed_crossings_do_not_change_tier_or_resources() {
        let w = config();
        let entry = State { position: w.cells };
        let advanced = w.step(entry, 0);
        assert_eq!(advanced.position, w.cells + 1);
        assert_eq!(w.step(advanced, 2), entry);
        assert_eq!(w.step(advanced, 3), entry);
        assert_ne!(w.key(entry, false).place, w.key(advanced, false).place);
        assert_eq!(w.key(entry, true), w.key(advanced, true));
        for s in [entry, advanced, w.initial()] {
            let k = w.key(s, false);
            assert_eq!((k.tier, k.health, k.stock, k.charge), (0, 1, 0, 0));
        }
    }

    #[test]
    fn both_origins_replay_and_account_for_the_crossing() {
        for rooted in [false, true] {
            let config = Config {
                cells: 11,
                rooted,
                ..config()
            };
            let world = crate::Workload {
                config: crate::worlds::World::Crossing(config),
                broken: false,
                scale: None,
            };
            let report = crate::run(&world, crate::test_seed(), 5_000, true).unwrap();
            if rooted {
                assert_eq!(report["success"], true);
            }
            let evidence = &report["evidence"];
            let timeline: Vec<[u64; 3]> =
                serde_json::from_value(report["parent_timeline"].clone()).unwrap();
            let parents: Vec<(u64, u16, u16)> =
                serde_json::from_value(evidence["job_parents"].clone()).unwrap();
            assert_eq!(
                timeline.len() as u64,
                report["executions"].as_u64().unwrap()
            );
            assert_eq!(timeline.len(), parents.len());
            for ((sequence, tier, place), row) in parents.iter().zip(&timeline) {
                assert_eq!((u64::from(*tier), u64::from(*place)), (row[1], row[2]));
                assert!(*sequence > 0);
            }
            for window in timeline.windows(2) {
                assert!(window[0][0] <= window[1][0]);
            }
            if let Some(end) = report["first_objective_work"].as_u64() {
                let index = timeline.iter().rposition(|row| row[0] < end).unwrap();
                assert_eq!(
                    report["first_objective_execution"].as_u64(),
                    Some(parents[index].0)
                );
            } else {
                assert!(report["first_objective_execution"].is_null());
            }
            assert_eq!(
                evidence["crossing_actions"].as_u64().unwrap()
                    + evidence["crossing_pool_actions"].as_u64().unwrap(),
                report["work"].as_u64().unwrap()
            );
            if rooted {
                assert_eq!(evidence["crossing_first_entry_work"], 0);
            }
            if let Some(end) = report["first_objective_work"].as_u64() {
                assert!(evidence["crossing_first_entry_work"].as_u64().unwrap() <= end);
            }
        }
    }

    #[test]
    fn invalid_requests_and_states_are_rejected() {
        let w = config();
        assert!(Config { cells: 0, ..w }.validate().is_err());
        assert!(Config { length: 9, ..w }.validate().is_err());
        assert!(Config { pattern: 64, ..w }.validate().is_err());
        let invalid = State {
            position: w.cells + u16::from(w.length) + 1,
        };
        assert!(!w.state_is_bounded(invalid));
        assert_eq!(w.step(invalid, 0), invalid);
    }
}
