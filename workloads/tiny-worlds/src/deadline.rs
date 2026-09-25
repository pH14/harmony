// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

const MAX_LENGTH: u8 = 8;
const MAX_INITIAL_TIME: u8 = 64;
const MAX_FAST_TICKS: u8 = 4;
const MAX_ROUTE_TICKS: u8 = 16;
const MAX_OBSTACLE_TICKS: u8 = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub initial_time: u8,
    pub fast_ticks: u8,
    pub slow_ticks: u8,
    pub obstacle_ticks: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub phase: u8,
    pub remaining: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_LENGTH).contains(&self.length) {
            return Err("length must be between 1 and 8".to_owned());
        }
        if !(1..=MAX_INITIAL_TIME).contains(&self.initial_time) {
            return Err("initial_time must be between 1 and 64".to_owned());
        }
        if !(1..=MAX_FAST_TICKS).contains(&self.fast_ticks) {
            return Err("fast_ticks must be between 1 and 4".to_owned());
        }
        if !(u16::from(self.fast_ticks) * 2 < u16::from(self.slow_ticks)
            && self.slow_ticks <= MAX_ROUTE_TICKS)
        {
            return Err(
                "slow_ticks must be greater than twice fast_ticks and at most 16".to_owned(),
            );
        }
        if !(1..=MAX_OBSTACLE_TICKS).contains(&self.obstacle_ticks) {
            return Err("obstacle_ticks must be between 1 and 16".to_owned());
        }

        let state_bound =
            usize::from(self.length + 2) * 2 * (usize::from(self.initial_time) + 1) * 2;
        if state_bound > crate::MAX_REACHABLE_STATES {
            return Err("configuration exceeds the 100000-state oracle bound".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            position: 0,
            phase: 0,
            remaining: self.initial_time,
            goal: false,
        }
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.state_is_bounded(state) {
            return state;
        }
        if state.goal || state.remaining == 0 || action > 3 {
            return state;
        }

        match (state.position, state.phase, action) {
            (position, 0, 0) if position < self.length => {
                self.spend_route_time(state, self.slow_ticks, position + 1, 0)
            }
            (position, 0, 1) if position < self.length => {
                self.spend_route_time(state, self.fast_ticks, position, 1)
            }
            (position, 1, 1) if position < self.length => {
                self.spend_route_time(state, self.fast_ticks, position + 1, 0)
            }
            (position, 0, 2) if position == self.length => {
                if state.remaining >= self.obstacle_ticks {
                    State {
                        position: self.length + 1,
                        phase: 0,
                        remaining: state.remaining - self.obstacle_ticks,
                        goal: true,
                    }
                } else {
                    State {
                        remaining: 0,
                        ..state
                    }
                }
            }
            _ => self.spend_idle_tick(state),
        }
    }

    pub fn goal(&self, state: State) -> bool {
        self.validate().is_ok()
            && self.state_is_bounded(state)
            && state.goal
            && state.position == self.length + 1
            && state.phase == 0
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }

    pub fn key(&self, state: State, broken: bool) -> crate::Key {
        crate::Key {
            stock: 0,
            place: u16::from(state.position),
            context: u16::from(state.phase),
            charge: if broken { 0 } else { state.remaining },
            health: 0,
            goal: state.goal,
            tier: 0,
        }
    }

    pub(crate) fn state_is_bounded(&self, state: State) -> bool {
        state.position <= self.length + 1
            && state.phase <= 1
            && (state.phase == 0 || state.position < self.length)
            && state.remaining <= self.initial_time
            && state.goal == (state.position == self.length + 1)
    }

    fn spend_route_time(&self, state: State, ticks: u8, position: u8, phase: u8) -> State {
        if state.remaining < ticks {
            State {
                remaining: 0,
                phase: 0,
                ..state
            }
        } else {
            State {
                position,
                phase,
                remaining: state.remaining - ticks,
                ..state
            }
        }
    }

    fn spend_idle_tick(&self, state: State) -> State {
        State {
            phase: 0,
            remaining: state.remaining - 1,
            ..state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, State};

    fn config() -> Config {
        Config {
            length: 2,
            initial_time: 12,
            fast_ticks: 1,
            slow_ticks: 3,
            obstacle_ticks: 2,
        }
    }

    fn fast_arrival(config: Config) -> State {
        let mut state = config.initial();
        for _ in 0..config.length {
            state = config.step(state, 1);
            state = config.step(state, 1);
        }
        state
    }

    fn slow_arrival(config: Config) -> State {
        let mut state = config.initial();
        for _ in 0..config.length {
            state = config.step(state, 0);
        }
        state
    }

    #[test]
    fn fast_and_slow_routes_join_at_shared_places_with_different_time() {
        let config = config();
        let fast = fast_arrival(config);
        let slow = slow_arrival(config);

        assert_eq!((fast.position, fast.phase), (config.length, 0));
        assert_eq!((slow.position, slow.phase), (config.length, 0));
        assert_eq!(
            fast.remaining,
            config.initial_time - 2 * config.length * config.fast_ticks
        );
        assert_eq!(
            slow.remaining,
            config.initial_time - config.length * config.slow_ticks
        );
        assert!(fast.remaining > slow.remaining);
    }

    #[test]
    fn environmental_ticks_are_separate_from_action_count() {
        let config = Config {
            initial_time: 24,
            fast_ticks: 2,
            slow_ticks: 5,
            ..config()
        };
        let mut fast = config.initial();
        let mut fast_actions = 0;
        for _ in 0..config.length {
            fast = config.step(fast, 1);
            fast_actions += 1;
            fast = config.step(fast, 1);
            fast_actions += 1;
        }
        let mut slow = config.initial();
        let mut slow_actions = 0;
        for _ in 0..config.length {
            slow = config.step(slow, 0);
            slow_actions += 1;
        }

        assert_eq!(fast_actions, 2 * usize::from(config.length));
        assert_eq!(slow_actions, usize::from(config.length));
        let fast_ticks_spent = usize::from(config.initial_time - fast.remaining);
        let slow_ticks_spent = usize::from(config.initial_time - slow.remaining);
        assert_eq!(
            fast_ticks_spent,
            fast_actions * usize::from(config.fast_ticks)
        );
        assert_eq!(
            slow_ticks_spent,
            slow_actions * usize::from(config.slow_ticks)
        );
        assert_ne!(fast_actions, fast_ticks_spent);
    }

    #[test]
    fn completion_at_exact_deadline_succeeds_but_shortfall_expires() {
        let config = config();
        let at_boundary = State {
            position: config.length,
            remaining: config.obstacle_ticks,
            ..config.initial()
        };
        let goal = config.step(at_boundary, 2);
        assert!(config.goal(goal));
        assert_eq!(goal.remaining, 0);

        let too_late = State {
            remaining: config.obstacle_ticks - 1,
            ..at_boundary
        };
        let expired = config.step(too_late, 2);
        assert!(!config.goal(expired));
        assert_eq!(expired.remaining, 0);
        assert_eq!(config.step(expired, 0), expired);
    }

    #[test]
    fn wrong_actions_spend_time_and_clear_fast_route_phase() {
        let config = config();
        let preparing = config.step(config.initial(), 1);
        assert_eq!(preparing.phase, 1);
        let interrupted = config.step(preparing, 0);
        assert_eq!(interrupted.phase, 0);
        assert_eq!(interrupted.position, preparing.position);
        assert_eq!(interrupted.remaining, preparing.remaining - 1);

        let waiting = config.step(config.initial(), 3);
        assert_eq!(waiting.position, 0);
        assert_eq!(waiting.remaining, config.initial_time - 1);
    }

    #[test]
    fn key_ablation_hides_only_time_and_keeps_phase_context() {
        let config = config();
        let fast = State {
            position: 1,
            phase: 0,
            remaining: 8,
            goal: false,
        };
        let slow = State {
            remaining: 4,
            ..fast
        };
        assert_ne!(config.key(fast, false), config.key(slow, false));
        assert_eq!(config.key(fast, true), config.key(slow, true));
        assert_eq!(
            config.key(fast, false).context,
            config.key(fast, true).context
        );
        assert_eq!(config.key(fast, true).charge, 0);
    }

    #[test]
    fn snapshot_round_trip_and_bounds_are_consistent() {
        let config = config();
        let state = config.step(config.initial(), 1);
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
        assert!(config.state_is_bounded(decoded));
        assert!(!config.state_is_bounded(State {
            phase: 1,
            position: config.length,
            ..decoded
        }));
    }

    #[test]
    fn reachability_distinguishes_fast_completion_from_insufficient_deadline() {
        let reachable = Config {
            length: 1,
            initial_time: 4,
            fast_ticks: 1,
            slow_ticks: 3,
            obstacle_ticks: 2,
        };
        assert!(reachable.reachable().unwrap());

        let unreachable = Config {
            initial_time: 3,
            ..reachable
        };
        assert!(!unreachable.reachable().unwrap());
    }

    #[test]
    fn strict_schema_requires_all_fields_and_rejects_wrong_types_and_extras() {
        let valid =
            r#"{"length":2,"initial_time":12,"fast_ticks":1,"slow_ticks":3,"obstacle_ticks":2}"#;
        assert!(serde_json::from_str::<Config>(valid).is_ok());
        assert!(
            serde_json::from_str::<Config>(
                r#"{"length":2,"initial_time":12,"fast_ticks":1,"slow_ticks":3}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Config>(
            r#"{"length":"2","initial_time":12,"fast_ticks":1,"slow_ticks":3,"obstacle_ticks":2}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Config>(
            r#"{"length":2,"initial_time":12,"fast_ticks":1,"slow_ticks":3,"obstacle_ticks":2,"unused":0}"#
        )
        .is_err());
    }

    #[test]
    fn every_configuration_field_is_bounded_and_semantic() {
        let base = config();
        assert!(Config { length: 0, ..base }.validate().is_err());
        assert!(Config { length: 9, ..base }.validate().is_err());
        assert!(
            Config {
                initial_time: 0,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                initial_time: 65,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                fast_ticks: 0,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                fast_ticks: 5,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                slow_ticks: 2,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                slow_ticks: 17,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                obstacle_ticks: 0,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                obstacle_ticks: 17,
                ..base
            }
            .validate()
            .is_err()
        );

        let short = Config { length: 1, ..base };
        assert_eq!(short.step(short.initial(), 0).position, short.length);
        assert!(base.step(base.initial(), 0).position < base.length);
        assert_eq!(base.initial().remaining, base.initial_time);
        let slower_fast = Config {
            fast_ticks: 2,
            slow_ticks: 5,
            ..base
        };
        assert_ne!(
            base.step(base.initial(), 1).remaining,
            slower_fast.step(slower_fast.initial(), 1).remaining
        );
        let slower_route = Config {
            slow_ticks: 4,
            ..base
        };
        assert_ne!(
            base.step(base.initial(), 0).remaining,
            slower_route.step(slower_route.initial(), 0).remaining
        );

        let at_obstacle = State {
            position: base.length,
            remaining: 10,
            ..base.initial()
        };
        assert_eq!(base.step(at_obstacle, 2).remaining, 8);
        let longer_obstacle = Config {
            obstacle_ticks: 4,
            ..base
        };
        assert_eq!(longer_obstacle.step(at_obstacle, 2).remaining, 6);
    }
}
