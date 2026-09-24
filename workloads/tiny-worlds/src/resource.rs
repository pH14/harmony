// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

const MAX_CONFIG_VALUE: u8 = 31;
const MAX_CORRIDOR_LEN: u8 = 12;
const MAX_BARRIER_CHARGE: u8 = 15;
const MAX_ROUTE_COST: u8 = 15;
const MAX_HEALTH_COST: u8 = 15;
const MAX_STATES: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub initial_charge: u8,
    pub initial_health: u8,
    pub barrier_charge: u8,
    pub route_cost: u8,
    pub health_cost: u8,
    pub refill_amount: u8,
    pub max_charge: u8,
    pub corridor_len: u8,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub place: u8,
    pub charge: u8,
    pub health: u8,
    pub goal: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.initial_charge > MAX_CONFIG_VALUE {
            return Err("initial_charge must be at most 31".to_owned());
        }
        if !(1..=MAX_CONFIG_VALUE).contains(&self.initial_health) {
            return Err("initial_health must be between 1 and 31".to_owned());
        }
        if !(1..=MAX_BARRIER_CHARGE).contains(&self.barrier_charge) {
            return Err("barrier_charge must be between 1 and 15".to_owned());
        }
        if self.route_cost > MAX_ROUTE_COST {
            return Err("route_cost must be at most 15".to_owned());
        }
        if self.health_cost > MAX_HEALTH_COST {
            return Err("health_cost must be at most 15".to_owned());
        }
        if !(1..=MAX_CONFIG_VALUE).contains(&self.refill_amount) {
            return Err("refill_amount must be between 1 and 31".to_owned());
        }
        if !(1..=MAX_CONFIG_VALUE).contains(&self.max_charge) {
            return Err("max_charge must be between 1 and 31".to_owned());
        }
        if self.initial_charge > self.max_charge {
            return Err("initial_charge must not exceed max_charge".to_owned());
        }
        if !(1..=MAX_CORRIDOR_LEN).contains(&self.corridor_len) {
            return Err("corridor_len must be between 1 and 12".to_owned());
        }

        let state_bound = usize::from(self.corridor_len + 2)
            * (usize::from(self.max_charge) + 1)
            * (usize::from(self.initial_health) + 1)
            * 2;
        if state_bound > MAX_STATES {
            return Err("configuration exceeds the 100000-state oracle bound".to_owned());
        }
        Ok(())
    }

    pub fn initial(&self) -> State {
        State {
            place: 0,
            charge: self.initial_charge,
            health: self.initial_health,
            goal: false,
        }
    }

    pub fn step(&self, state: State, action: u8) -> State {
        if self.validate().is_err() || !self.state_is_bounded(state) {
            return state;
        }
        self.transition(state, action)
    }

    pub fn goal(&self, state: State) -> bool {
        state.goal && state.place == self.corridor_len + 1 && state.health > 0
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
                let next = self.transition(state, action);
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

    fn state_is_bounded(&self, state: State) -> bool {
        state.place <= self.corridor_len.saturating_add(1)
            && state.charge <= self.max_charge
            && state.health <= self.initial_health
    }

    fn transition(&self, state: State, action: u8) -> State {
        if state.goal || state.health == 0 {
            return state;
        }

        match action {
            0 => {
                if state.place > self.corridor_len {
                    return state;
                }
                let at_barrier = state.place == self.corridor_len;
                let charge_cost = if at_barrier {
                    u16::from(self.barrier_charge)
                } else {
                    u16::from(self.route_cost)
                };
                if u16::from(state.charge) < charge_cost
                    || (at_barrier && state.health <= self.health_cost)
                {
                    return state;
                }
                let place = state.place + 1;
                State {
                    place,
                    charge: (u16::from(state.charge) - charge_cost) as u8,
                    health: if at_barrier {
                        state.health - self.health_cost
                    } else {
                        state.health
                    },
                    goal: place == self.corridor_len + 1,
                }
            }
            1 => {
                if state.place != 0 || state.charge >= self.max_charge {
                    return state;
                }
                let charge = (u16::from(state.charge) + u16::from(self.refill_amount))
                    .min(u16::from(self.max_charge)) as u8;
                State { charge, ..state }
            }
            2 => {
                if state.place == 0 {
                    state
                } else {
                    State {
                        place: 0,
                        goal: false,
                        ..state
                    }
                }
            }
            3 => state,
            _ => state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, State};

    fn config() -> Config {
        Config {
            initial_charge: 0,
            initial_health: 4,
            barrier_charge: 5,
            route_cost: 1,
            health_cost: 1,
            refill_amount: 2,
            max_charge: 8,
            corridor_len: 3,
        }
    }

    #[test]
    fn refill_is_limited_to_the_station_and_clamped_to_capacity() {
        let config = config();
        let station = config.step(config.initial(), 1);
        assert_eq!(station.charge, 2);
        assert_eq!(config.step(station, 1).charge, 4);

        let full = State {
            charge: 7,
            ..station
        };
        assert_eq!(config.step(full, 1).charge, 8);
        assert_eq!(
            config
                .step(
                    State {
                        place: 1,
                        ..station
                    },
                    1
                )
                .charge,
            2
        );
    }

    #[test]
    fn route_then_barrier_consume_charge_and_gate_consumes_health() {
        let config = config();
        let stocked = State {
            charge: 8,
            ..config.initial()
        };
        let after_route_one = config.step(stocked, 0);
        assert_eq!(
            after_route_one,
            State {
                place: 1,
                charge: 7,
                health: 4,
                goal: false,
            }
        );
        let after_route_three = config.step(config.step(after_route_one, 0), 0);
        assert_eq!(after_route_three.place, 3);
        assert_eq!(after_route_three.charge, 5);
        assert_eq!(after_route_three.health, 4);

        let at_goal = config.step(after_route_three, 0);
        assert_eq!(
            at_goal,
            State {
                place: 4,
                charge: 0,
                health: 3,
                goal: true,
            }
        );
        assert!(config.goal(at_goal));
    }

    #[test]
    fn return_preserves_stock_and_health_while_wait_is_a_noop() {
        let config = config();
        let away = State {
            place: 2,
            charge: 1,
            health: 2,
            goal: false,
        };
        assert_eq!(config.step(away, 2), State { place: 0, ..away });
        assert_eq!(config.step(away, 3), away);
        assert_eq!(config.step(away, 9), away);
    }

    #[test]
    fn state_snapshot_round_trips() {
        let state = State {
            place: 2,
            charge: 1,
            health: 2,
            goal: false,
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn config_schema_requires_typed_fields_and_rejects_extras() {
        let valid_config = r#"{
            "initial_charge":0,
            "initial_health":4,
            "barrier_charge":5,
            "route_cost":1,
            "health_cost":1,
            "refill_amount":2,
            "max_charge":8,
            "corridor_len":3
        }"#;
        let parsed: Config = serde_json::from_str(valid_config).unwrap();
        assert_eq!(parsed.barrier_charge, 5);

        let missing_required = r#"{
            "initial_charge":0,
            "initial_health":4,
            "route_cost":1,
            "health_cost":1,
            "refill_amount":2,
            "max_charge":8,
            "corridor_len":3
        }"#;
        assert!(serde_json::from_str::<Config>(missing_required).is_err());
        assert!(serde_json::from_str::<Config>(
            r#"{"initial_charge":"0","initial_health":4,"barrier_charge":5,"route_cost":1,"health_cost":1,"refill_amount":2,"max_charge":8,"corridor_len":3}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Config>(
            r#"{"initial_charge":0,"initial_health":4,"barrier_charge":5,"route_cost":1,"health_cost":1,"refill_amount":2,"max_charge":8,"corridor_len":3,"unused":0}"#
        )
        .is_err());
    }

    #[test]
    fn exhaustive_oracle_finds_multi_refill_route_and_rejects_short_stock() {
        let solvable = config();
        let mut witness = solvable.initial();
        for _ in 0..4 {
            witness = solvable.step(witness, 1);
        }
        for _ in 0..=solvable.corridor_len {
            witness = solvable.step(witness, 0);
        }
        assert!(solvable.goal(witness));
        assert!(solvable.reachable().unwrap());

        let insufficient_capacity = Config {
            max_charge: 6,
            ..solvable
        };
        assert!(!insufficient_capacity.reachable().unwrap());

        let insufficient_health = Config {
            max_charge: 8,
            health_cost: 4,
            ..solvable
        };
        assert!(!insufficient_health.reachable().unwrap());
    }
}
