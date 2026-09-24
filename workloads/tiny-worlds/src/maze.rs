// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

const MAX_LENGTH: u8 = 8;
const MAX_STATES: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub pattern: u16,
    pub reverse_actions: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub place: u8,
    pub history: u16,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_LENGTH).contains(&self.length) {
            return Err("length must be between 1 and 8".to_owned());
        }
        if self.pattern >= (1u16 << self.length) {
            return Err("pattern must be less than 1 << length".to_owned());
        }
        let state_bound = usize::from(self.length + 2) * (usize::from(1u16 << self.length)) * 2;
        if state_bound > MAX_STATES {
            return Err("configuration exceeds the 100000-state oracle bound".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            place: 0,
            history: 0,
            goal: false,
        }
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.state_is_bounded(state) || self.goal(state) {
            return state;
        }

        match action {
            0 | 1 if state.place < self.length => {
                let history = state.history | (u16::from(action) << state.place);
                State {
                    place: state.place + 1,
                    history,
                    goal: false,
                }
            }
            0 if state.place == self.length => {
                if state.history == self.expected_raw_history() {
                    State {
                        place: self.length + 1,
                        history: state.history,
                        goal: true,
                    }
                } else {
                    self.initial()
                }
            }
            1 if state.place == self.length => state,
            2 => self.initial(),
            3 => state,
            _ => state,
        }
    }

    pub fn goal(&self, state: State) -> bool {
        self.validate().is_ok()
            && state.goal
            && state.place == self.length + 1
            && state.history == self.expected_raw_history()
    }

    pub fn key(&self, state: State, lossy: bool) -> crate::Key {
        crate::Key {
            place: u16::from(state.place),
            context: if lossy { 0 } else { state.history },
            charge: 0,
            health: 0,
            goal: state.goal,
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;

        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::new();
        let initial = self.initial();
        seen.insert(initial);
        pending.push_back(initial);

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

    fn expected_raw_history(&self) -> u16 {
        let mask = (1u16 << self.length) - 1;
        if self.reverse_actions {
            self.pattern ^ mask
        } else {
            self.pattern
        }
    }

    pub(crate) fn state_is_bounded(&self, state: State) -> bool {
        let history_mask = (1u16 << self.length) - 1;
        state.place <= self.length + 1
            && state.goal == (state.place == self.length + 1)
            && state.history & !history_mask == 0
            && (state.place >= self.length || state.history >> state.place == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, State};

    fn config() -> Config {
        Config {
            length: 2,
            pattern: 1,
            reverse_actions: false,
        }
    }

    fn arrive(config: Config, choices: [u8; 2]) -> State {
        let mut state = config.initial();
        for action in choices {
            state = config.step(state, action);
        }
        state
    }

    #[test]
    fn correct_and_wrong_histories_share_place_but_keys_preserve_context() {
        let config = config();
        let correct = arrive(config, [1, 0]);
        let wrong = arrive(config, [0, 0]);

        assert_eq!(correct.place, config.length);
        assert_eq!(wrong.place, config.length);
        assert_ne!(correct.history, wrong.history);
        assert_eq!(config.key(correct, true), config.key(wrong, true));
        assert_ne!(config.key(correct, false), config.key(wrong, false));
        assert_eq!(config.key(correct, false).context, correct.history);
        assert_eq!(config.key(wrong, false).context, wrong.history);
    }

    #[test]
    fn either_arrival_order_keeps_the_correct_loop_exit_available() {
        let config = config();
        let correct = arrive(config, [1, 0]);
        let wrong = arrive(config, [0, 0]);

        for arrivals in [[correct, wrong], [wrong, correct]] {
            let first_result = config.step(arrivals[0], 0);
            let second_result = config.step(arrivals[1], 0);
            assert_eq!(config.goal(first_result), arrivals[0] == correct);
            assert_eq!(config.goal(second_result), arrivals[1] == correct);
        }
    }

    #[test]
    fn wrong_submission_and_explicit_reset_return_to_start() {
        let config = config();
        let wrong = arrive(config, [0, 0]);
        assert_eq!(config.step(wrong, 0), config.initial());

        let midway = config.step(config.initial(), 1);
        assert_eq!(config.step(midway, 2), config.initial());

        let retried = arrive(config, [1, 0]);
        assert!(config.goal(config.step(retried, 0)));
    }

    #[test]
    fn reverse_actions_changes_the_raw_history_needed_for_the_same_pattern() {
        let normal = config();
        let reversed = Config {
            reverse_actions: true,
            ..normal
        };

        let normal_goal = normal.step(arrive(normal, [1, 0]), 0);
        let reversed_goal = reversed.step(arrive(reversed, [0, 1]), 0);
        assert!(normal.goal(normal_goal));
        assert!(reversed.goal(reversed_goal));
        assert_ne!(normal_goal.history, reversed_goal.history);
        assert!(!normal.goal(normal.step(arrive(normal, [0, 1]), 0)));
        assert!(!reversed.goal(reversed.step(arrive(reversed, [1, 0]), 0)));
    }

    #[test]
    fn state_snapshot_round_trips() {
        let state = State {
            place: 2,
            history: 1,
            goal: false,
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn config_schema_requires_every_typed_field_and_rejects_extras() {
        let valid = r#"{"length":2,"pattern":1,"reverse_actions":false}"#;
        assert!(serde_json::from_str::<Config>(valid).is_ok());
        assert!(serde_json::from_str::<Config>(r#"{"length":2,"pattern":1}"#).is_err());
        assert!(
            serde_json::from_str::<Config>(r#"{"length":"2","pattern":1,"reverse_actions":false}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(
                r#"{"length":2,"pattern":1,"reverse_actions":false,"extra":0}"#
            )
            .is_err()
        );
    }

    #[test]
    fn validation_bounds_length_and_pattern() {
        let base = config();
        assert!(base.validate().is_ok());
        assert!(Config { length: 0, ..base }.validate().is_err());
        assert!(Config { length: 9, ..base }.validate().is_err());
        assert!(
            Config {
                length: 8,
                pattern: 255,
                ..base
            }
            .validate()
            .is_ok()
        );
        assert!(
            Config {
                length: 8,
                pattern: 256,
                ..base
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn reachability_bfs_matches_independent_raw_sequence_enumeration() {
        for length in 1..=4 {
            let all_bits = 1u16 << length;
            let mask = all_bits - 1;
            for pattern in 0..all_bits {
                for reverse_actions in [false, true] {
                    let config = Config {
                        length,
                        pattern,
                        reverse_actions,
                    };
                    let enumerated = (0..all_bits).any(|raw| {
                        let physical = if reverse_actions { raw ^ mask } else { raw };
                        physical == pattern
                    });
                    assert_eq!(config.reachable().unwrap(), enumerated);
                }
            }
        }
    }
}
