// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::target::{
    MM2_FIRST_WILY_STAGE, MM2_STAGE_COUNT, Mm2Observations, Mm2Stage, ROBOT_MASTER_ORDER,
};

pub const NAMED_PROGRESS_FORMAT: &str = "mm2-named-progress-v1";
const WILY5: u8 = 12;
const WILY_MACHINE: u8 = 12;
const ROOM_MARK: &str = "_room_";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    pub execution: u64,
    pub route_action_end_frame: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct NamedProgress {
    pub format: &'static str,
    pub first_seen: BTreeMap<String, Option<FirstSeen>>,
}

fn stage_name(stage: u8) -> Option<&'static str> {
    Mm2Stage::from_number(stage).ok().map(Mm2Stage::name)
}

#[must_use]
pub fn route_milestones() -> Vec<String> {
    let mut names = Vec::new();
    for stage in ROBOT_MASTER_ORDER {
        for event in ["entered", "boss", "defeated"] {
            names.push(format!("{}_{event}", stage.name()));
        }
    }
    for stage in MM2_FIRST_WILY_STAGE..MM2_FIRST_WILY_STAGE + 6 {
        let name = stage_name(stage).unwrap_or_default();
        names.push(format!("{name}_entered"));
        if stage == WILY5 {
            for refight in ROBOT_MASTER_ORDER {
                names.push(format!("{name}_{}_refight", refight.name()));
            }
            names.push(format!("{name}_machine"));
            names.push(format!("{name}_machine_shell"));
        } else {
            names.push(format!("{name}_boss"));
        }
        names.push(format!("{name}_defeated"));
    }
    names.push("ending".to_owned());
    names
}

#[must_use]
pub fn is_route_milestone(name: &str) -> bool {
    !name.contains(ROOM_MARK)
}

impl Default for NamedProgress {
    fn default() -> Self {
        Self {
            format: NAMED_PROGRESS_FORMAT,
            first_seen: route_milestones()
                .into_iter()
                .map(|name| (name, None))
                .collect(),
        }
    }
}

impl NamedProgress {
    pub fn observe(
        &mut self,
        observation: &Mm2Observations,
        execution: u64,
        route_action_end_frame: u64,
    ) -> Vec<String> {
        let stamp = FirstSeen {
            execution,
            route_action_end_frame,
        };
        let mut discoveries = Vec::new();
        for name in Self::reached(observation) {
            let first = self.first_seen.entry(name.clone()).or_default();
            if first.is_none() {
                *first = Some(stamp);
                discoveries.push(name);
            }
        }
        discoveries
    }

    #[must_use]
    pub fn reached(observation: &Mm2Observations) -> Vec<String> {
        let state = observation.decoded;
        let mut names = Vec::new();
        if observation.ending {
            names.push("wily6_defeated".to_owned());
            names.push("ending".to_owned());
            return names;
        }
        if observation.dead {
            return names;
        }
        for stage in 0..MM2_STAGE_COUNT {
            if state.weapons_obtained & (1 << stage) != 0 {
                names.push(format!("{}_defeated", stage_name(stage).unwrap_or_default()));
            }
        }
        if state.weapons_obtained == u8::MAX {
            for cleared in MM2_FIRST_WILY_STAGE..state.stage.min(MM2_FIRST_WILY_STAGE + 6) {
                names.push(format!("{}_defeated", stage_name(cleared).unwrap_or_default()));
            }
        }
        let Some(here) = stage_name(state.stage) else {
            return names;
        };
        if !state.playable() {
            return names;
        }
        names.push(format!("{here}{ROOM_MARK}{}", state.room));
        if observation.arrived {
            names.push(format!("{here}_entered"));
        }
        let fresh_boss = state.boss_damage() == 0
            && state.boss_health > 0
            && state.boss_fight_underway()
            && state.boss_phase >= 2;
        if state.stage == WILY5 {
            for (bit, refight) in (0..MM2_STAGE_COUNT).filter_map(|bit| Some((bit, stage_name(bit)?))) {
                if state.refights & (1 << bit) != 0 {
                    names.push(format!("{here}_{refight}_refight"));
                }
            }
            if state.current_boss == WILY_MACHINE && fresh_boss && !state.machine_shell_broken() {
                names.push(format!("{here}_machine"));
            }
            if state.machine_shell_broken() {
                names.push(format!("{here}_machine_shell"));
            }
        } else if fresh_boss {
            names.push(format!("{here}_boss"));
        }
        names
    }

    pub fn restore(&mut self, entries: Vec<(String, Option<FirstSeen>)>) -> Result<(), String> {
        for (name, seen) in entries {
            if !is_route_milestone(&name) || self.first_seen.contains_key(&name) {
                self.first_seen.insert(name, seen);
            } else {
                return Err(format!("search checkpoint names an unknown milestone {name}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::target::Mm2MechanicalState;

    fn playing(stage: u8, weapons: u8) -> Mm2MechanicalState {
        Mm2MechanicalState {
            stage,
            room: 3,
            health: 28,
            player_state: 3,
            bank: 0x0e,
            weapons_obtained: weapons,
            ..Mm2MechanicalState::default()
        }
    }

    fn at(state: Mm2MechanicalState) -> Mm2Observations {
        Mm2Observations {
            decoded: state,
            ..Mm2Observations::default()
        }
    }

    #[test]
    fn the_route_lists_every_stage_boss_and_the_ending_without_dashes() {
        let names = route_milestones();
        assert_eq!(names.first().map(String::as_str), Some("crash_entered"));
        assert!(names.contains(&"wily5_quick_refight".to_owned()));
        assert!(names.contains(&"wily4_boss".to_owned()));
        assert!(!names.contains(&"wily5_boss".to_owned()));
        assert_eq!(names.last().map(String::as_str), Some("ending"));
        assert!(names.iter().all(|name| !name.contains('-')));
    }

    #[test]
    fn an_arrival_enters_its_stage_and_rooms_report_only_in_play() {
        let mut arrival = at(playing(5, 0x80));
        arrival.arrived = true;
        let names = NamedProgress::reached(&arrival);
        assert!(names.contains(&"flash_entered".to_owned()));
        assert!(names.contains(&"crash_defeated".to_owned()));
        assert!(names.contains(&"flash_room_3".to_owned()));

        let walking = at(playing(5, 0x80));
        assert!(!NamedProgress::reached(&walking).contains(&"flash_entered".to_owned()));

        let menu = at(Mm2MechanicalState {
            bank: 0x0d,
            ..playing(5, 0x80)
        });
        assert_eq!(NamedProgress::reached(&menu), vec!["crash_defeated"]);

        let mut dead = arrival;
        dead.dead = true;
        assert!(NamedProgress::reached(&dead).is_empty());
    }

    #[test]
    fn a_boss_counts_once_its_meter_is_full_and_the_fight_has_started() {
        let mut fight = playing(7, 0);
        fight.boss_phase = 2;
        fight.boss_health = 28;
        assert!(NamedProgress::reached(&at(fight)).contains(&"crash_boss".to_owned()));
        fight.boss_health = 20;
        assert!(!NamedProgress::reached(&at(fight)).contains(&"crash_boss".to_owned()));
        fight.boss_phase = 1;
        fight.boss_health = 28;
        assert!(!NamedProgress::reached(&at(fight)).contains(&"crash_boss".to_owned()));
    }

    #[test]
    fn castle_clears_follow_the_stage_and_wily5_reports_refights_and_the_machine() {
        let mut hub = playing(12, u8::MAX);
        hub.refights = 0x81;
        let names = NamedProgress::reached(&at(hub));
        for name in ["wily4_defeated", "wily1_defeated", "wily5_heat_refight", "wily5_crash_refight"] {
            assert!(names.contains(&name.to_owned()), "{name}");
        }
        assert!(!names.contains(&"wily5_defeated".to_owned()));
        hub.current_boss = 12;
        hub.boss_phase = 4;
        assert!(NamedProgress::reached(&at(hub)).contains(&"wily5_machine_shell".to_owned()));

        let mut ending = at(playing(5, u8::MAX));
        ending.ending = true;
        ending.dead = true;
        assert_eq!(NamedProgress::reached(&ending), vec!["wily6_defeated", "ending"]);
    }

    #[test]
    fn a_restored_checkpoint_keeps_rooms_and_refuses_unknown_route_names() {
        let mut progress = NamedProgress::default();
        let stamp = Some(FirstSeen {
            execution: 4,
            route_action_end_frame: 9,
        });
        progress
            .restore(vec![
                ("crash_entered".to_owned(), stamp),
                ("crash_room_7".to_owned(), stamp),
            ])
            .expect("restore");
        assert_eq!(progress.first_seen["crash_room_7"], stamp);
        assert!(progress.restore(vec![("kraid_room".to_owned(), stamp)]).is_err());
    }
}
