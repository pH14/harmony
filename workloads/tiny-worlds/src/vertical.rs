// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use serde::{Deserialize, Serialize};

pub const UP: u8 = 1;
pub const RIGHT: u8 = 2;
pub const CONTROLLER_MASK: u8 = UP | RIGHT;
pub const LONGEST_HOLD: u8 = 64;

pub fn action(buttons: u8, frames: u8) -> u8 {
    assert!(buttons <= CONTROLLER_MASK && (1..=LONGEST_HOLD).contains(&frames));
    ((frames - 1) << 2) | buttons
}

pub fn duration(action: u8) -> u8 {
    (action >> 2) + 1
}

pub fn step_buttons(rand: &mut searcher::search::rand::RomuDuoJrRand, previous: u8) -> u8 {
    match rand.below(std::num::NonZeroUsize::new(3).unwrap()) {
        0 => previous ^ UP,
        1 => previous ^ RIGHT,
        _ => previous,
    }
}

pub fn sample_hold(rand: &mut searcher::search::rand::RomuDuoJrRand) -> u8 {
    2 + rand.below(std::num::NonZeroUsize::new(usize::from(LONGEST_HOLD - 1)).unwrap()) as u8
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub height: u16,
    pub exit_height: u16,
    pub exit_width: u16,
    pub exit_distance: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct State {
    pub height: u16,
    pub across: u32,
    pub goal: bool,
}

impl Config {
    pub fn validate(self) -> Result<(), String> {
        if self.height == 0 || self.exit_height == 0 || self.exit_height > self.height {
            return Err("exit height must be positive and within the screen".into());
        }
        if self.exit_width == 0 || self.exit_width > self.exit_height {
            return Err("exit width must be positive and no greater than exit height".into());
        }
        if self.exit_distance == 0 || self.exit_distance > self.exit_width {
            return Err("exit distance must be positive and fit the falling exit window".into());
        }
        Ok(())
    }

    pub fn reachable(self) -> Result<bool, String> {
        self.validate()?;
        let mut state = State::default();
        for _ in 0..self.exit_height {
            state = self.step(state, UP);
        }
        for _ in 0..self.exit_distance {
            state = self.step(state, RIGHT);
        }
        Ok(state.goal)
    }

    pub fn valid_state(self, state: State) -> bool {
        self.validate().is_ok()
            && state.height <= self.height
            && state.across <= u32::from(self.exit_distance) + 1
            && (!state.goal || state.across == u32::from(self.exit_distance))
    }

    pub fn apply(self, mut state: State, held: u8) -> State {
        for _ in 0..duration(held) {
            state = self.step(state, held & CONTROLLER_MASK);
        }
        state
    }

    pub fn step(self, mut state: State, buttons: u8) -> State {
        if state.goal {
            return state;
        }
        if buttons == UP {
            state.height = state.height.saturating_add(1).min(self.height);
            state.across = 0;
        } else {
            let in_exit = state.height <= self.exit_height
                && state.height > self.exit_height - self.exit_width;
            state.across = if buttons == RIGHT {
                (state.across + 1).min(u32::from(self.exit_distance) + 1)
            } else {
                0
            };
            state.height = state.height.saturating_sub(1);
            state.goal = in_exit && state.across == u32::from(self.exit_distance);
        }
        state
    }

    pub fn key(self, state: State) -> Key {
        Key {
            place: 0,
            context: 0,
            tier: 0,
            stock: 0,
            charge: 0,
            health: 1,
            goal: state.goal,
        }
    }

    pub fn observation(self, state: State) -> Vec<u64> {
        vec![
            u64::from(state.height),
            u64::from(state.across),
            u64::from(state.goal),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Config {
        Config {
            height: 720,
            exit_height: 680,
            exit_width: 12,
            exit_distance: 8,
        }
    }

    #[test]
    fn all_input_modes_replay_and_account_for_work() {
        use searcher::search::draw::{InputMode, draw_mixture_from_identifier};
        let workload = crate::Workload {
            config: crate::worlds::World::Vertical(Config {
                height: 160,
                exit_height: 140,
                exit_width: 12,
                exit_distance: 8,
            }),
            broken: false,
            scale: None,
        };
        for input_mode in [
            InputMode::Current,
            InputMode::Chaining,
            InputMode::Sticky,
            InputMode::StickyChaining,
        ] {
            let report = crate::run_kept(
                &workload,
                crate::Keep::Portfolio,
                26102803,
                20_000,
                true,
                1,
                crate::SearchSettings {
                    input_mode,
                    mixture: Some(draw_mixture_from_identifier("energy_splice:6").unwrap()),
                    stop_on_objective: Some(false),
                },
            )
            .unwrap();
            assert_eq!(report["verified"], true);
            let counts: Vec<u64> =
                serde_json::from_value(report["input_strategy_jobs"].clone()).unwrap();
            assert_eq!(
                counts.iter().sum::<u64>(),
                report["executions"].as_u64().unwrap()
            );
            if input_mode.chaining() {
                assert!(counts[2] + counts[3] > 0);
            }
            if input_mode.sticky() {
                assert!(counts[1] + counts[3] > 0);
            }
        }
    }

    #[test]
    fn held_actions_round_trip_and_match_single_frames() {
        let config = layout();
        for buttons in 0..=CONTROLLER_MASK {
            for frames in 1..=LONGEST_HOLD {
                let held = action(buttons, frames);
                assert_eq!(duration(held), frames);
                assert_eq!(held & CONTROLLER_MASK, buttons);
                let start = State {
                    height: config.exit_height,
                    ..State::default()
                };
                let mut expected = start;
                for _ in 0..frames {
                    expected = config.step(expected, buttons);
                }
                assert_eq!(config.apply(start, held), expected);
            }
        }
    }

    #[test]
    fn runner_replays_a_single_cell_climb_and_its_exit() {
        let workload = crate::Workload {
            config: crate::worlds::World::Vertical(Config {
                height: 8,
                exit_height: 6,
                exit_width: 3,
                exit_distance: 2,
            }),
            broken: false,
            scale: None,
        };
        let report = crate::run(&workload, 26102802, 20_000, true).unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["success"], true);
    }

    #[test]
    fn a_long_climb_and_short_exact_move_reach_the_exit_without_intermediate_cells() {
        let config = layout();
        let root = State::default();
        let mut state = root;
        for _ in 0..config.exit_height {
            state = config.step(state, UP);
            assert_eq!(config.key(state), config.key(root));
            assert!(config.valid_state(state));
        }
        assert_ne!(config.observation(state), config.observation(root));
        for _ in 0..config.exit_distance {
            state = config.step(state, RIGHT);
            assert!(config.valid_state(state));
        }
        assert!(state.goal);
        for buttons in 0..=CONTROLLER_MASK {
            assert_eq!(config.step(state, buttons), state);
        }
    }

    #[test]
    fn wrong_height_or_simultaneous_controls_do_not_open_the_exit() {
        let config = layout();
        for buttons in [RIGHT, UP | RIGHT] {
            let mut state = State {
                height: config.height,
                ..State::default()
            };
            for _ in 0..config.height {
                state = config.step(state, buttons);
                assert!(config.valid_state(state));
            }
            assert!(!state.goal);
        }
        let mut state = State {
            height: config.exit_height,
            ..State::default()
        };
        for _ in 0..config.exit_distance {
            state = config.step(state, UP | RIGHT);
        }
        assert!(!state.goal);
    }

    #[test]
    fn every_valid_layout_has_a_constructed_witness_and_no_clock_progress() {
        for height in 1..=16 {
            for exit_height in 1..=height {
                for exit_width in 1..=exit_height {
                    for exit_distance in 1..=exit_width {
                        let config = Config {
                            height,
                            exit_height,
                            exit_width,
                            exit_distance,
                        };
                        config.validate().unwrap();
                        let root = State::default();
                        assert_eq!(config.step(root, 0), root);
                        let mut state = root;
                        for _ in 0..exit_height {
                            state = config.step(state, UP);
                        }
                        for _ in 0..exit_distance {
                            state = config.step(state, RIGHT);
                        }
                        assert!(state.goal && config.valid_state(state));
                    }
                }
            }
        }
    }
}
