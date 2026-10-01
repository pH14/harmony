// SPDX-License-Identifier: AGPL-3.0-or-later

use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HoldBand {
    pub minimum: u8,
    pub maximum: u8,
    pub weight: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub events: Vec<u16>,
    pub holds: Vec<HoldBand>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub elapsed: u16,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub held_actions: Vec<u64>,
    pub parent_phase_jobs: Vec<u64>,
    pub maximum_parent_elapsed: Vec<u16>,
    pub first_event_work: Vec<Option<u64>>,
    pub first_event_execution: Vec<Option<u64>>,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.events.is_empty() || self.events.len() > 64 {
            return Err("passive clock needs 1..=64 events".into());
        }
        if self.events[0] == 0 || self.events.windows(2).any(|w| w[0] >= w[1]) {
            return Err("passive clock events must be positive and strictly increasing".into());
        }
        if self.holds.is_empty() || self.holds.len() > 16 {
            return Err("passive clock needs 1..=16 hold bands".into());
        }
        if self
            .holds
            .iter()
            .any(|b| b.minimum == 0 || b.minimum > b.maximum || b.weight == 0)
        {
            return Err("hold bands need positive durations and weights".into());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State { elapsed: 0 }
    }

    pub fn valid_state(&self, state: State) -> bool {
        self.events.last().is_some_and(|&end| state.elapsed <= end)
    }

    pub fn valid_action(&self, action: u8) -> bool {
        self.holds
            .iter()
            .any(|b| (b.minimum..=b.maximum).contains(&action))
    }

    pub fn maximum_hold(&self) -> u64 {
        self.holds
            .iter()
            .map(|b| u64::from(b.maximum))
            .max()
            .unwrap_or(0)
    }

    pub fn sample(&self, rand: &mut RomuDuoJrRand) -> u8 {
        let total: usize = self.holds.iter().map(|b| usize::from(b.weight)).sum();
        let mut choice = rand.below(NonZeroUsize::new(total).expect("validated positive bands"));
        for band in &self.holds {
            if choice < usize::from(band.weight) {
                let width = usize::from(band.maximum) - usize::from(band.minimum) + 1;
                return band.minimum
                    + rand.below(NonZeroUsize::new(width).expect("validated hold interval")) as u8;
            }
            choice -= usize::from(band.weight);
        }
        unreachable!("weighted draw selects a hold band")
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.valid_state(state) || !self.valid_action(action) {
            return state;
        }
        State {
            elapsed: state
                .elapsed
                .saturating_add(u16::from(action))
                .min(*self.events.last().expect("validated events")),
        }
    }

    pub fn phase(&self, state: State) -> usize {
        self.events.partition_point(|&event| event <= state.elapsed)
    }

    pub fn goal(&self, state: State) -> bool {
        self.valid_state(state) && self.events.last() == Some(&state.elapsed)
    }

    pub fn key(&self, state: State, broken: bool) -> crate::Key {
        crate::Key {
            stock: 0,
            place: if broken { 0 } else { self.phase(state) as u16 },
            context: 0,
            charge: 0,
            health: 0,
            goal: self.goal(state),
            tier: 0,
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, HoldBand, State};
    use crate::{Keep, SearchSettings, Workload, run_kept, worlds::World};
    use searcher::search::draw::SuffixShape;

    fn config() -> Config {
        Config {
            events: vec![33, 41],
            holds: vec![HoldBand {
                minimum: 8,
                maximum: 8,
                weight: 1,
            }],
        }
    }

    #[test]
    fn elapsed_time_is_restored_and_hidden_between_events() {
        let w = config();
        let root = w.initial();
        let later = w.step(w.step(root, 8), 8);
        assert_eq!(later.elapsed, 16);
        assert_eq!(w.key(root, false), w.key(later, false));
        let visible = w.step(State { elapsed: 32 }, 8);
        assert_ne!(w.key(root, false), w.key(visible, false));
        assert_eq!(w.key(root, true), w.key(visible, true));
        assert_eq!(w.step(root, 8).elapsed, 8);
        assert_eq!(w.step(State { elapsed: 40 }, 8).elapsed, 41);
        assert!(w.goal(w.step(State { elapsed: 40 }, 8)));
    }

    #[test]
    fn malformed_clocks_and_unavailable_holds_are_rejected() {
        let mut w = config();
        assert!(w.reachable().unwrap());
        assert_eq!(w.step(w.initial(), 7), w.initial());
        w.events = vec![0, 3];
        assert!(w.validate().is_err());
        w.events = vec![3, 3];
        assert!(w.validate().is_err());
        w.events = vec![3, 2];
        assert!(w.validate().is_err());
        w.events = vec![3];
        w.holds[0].minimum = 0;
        assert!(w.validate().is_err());
        w.holds[0].minimum = 8;
        w.holds[0].weight = 0;
        assert!(w.validate().is_err());
    }

    #[test]
    fn bounded_suffix_cannot_cross_hidden_clock_but_full_suffix_replays() {
        let workload = Workload {
            config: World::PassiveClock(config()),
            broken: false,
            scale: None,
        };
        for _ in 0..3 {
            let seed = crate::test_seed();
            let bounded = run_kept(
                &workload,
                Keep::Portfolio,
                seed,
                12_000,
                true,
                1,
                SearchSettings {
                    suffix: SuffixShape::OneToSixBounded,
                    ..SearchSettings::default()
                },
            )
            .unwrap();
            assert_eq!(bounded["success"], false);
            assert_eq!(bounded["live_entries"], 1);
            assert!(bounded["evidence"]["passive_clock"]["first_event_work"][0].is_null());
            let full = run_kept(
                &workload,
                Keep::Portfolio,
                seed,
                12_000,
                true,
                1,
                SearchSettings {
                    suffix: SuffixShape::OneToSix,
                    ..SearchSettings::default()
                },
            )
            .unwrap();
            assert_eq!(full["success"], true);
            assert_eq!(full["work"].as_u64().unwrap() % 8, 0);
            assert!(full["first_objective_execution"].as_u64().unwrap() > 0);
            assert!(
                full["evidence"]["passive_clock"]["first_event_execution"][0]
                    .as_u64()
                    .unwrap()
                    <= full["first_objective_execution"].as_u64().unwrap()
            );
        }
    }

    #[test]
    fn weighted_hold_bands_have_their_declared_support_and_mass() {
        let w = Config {
            events: vec![544, 629],
            holds: vec![
                HoldBand {
                    minimum: 2,
                    maximum: 7,
                    weight: 2,
                },
                HoldBand {
                    minimum: 2,
                    maximum: 12,
                    weight: 11,
                },
                HoldBand {
                    minimum: 48,
                    maximum: 120,
                    weight: 11,
                },
            ],
        };
        assert!(w.validate().is_ok());
        let mut rand = searcher::search::rand::RomuDuoJrRand::with_seed(crate::test_seed());
        let mut long = 0;
        for _ in 0..6000 {
            let duration = w.sample(&mut rand);
            assert!(w.valid_action(duration));
            long += usize::from(duration >= 48);
        }
        assert!((2400..3120).contains(&long));
    }

    #[test]
    fn snapshot_restore_keeps_the_clock_separate_from_charged_work() {
        use searcher::search::campaign::TargetExecution;
        let workload: Workload = Workload {
            config: World::PassiveClock(config()),
            broken: false,
            scale: None,
        };
        let mut target = workload.new_target().unwrap();
        let mut milestone = false;
        assert!(
            workload
                .apply_action(&mut target, &0, &mut milestone)
                .is_err()
        );
        workload
            .apply_action(&mut target, &8, &mut milestone)
            .unwrap();
        let snapshot = workload.snapshot(&mut target).unwrap();
        workload
            .apply_action(&mut target, &8, &mut milestone)
            .unwrap();
        workload.restore(&mut target, &snapshot).unwrap();
        assert_eq!(
            target.state,
            crate::worlds::State::PassiveClock(State { elapsed: 8 })
        );
        assert_eq!(workload.execution_work(&target), 16);
        workload
            .apply_action(&mut target, &8, &mut milestone)
            .unwrap();
        assert_eq!(
            target.state,
            crate::worlds::State::PassiveClock(State { elapsed: 16 })
        );
        assert_eq!(workload.execution_work(&target), 24);
    }

    #[test]
    fn visible_clock_removes_the_bounded_suffix_loss() {
        let mut w = config();
        w.events = vec![8, 16, 24, 32, 40, 41];
        let workload = Workload {
            config: World::PassiveClock(w),
            broken: false,
            scale: None,
        };
        for _ in 0..3 {
            let report = run_kept(
                &workload,
                Keep::Portfolio,
                crate::test_seed(),
                12_000,
                true,
                1,
                SearchSettings {
                    suffix: SuffixShape::OneToSixBounded,
                    ..SearchSettings::default()
                },
            )
            .unwrap();
            assert_eq!(report["success"], true);
        }
    }

    #[test]
    fn scaled_calibration_stops_at_its_first_objective_and_reports_ticks_and_tries() {
        let workload = Workload {
            config: World::PassiveClock(Config {
                events: vec![1],
                holds: vec![HoldBand {
                    minimum: 8,
                    maximum: 8,
                    weight: 1,
                }],
            }),
            broken: false,
            scale: Some(crate::Scale {
                memory_budget_mib: 32,
                ..crate::Scale::default()
            }),
        };
        let report = crate::run_scaled(
            &workload,
            crate::test_seed(),
            12_000,
            &mut std::io::sink(),
            SearchSettings {
                stop_on_objective: Some(true),
                ..SearchSettings::default()
            },
        )
        .unwrap();
        assert_eq!(report["success"], true);
        assert_eq!(report["work_unit"], "clock_ticks");
        assert_eq!(report["first_objective_execution"], 1);
        assert_eq!(report["executions"], 1);
        assert_eq!(report["first_objective_work"], 8);
        assert_eq!(report["work"], 8);
        assert_eq!(report["passive_clock"]["held_actions"][8], 1);
    }
}
