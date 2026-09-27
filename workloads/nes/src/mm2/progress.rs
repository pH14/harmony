// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::target::{Mm2Observations, Mm2Scene};

pub const ROBOT_MASTERS: [&str; 8] = [
    "heat", "air", "wood", "bubble", "quick", "flash", "metal", "crash",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    pub execution: u64,
    pub route_action_end_frame: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NamedProgress {
    pub first_seen: BTreeMap<String, Option<FirstSeen>>,
}

impl Default for NamedProgress {
    fn default() -> Self {
        Self {
            first_seen: required_milestones()
                .into_iter()
                .map(|name| (name, None))
                .collect(),
        }
    }
}

pub fn required_milestones() -> Vec<String> {
    ROBOT_MASTERS
        .iter()
        .map(|name| format!("{name}_defeated"))
        .chain((1..=6).flat_map(|stage| {
            [
                format!("wily{stage}_entered"),
                format!("wily{stage}_boss_defeated"),
            ]
        }))
        .chain(["ending".to_owned()])
        .collect()
}

impl NamedProgress {
    pub fn reached(observation: &Mm2Observations) -> Vec<String> {
        let state = observation.decoded;
        if observation.dead && state.scene != Mm2Scene::Ending {
            return Vec::new();
        }
        let mut reached = Vec::new();
        for (bit, name) in ROBOT_MASTERS.iter().enumerate() {
            if state.weapons_obtained & (1 << bit) != 0 {
                reached.push(format!("{name}_defeated"));
            }
        }
        for stage in 1..=6 {
            if state.castle_clears >= stage {
                reached.push(format!("wily{stage}_entered"));
                reached.push(format!("wily{stage}_boss_defeated"));
            }
        }
        if (8..=13).contains(&state.stage) && state.weapons_obtained == 255 && state.health > 0 {
            let stage = state.stage - 7;
            reached.push(format!("wily{stage}_entered"));
            reached.push(format!("wily{stage}_room_{}", state.room));
        }
        if state.stage == 12 {
            for (bit, name) in ROBOT_MASTERS.iter().enumerate() {
                if state.refighting_mask & (1 << bit) != 0 {
                    reached.push(format!("wily5_{name}_refight_defeated"));
                }
            }
            if state.wily_machine_shell_broken {
                reached.push("wily5_machine_shell_broken".to_owned());
            }
        }
        if state.scene == Mm2Scene::Ending {
            reached.push("ending".to_owned());
        }
        reached
    }

    pub fn observe(
        &mut self,
        observation: &Mm2Observations,
        execution: u64,
        route_action_end_frame: u64,
    ) -> Vec<String> {
        let mut discoveries = Vec::new();
        for name in Self::reached(observation) {
            let entry = self.first_seen.entry(name.clone()).or_default();
            if entry.is_none() {
                *entry = Some(FirstSeen {
                    execution,
                    route_action_end_frame,
                });
                discoveries.push(name);
            }
        }
        discoveries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::target::Mm2MechanicalState;

    #[test]
    fn ending_after_six_castle_clears_reports_every_required_milestone() {
        let state = Mm2MechanicalState {
            weapons_obtained: 255,
            castle_clears: 6,
            scene: Mm2Scene::Ending,
            ..Default::default()
        };
        let observation = Mm2Observations {
            decoded: state,
            frame_count: 0,
            changed_indices: Vec::new(),
            dead: false,
            fall_run: 0,
            log_line: String::new(),
        };
        let mut progress = NamedProgress::default();
        assert_eq!(progress.observe(&observation, 42, 100).len(), 21);
        assert!(
            progress
                .first_seen
                .values()
                .all(|stamp| stamp.is_some_and(|stamp| stamp.execution == 42))
        );
        assert!(progress.observe(&observation, 43, 101).is_empty());
    }

    #[test]
    fn a_dead_observation_cannot_discover_a_milestone() {
        let observation = Mm2Observations {
            decoded: Mm2MechanicalState {
                weapons_obtained: 255,
                stage: 12,
                castle_clears: 4,
                ..Default::default()
            },
            frame_count: 0,
            changed_indices: Vec::new(),
            dead: true,
            fall_run: 0,
            log_line: String::new(),
        };
        assert!(NamedProgress::reached(&observation).is_empty());
    }
}
