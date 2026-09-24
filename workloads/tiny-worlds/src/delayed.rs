// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

const MAX_STATES: usize = 100_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Sequence,
    Wait,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub horizon: u8,
    pub distractions: u8,
    pub mode: Mode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub lane: u8,
    pub progress: u8,
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
        let state_bound =
            (usize::from(self.distractions) + 1) * (usize::from(self.horizon) + 1) * 2;
        if state_bound > MAX_STATES {
            return Err("configuration exceeds the 100000-state oracle bound".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            lane: 0,
            progress: 0,
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
                    goal: false,
                };
            }
            let correct_action = match self.mode {
                Mode::Sequence => 0,
                Mode::Wait => 3,
            };
            if action != correct_action {
                return State {
                    progress: 0,
                    ..state
                };
            }
            let progress = state.progress + 1;
            return State {
                progress,
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
                progress: 0,
                goal: false,
            },
            2 => State {
                lane: 0,
                progress: 0,
                goal: false,
            },
            3 => state,
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

    pub fn key(&self, state: State, lossy: bool) -> crate::Key {
        crate::Key {
            stock: 0,
            place: u16::from(state.lane),
            context: if lossy { 0 } else { u16::from(state.progress) },
            charge: 0,
            health: 0,
            goal: state.goal,
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;

        let initial = self.initial();
        let mut seen = BTreeSet::from([initial]);
        let mut pending = VecDeque::from([initial]);
        while let Some(state) = pending.pop_front() {
            if self.goal(state) {
                return Ok(true);
            }
            for action in 0..=3 {
                let next = self.step(state, action);
                if seen.insert(next) {
                    if seen.len() > MAX_STATES {
                        return Err("reachability search exceeded 100000 states".to_owned());
                    }
                    pending.push_back(next);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn state_is_bounded(&self, state: State) -> bool {
        state.lane <= self.distractions
            && if state.lane == 0 {
                state.progress <= self.horizon && state.goal == (state.progress == self.horizon)
            } else {
                state.progress == 0 && !state.goal
            }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Mode, State};

    fn config(horizon: u8, distractions: u8, mode: Mode) -> Config {
        Config {
            horizon,
            distractions,
            mode,
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
        let valid = r#"{"horizon":4,"distractions":3,"mode":"sequence"}"#;
        assert!(serde_json::from_str::<Config>(valid).is_ok());
        assert!(serde_json::from_str::<Config>(r#"{"horizon":4,"mode":"sequence"}"#).is_err());
        assert!(
            serde_json::from_str::<Config>(r#"{"horizon":"4","distractions":3,"mode":"sequence"}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(r#"{"horizon":4,"distractions":3,"mode":"Sequence"}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(
                r#"{"horizon":4,"distractions":3,"mode":"sequence","extra":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn state_schema_requires_typed_fields_and_rejects_unknown_fields() {
        let valid = r#"{"lane":1,"progress":0,"goal":false}"#;
        assert!(serde_json::from_str::<State>(valid).is_ok());
        assert!(serde_json::from_str::<State>(r#"{"lane":1,"progress":0}"#).is_err());
        assert!(
            serde_json::from_str::<State>(r#"{"lane":"1","progress":0,"goal":false}"#).is_err()
        );
        assert!(
            serde_json::from_str::<State>(r#"{"lane":1,"progress":0,"goal":false,"extra":1}"#)
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
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 0,
            progress: 2,
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 1,
            progress: 1,
            goal: false
        }));
        assert!(!base.state_is_bounded(State {
            lane: 1,
            progress: 0,
            goal: true
        }));
        assert!(base.state_is_bounded(State {
            lane: 0,
            progress: 1,
            goal: true
        }));
    }
}
