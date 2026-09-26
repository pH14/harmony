// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub segment_len: u8,
    pub return_to_land: bool,
    pub observable: bool,
    pub water_action: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub regime: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=8).contains(&self.segment_len) {
            return Err("segment_len must be between 1 and 8".to_owned());
        }
        if !(1..=3).contains(&self.water_action) {
            return Err("water_action must be between 1 and 3".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            position: 0,
            regime: 0,
            goal: false,
        }
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.state_is_bounded(state) || state.goal {
            return state;
        }

        let expected_action = match state.regime {
            0 | 2 => 0,
            1 => self.water_action,
            _ => return state,
        };
        if action != expected_action {
            return state;
        }

        let position = state.position + 1;
        if position < self.segment_len {
            return State { position, ..state };
        }

        if state.regime < self.final_regime() {
            State {
                position: 0,
                regime: state.regime + 1,
                goal: false,
            }
        } else {
            State {
                position: self.segment_len,
                goal: true,
                ..state
            }
        }
    }

    pub fn goal(&self, state: State) -> bool {
        self.state_is_bounded(state)
            && state.goal
            && state.position == self.segment_len
            && state.regime == self.final_regime()
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }

    pub fn key(&self, state: State, _lossy: bool) -> crate::Key {
        crate::Key {
            stock: 0,
            place: u16::from(state.position),
            context: if self.observable {
                u16::from(state.regime)
            } else {
                0
            },
            charge: 0,
            health: 0,
            goal: state.goal,
            tier: 0,
        }
    }

    fn final_regime(&self) -> u8 {
        if self.return_to_land { 2 } else { 1 }
    }

    pub(crate) fn state_is_bounded(&self, state: State) -> bool {
        state.position <= self.segment_len
            && state.regime <= self.final_regime()
            && (!state.goal
                || (state.position == self.segment_len && state.regime == self.final_regime()))
            && (state.goal || state.position < self.segment_len)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, State};

    fn config(segment_len: u8, return_to_land: bool, observable: bool, water_action: u8) -> Config {
        Config {
            segment_len,
            return_to_land,
            observable,
            water_action,
        }
    }

    #[test]
    fn land_water_sequence_switches_actions_and_ends_on_water_crossing() {
        let world = config(2, false, true, 2);
        let first = world.step(world.initial(), 0);
        assert_eq!(
            first,
            State {
                position: 1,
                regime: 0,
                goal: false
            }
        );
        let water = world.step(first, 0);
        assert_eq!(
            water,
            State {
                position: 0,
                regime: 1,
                goal: false
            }
        );
        assert_eq!(world.step(water, 0), water);
        let last = world.step(world.step(water, 2), 2);
        assert_eq!(
            last,
            State {
                position: 2,
                regime: 1,
                goal: true
            }
        );
        assert!(world.goal(last));
    }

    #[test]
    fn land_water_land_sequence_returns_to_land_before_goal() {
        let world = config(1, true, true, 1);
        let water = world.step(world.initial(), 0);
        assert_eq!(
            water,
            State {
                position: 0,
                regime: 1,
                goal: false
            }
        );
        let last_land = world.step(water, 1);
        assert_eq!(
            last_land,
            State {
                position: 0,
                regime: 2,
                goal: false
            }
        );
        let goal = world.step(last_land, 0);
        assert_eq!(
            goal,
            State {
                position: 1,
                regime: 2,
                goal: true
            }
        );
        assert!(world.goal(goal));
    }

    #[test]
    fn action_effectiveness_switches_in_water_and_recovers_on_land() {
        let world = config(1, true, true, 2);
        let water = world.step(world.initial(), 0);
        assert_eq!(world.step(water, 0), water);
        let last_land = world.step(water, 2);
        assert_eq!(last_land.regime, 2);
        assert_eq!(world.step(last_land, 2), last_land);
        assert!(world.goal(world.step(last_land, 0)));
    }

    #[test]
    fn each_registered_water_action_advances_only_in_water() {
        for water_action in 1..=3 {
            let world = config(1, false, true, water_action);
            let water = world.step(world.initial(), 0);
            for action in 1..=3 {
                let next = world.step(water, action);
                if action == water_action {
                    assert!(world.goal(next));
                } else {
                    assert_eq!(next, water);
                }
            }
        }
    }

    #[test]
    fn observability_changes_representation_without_changing_mechanics() {
        let visible = config(2, true, true, 3);
        let hidden = config(2, true, false, 3);
        let land = State {
            position: 0,
            regime: 0,
            goal: false,
        };
        let water = State {
            position: 0,
            regime: 1,
            goal: false,
        };

        for action in 0..=3 {
            assert_eq!(visible.step(land, action), hidden.step(land, action));
            assert_eq!(visible.step(water, action), hidden.step(water, action));
        }
        assert_ne!(visible.key(land, false), visible.key(water, false));
        assert_eq!(hidden.key(land, false), hidden.key(water, false));
        assert_eq!(visible.key(land, false), visible.key(land, true));
    }

    #[test]
    fn unsignaled_regime_aliasing_can_hide_the_action_switch_from_both_arms() {
        let world = config(1, false, false, 2);
        let water = world.step(world.initial(), 0);
        assert_eq!(water.regime, 1);
        assert_eq!(world.key(world.initial(), false), world.key(water, false));
        assert_eq!(world.step(water, 0), water);
        assert!(world.goal(world.step(water, 2)));
    }

    #[test]
    fn bounded_reachability_finds_both_sequence_shapes_and_all_water_actions() {
        for return_to_land in [false, true] {
            for water_action in 1..=3 {
                assert!(
                    config(8, return_to_land, true, water_action)
                        .reachable()
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn configuration_schema_requires_typed_fields_and_rejects_extras() {
        let valid =
            r#"{"segment_len":2,"return_to_land":true,"observable":false,"water_action":3}"#;
        assert!(serde_json::from_str::<Config>(valid).is_ok());
        assert!(
            serde_json::from_str::<Config>(
                r#"{"segment_len":2,"return_to_land":true,"observable":false}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(
                r#"{"segment_len":"2","return_to_land":true,"observable":false,"water_action":3}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Config>(
            r#"{"segment_len":2,"return_to_land":true,"observable":false,"water_action":3,"unused":1}"#
        )
        .is_err());
    }

    #[test]
    fn configuration_rejects_out_of_range_values() {
        assert!(config(0, false, true, 1).validate().is_err());
        assert!(config(9, false, true, 1).validate().is_err());
        assert!(config(1, false, true, 0).validate().is_err());
        assert!(config(1, false, true, 4).validate().is_err());
    }
}
