// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{Key, actions};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub actions: actions::Config,
    pub initial_time: u8,
    pub action_ticks: [u8; 4],
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub actions: actions::State,
    pub remaining: u8,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        self.actions.validate()?;
        if !(1..=64).contains(&self.initial_time) {
            return Err("initial_time must be in 1..=64".into());
        }
        if self
            .action_ticks
            .iter()
            .any(|ticks| !(1..=16).contains(ticks))
        {
            return Err("each action duration must be in 1..=16".into());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            actions: self.actions.initial(),
            remaining: self.initial_time,
        }
    }

    pub fn state_is_bounded(&self, state: State) -> bool {
        state.remaining <= self.initial_time
            && self.actions.state_is_bounded(state.actions)
            && (!state.actions.goal || self.actions.goal(state.actions))
    }

    pub fn goal(&self, state: State) -> bool {
        self.state_is_bounded(state) && self.actions.goal(state.actions)
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err()
            || !self.state_is_bounded(state)
            || self.goal(state)
            || state.remaining == 0
            || action >= 4
        {
            return state;
        }
        let ticks = self.action_ticks[usize::from(action)];
        if ticks > state.remaining {
            return State {
                remaining: 0,
                ..state
            };
        }
        State {
            actions: self.actions.step(state.actions, action),
            remaining: state.remaining - ticks,
        }
    }

    pub fn key(&self, state: State, _broken: bool) -> Key {
        Key {
            charge: state.remaining,
            ..self.actions.key(state.actions, false)
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
            actions: actions::Config {
                segment_len: 1,
                return_to_land: true,
                observable: true,
                water_action: 1,
            },
            initial_time: 4,
            action_ticks: [1, 2, 1, 1],
        }
    }

    #[test]
    fn changing_actions_and_clock_are_independent_mechanics() {
        let w = config();
        let land = w.step(w.initial(), 0);
        assert_eq!(land.actions.regime, 1);
        let wasted = w.step(land, 0);
        assert_eq!(wasted.actions, land.actions);
        assert_eq!(wasted.remaining, 2);
        let water = w.step(land, 1);
        assert_eq!(water.actions.regime, 2);
        assert_eq!(water.remaining, 1);
        let goal = w.step(water, 0);
        assert!(w.goal(goal));
        assert_eq!(goal.remaining, 0);
        assert_eq!(w.step(goal, 3), goal);
        assert!(!w.goal(w.step(w.step(wasted, 1), 0)));
    }

    #[test]
    fn insufficient_time_never_advances_and_expiration_is_absorbing() {
        let w = config();
        let state = State {
            actions: actions::State {
                position: 0,
                regime: 1,
                goal: false,
            },
            remaining: 1,
        };
        let expired = w.step(state, 1);
        assert_eq!(expired.actions, state.actions);
        assert_eq!(expired.remaining, 0);
        assert_eq!(w.step(expired, 1), expired);
    }

    #[test]
    fn oracle_and_configuration_fields_drive_reachability() {
        let w = config();
        assert!(w.reachable().unwrap());
        assert!(
            !Config {
                initial_time: 3,
                ..w
            }
            .reachable()
            .unwrap()
        );
        assert!(
            !Config {
                action_ticks: [1, 3, 1, 1],
                ..w
            }
            .reachable()
            .unwrap()
        );
        assert!(
            Config {
                actions: actions::Config {
                    return_to_land: false,
                    ..w.actions
                },
                initial_time: 3,
                ..w
            }
            .reachable()
            .unwrap()
        );
        assert!(
            Config {
                initial_time: 0,
                ..w
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                action_ticks: [1, 0, 1, 1],
                ..w
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn control_preserves_clock_and_representation() {
        let w = config();
        let state = w.step(w.initial(), 0);
        assert_eq!(w.key(state, false), w.key(state, true));
        assert_eq!(w.key(state, false).charge, 3);
        assert_ne!(
            w.key(state, false).context,
            w.key(w.initial(), false).context
        );
        assert!(!w.state_is_bounded(State {
            remaining: 5,
            ..state
        }));
    }

    #[test]
    fn schema_rejects_unknown_missing_and_mistyped_fields() {
        let mut v = serde_json::to_value(config()).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
        let mut v = serde_json::to_value(config()).unwrap();
        v.as_object_mut().unwrap().remove("initial_time");
        assert!(serde_json::from_value::<Config>(v).is_err());
        let mut v = serde_json::to_value(config()).unwrap();
        v["action_ticks"] = serde_json::json!([1, 2, 3]);
        assert!(serde_json::from_value::<Config>(v).is_err());
    }
}
