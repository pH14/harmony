// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Sequence,
    Wait,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Identity,
    Place,
    Engaged,
    Tier,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub horizon: u8,
    pub distractions: u8,
    pub mode: Mode,
    pub placement: Placement,
    pub sticky_credit: bool,
    pub ammo: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub lane: u8,
    pub progress: u8,
    pub memory: u8,
    pub ammo: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.horizon) {
            return Err("horizon must be between 1 and 16".to_owned());
        }
        if !(1..=32).contains(&self.distractions) {
            return Err("distractions must be between 1 and 32".to_owned());
        }
        if self.ammo > 31 || (self.ammo > 0 && self.ammo < self.horizon) {
            return Err("ammo must be 0 or between the horizon and 31".to_owned());
        }
        let memory_levels = if self.sticky_credit {
            usize::from(self.horizon) + 1
        } else {
            1
        };
        let state_bound = (usize::from(self.distractions) + 1)
            * (usize::from(self.horizon) + 1)
            * memory_levels
            * (usize::from(self.ammo) + 1)
            * 2;
        if state_bound > crate::MAX_REACHABLE_STATES {
            return Err("configuration exceeds the 100000-state oracle bound".to_owned());
        }
        if self.sticky_credit && matches!(self.placement, Placement::Identity) {
            return Err("sticky_credit requires a placement other than identity".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            lane: 0,
            progress: 0,
            memory: 0,
            ammo: self.ammo,
            goal: false,
        }
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.state_is_bounded(state) || self.goal(state) {
            return state;
        }
        if action > 3 {
            return state;
        }

        if state.lane == 0 {
            if action == 1 {
                return State {
                    lane: 1,
                    progress: 0,
                    memory: if self.sticky_credit {
                        state.progress
                    } else {
                        0
                    },
                    ..state
                };
            }
            let correct_action = match self.mode {
                Mode::Sequence => 0,
                Mode::Wait => 3,
            };
            let ammo = state.ammo.saturating_sub(u8::from(self.ammo > 0));
            if action != correct_action {
                return State {
                    progress: if self.ammo > 0 { state.progress } else { 0 },
                    ammo,
                    ..state
                };
            }
            if self.ammo > 0 && state.ammo == 0 {
                return state;
            }
            let progress = state.progress + 1;
            return State {
                progress,
                ammo,
                goal: progress == self.horizon,
                ..state
            };
        }

        match action {
            0 | 1 => State {
                lane: if state.lane == self.distractions {
                    1
                } else {
                    state.lane + 1
                },
                ..state
            },
            2 => State {
                lane: 0,
                memory: 0,
                ..state
            },
            3 => State {
                ammo: (state.ammo + 1).min(self.ammo),
                ..state
            },
            _ => state,
        }
    }

    pub fn goal(&self, state: State) -> bool {
        self.validate().is_ok()
            && self.state_is_bounded(state)
            && state.goal
            && state.lane == 0
            && state.progress == self.horizon
    }

    pub fn level(&self, state: State) -> u8 {
        if state.lane == 0 {
            state.progress
        } else if self.sticky_credit {
            state.memory
        } else {
            0
        }
    }

    pub fn key(&self, state: State, lossy: bool) -> crate::Key {
        let level = if lossy { 0 } else { self.level(state) };
        let spread = u16::from(state.lane) * (u16::from(self.horizon) + 1) + u16::from(level);
        let (place, context, tier) = match self.placement {
            Placement::Identity => (u16::from(state.lane), u16::from(level), 0),
            Placement::Place => (spread, 0, 0),
            Placement::Engaged => (spread, 0, u16::from(level > 0)),
            Placement::Tier => (u16::from(state.lane), 0, u16::from(level)),
        };
        crate::Key {
            stock: 0,
            place,
            context,
            charge: state.ammo,
            health: 0,
            goal: state.goal,
            tier,
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }

    pub(crate) fn state_is_bounded(&self, state: State) -> bool {
        state.lane <= self.distractions
            && state.ammo <= self.ammo
            && (self.sticky_credit || state.memory == 0)
            && if state.lane == 0 {
                state.progress <= self.horizon
                    && state.goal == (state.progress == self.horizon)
                    && state.memory == 0
            } else {
                state.progress == 0 && !state.goal && state.memory < self.horizon
            }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Mode, Placement, State};

    fn config(horizon: u8, distractions: u8, mode: Mode) -> Config {
        Config {
            horizon,
            distractions,
            mode,
            placement: Placement::Identity,
            sticky_credit: false,
            ammo: 0,
        }
    }

    #[test]
    fn sequence_mode_advances_on_zero_and_resets_on_other_useful_actions() {
        let world = config(3, 2, Mode::Sequence);
        let one = world.step(world.initial(), 0);
        assert_eq!(one.progress, 1);
        assert_eq!(one.lane, 0);
        assert_eq!(world.step(one, 2), world.initial());
        assert_eq!(world.step(one, 3), world.initial());
        let goal = world.step(world.step(world.initial(), 0), 0);
        assert_eq!(goal.progress, 2);
        assert!(!world.goal(goal));
        assert!(world.goal(world.step(goal, 0)));
    }

    #[test]
    fn wait_mode_advances_only_on_action_three() {
        let world = config(2, 1, Mode::Wait);
        let one = world.step(world.initial(), 3);
        assert_eq!(one.progress, 1);
        assert_eq!(world.step(one, 0), world.initial());
        assert_eq!(world.step(one, 2), world.initial());
        let goal = world.step(one, 3);
        assert_eq!(goal.progress, 2);
        assert!(world.goal(goal));
    }

    #[test]
    fn distraction_cycle_resets_progress_and_returns_to_useful_lane() {
        let world = config(3, 3, Mode::Sequence);
        let partial = world.step(world.initial(), 0);
        let first = world.step(partial, 1);
        assert_eq!(
            first,
            State {
                lane: 1,
                progress: 0,
                memory: 0,
                ammo: 0,
                goal: false
            }
        );
        let second = world.step(first, 0);
        let third = world.step(second, 1);
        assert_eq!(second.lane, 2);
        assert_eq!(third.lane, 3);
        assert_eq!(world.step(third, 0).lane, 1);
        assert_eq!(world.step(first, 3), first);
        let returned = world.step(third, 2);
        assert_eq!(returned, world.initial());
        assert_eq!(world.step(returned, 0).progress, 1);
    }

    #[test]
    fn goal_is_absorbing_and_invalid_actions_leave_state_unchanged() {
        let world = config(1, 1, Mode::Sequence);
        let goal = world.step(world.initial(), 0);
        assert!(world.goal(goal));
        assert_eq!(world.step(goal, 1), goal);
        let initial = world.initial();
        assert_eq!(world.step(initial, 4), initial);
        let distraction = world.step(initial, 1);
        assert_eq!(world.step(distraction, 4), distraction);
    }

    #[test]
    fn normal_key_preserves_progress_and_broken_key_collapses_it() {
        let world = config(3, 2, Mode::Sequence);
        let first = world.step(world.initial(), 0);
        let second = world.step(first, 0);
        assert_eq!(first.lane, second.lane);
        assert_ne!(world.key(first, false), world.key(second, false));
        assert_eq!(world.key(first, true), world.key(second, true));
        assert_eq!(world.key(first, true).context, 0);
        assert_eq!(world.key(first, false).context, 1);
    }

    #[test]
    fn bounded_reachability_finds_both_modes_at_maximum_parameters() {
        for mode in [Mode::Sequence, Mode::Wait] {
            assert!(config(16, 32, mode).reachable().unwrap());
        }
    }

    #[test]
    fn configuration_schema_is_strict_and_mode_is_snake_case() {
        let valid = r#"{"horizon":4,"distractions":3,"mode":"sequence","placement":"identity","sticky_credit":false,"ammo":0}"#;
        assert!(serde_json::from_str::<Config>(valid).is_ok());
        assert!(
            serde_json::from_str::<Config>(
                r#"{"horizon":4,"mode":"sequence","placement":"identity","sticky_credit":false,"ammo":0}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(r#"{"horizon":"4","distractions":3,"mode":"sequence","placement":"identity","sticky_credit":false,"ammo":0}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(r#"{"horizon":4,"distractions":3,"mode":"Sequence","placement":"identity","sticky_credit":false,"ammo":0}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(
                r#"{"horizon":4,"distractions":3,"mode":"sequence","extra":1,"placement":"identity","sticky_credit":false,"ammo":0}"#
            )
            .is_err()
        );
    }

    #[test]
    fn state_schema_requires_typed_fields_and_rejects_unknown_fields() {
        let valid = r#"{"lane":1,"progress":0,"memory":0,"ammo":0,"goal":false}"#;
        assert!(serde_json::from_str::<State>(valid).is_ok());
        assert!(
            serde_json::from_str::<State>(r#"{"lane":1,"progress":0,"memory":0,"ammo":0}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<State>(
                r#"{"lane":"1","progress":0,"memory":0,"ammo":0,"goal":false}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<State>(
                r#"{"lane":1,"progress":0,"memory":0,"ammo":0,"goal":false,"extra":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn validation_and_state_bounds_reject_invalid_values() {
        let base = config(1, 1, Mode::Sequence);
        assert!(base.validate().is_ok());
        assert!(config(0, 1, Mode::Sequence).validate().is_err());
        assert!(config(17, 1, Mode::Sequence).validate().is_err());
        assert!(config(1, 0, Mode::Sequence).validate().is_err());
        assert!(config(1, 33, Mode::Sequence).validate().is_err());
        assert!(!base.state_is_bounded(State {
            lane: 0,
            progress: 1,
            memory: 0,
            ammo: 0,
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 0,
            progress: 2,
            memory: 0,
            ammo: 0,
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 1,
            progress: 1,
            memory: 0,
            ammo: 0,
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 1,
            progress: 0,
            memory: 0,
            ammo: 0,
            goal: true
        }));
        assert!(base.state_is_bounded(State {
            lane: 0,
            progress: 1,
            memory: 0,
            ammo: 0,
            goal: true
        }));
    }

    #[test]
    fn placements_put_partial_progress_in_identity_place_or_tier() {
        let base = config(3, 2, Mode::Sequence);
        let one = base.step(base.initial(), 0);
        let two = base.step(one, 0);
        let cases = [
            (Placement::Identity, (0, 1, 0), (0, 2, 0)),
            (Placement::Place, (1, 0, 0), (2, 0, 0)),
            (Placement::Engaged, (1, 0, 1), (2, 0, 1)),
            (Placement::Tier, (0, 0, 1), (0, 0, 2)),
        ];
        for (placement, first, second) in cases {
            let world = Config { placement, ..base };
            for (state, (place, context, tier)) in [(one, first), (two, second)] {
                let key = world.key(state, false);
                assert_eq!((key.place, key.context, key.tier), (place, context, tier));
                let hidden = world.key(state, true);
                assert_eq!((hidden.context, hidden.tier), (0, 0));
            }
            let start = world.key(world.initial(), false);
            assert_eq!((start.place, start.context, start.tier), (0, 0, 0));
        }
    }

    #[test]
    fn sticky_credit_keeps_the_reading_only_in_the_key() {
        let lane = Config {
            placement: Placement::Engaged,
            ..config(3, 2, Mode::Sequence)
        };
        let sticky = Config {
            sticky_credit: true,
            ..lane
        };
        assert!(
            Config {
                placement: Placement::Identity,
                ..sticky
            }
            .validate()
            .is_err()
        );
        let two = sticky.step(sticky.step(sticky.initial(), 0), 0);
        let left = sticky.step(two, 1);
        let further = sticky.step(left, 0);
        assert_eq!(
            left,
            State {
                memory: 2,
                ..lane.step(two, 1)
            }
        );
        assert_eq!(lane.step(two, 1).memory, 0);
        assert_eq!((further.lane, further.progress, further.memory), (2, 0, 2));
        assert_eq!(sticky.level(further), 2);
        assert_eq!(lane.level(further), 0);
        assert_eq!(sticky.key(further, false).tier, 1);
        assert_eq!(lane.key(further, false).tier, 0);
        assert_ne!(
            sticky.key(further, false).place,
            sticky
                .key(sticky.step(sticky.step(sticky.initial(), 1), 0), false)
                .place
        );
        let back = sticky.step(further, 2);
        assert_eq!(back, sticky.initial());
        assert_eq!(sticky.key(back, false).tier, 0);
        assert!(sticky.reachable().unwrap());
    }

    #[test]
    fn ammo_is_spent_on_every_fight_action_and_refilled_only_in_distraction_lanes() {
        let world = Config {
            placement: Placement::Tier,
            ammo: 2,
            ..config(2, 2, Mode::Sequence)
        };
        assert_eq!(world.initial().ammo, 2);
        let one = world.step(world.initial(), 0);
        assert_eq!((one.progress, one.ammo), (1, 1));
        assert_eq!(world.key(one, false).charge, 1);
        let missed = world.step(one, 2);
        assert_eq!((missed.progress, missed.ammo), (1, 0));
        assert_eq!(world.step(missed, 0), missed);
        assert_eq!(world.step(missed, 3), missed);
        let away = world.step(missed, 1);
        assert_eq!((away.lane, away.ammo, away.memory), (1, 0, 0));
        let refilled = world.step(world.step(world.step(away, 3), 3), 3);
        assert_eq!(refilled.ammo, 2);
        let back = world.step(refilled, 2);
        assert_eq!((back.lane, back.progress, back.ammo), (0, 0, 2));
        assert!(world.goal(world.step(world.step(back, 0), 0)));
        assert!(world.reachable().unwrap());
        assert!(!world.state_is_bounded(State {
            ammo: 3,
            ..world.initial()
        }));
        assert!(!world.state_is_bounded(State { memory: 1, ..away }));
        assert!(Config { ammo: 1, ..world }.validate().is_err());
        assert!(Config { ammo: 32, ..world }.validate().is_err());
    }

    #[test]
    fn every_placement_replays_at_fixed_work() {
        for placement in [
            Placement::Identity,
            Placement::Place,
            Placement::Engaged,
            Placement::Tier,
        ] {
            for (sticky_credit, ammo) in [(false, 0), (true, 0), (false, 6)] {
                let world = Config {
                    placement,
                    sticky_credit,
                    ammo,
                    ..config(6, 8, Mode::Sequence)
                };
                if world.validate().is_err() {
                    continue;
                }
                let workload = crate::Workload {
                    config: crate::worlds::World::Delayed(world),
                    broken: false,
                    scale: None,
                };
                let report = crate::run(&workload, crate::test_seed(), 2000, true).unwrap();
                assert_eq!(report["verified"], true);
            }
        }
    }
}
