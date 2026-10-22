// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::target::{BOSSES, CastlevaniaObservations, FINAL_STAGE, FULL_BOSS_HEALTH};

pub const NAMED_PROGRESS_FORMAT: &str = "castlevania-named-progress-v1";
const SECTION_MARK: &str = "_section_";

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

fn boss_of(stage: u8) -> Option<&'static str> {
    BOSSES
        .iter()
        .find(|(boss_stage, _)| *boss_stage == stage)
        .map(|(_, name)| *name)
}

#[must_use]
pub fn route_milestones() -> Vec<String> {
    let mut names = Vec::new();
    for stage in 1..=FINAL_STAGE {
        names.push(format!("stage_{stage:02}_entered"));
        if let Some(boss) = boss_of(stage) {
            names.push(format!("{boss}_engaged"));
            names.push(format!("{boss}_defeated"));
        }
    }
    names.push("ending".to_owned());
    names
}

#[must_use]
pub fn is_route_milestone(name: &str) -> bool {
    !name.contains(SECTION_MARK)
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
        observation: &CastlevaniaObservations,
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
    pub fn reached(observation: &CastlevaniaObservations) -> Vec<String> {
        let state = observation.decoded;
        let mut names = Vec::new();
        if state.dead() {
            return names;
        }
        for stage in 1..=state.stage.min(FINAL_STAGE) {
            names.push(format!("stage_{stage:02}_entered"));
            if let Some(boss) = boss_of(stage)
                && stage < state.stage
            {
                names.push(format!("{boss}_engaged"));
                names.push(format!("{boss}_defeated"));
            }
        }
        if state.victory() {
            names.push("dracula_engaged".to_owned());
            names.push("dracula_defeated".to_owned());
            names.push("ending".to_owned());
            return names;
        }
        if let Some(boss) = boss_of(state.stage) {
            if state.boss_health < FULL_BOSS_HEALTH && state.playing() {
                names.push(format!("{boss}_engaged"));
            }
            if state.boss_down() && state.stage != FINAL_STAGE {
                names.push(format!("{boss}_defeated"));
            }
        }
        if state.playing() {
            names.push(format!(
                "stage_{:02}{SECTION_MARK}{}",
                state.stage, state.section
            ));
        }
        names
    }

    pub fn restore(&mut self, entries: Vec<(String, Option<FirstSeen>)>) -> Result<(), String> {
        for (name, seen) in entries {
            if !is_route_milestone(&name) || self.first_seen.contains_key(&name) {
                self.first_seen.insert(name, seen);
            } else {
                return Err(format!(
                    "search checkpoint names an unknown milestone {name}"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::castlevania::target::CastlevaniaMechanicalState;

    fn at(stage: u8) -> CastlevaniaObservations {
        CastlevaniaObservations {
            frame_count: 0,
            decoded: CastlevaniaMechanicalState {
                mode: 5,
                submode: 6,
                stage,
                health: 0x40,
                lives: 4,
                boss_health: 0x40,
                ..CastlevaniaMechanicalState::default()
            },
            terminal: false,
        }
    }

    #[test]
    fn the_route_lists_every_stage_and_boss_and_ends_with_the_ending() {
        let names = route_milestones();
        assert_eq!(names.first().map(String::as_str), Some("stage_01_entered"));
        assert!(names.contains(&"phantom_bat_defeated".to_owned()));
        assert!(names.contains(&"dracula_engaged".to_owned()));
        assert_eq!(names.last().map(String::as_str), Some("ending"));
        assert_eq!(names.len(), 18 + 12 + 1);
        assert!(names.iter().all(|name| !name.contains('-')));
    }

    #[test]
    fn a_later_stage_implies_earlier_stages_and_bosses() {
        let names = NamedProgress::reached(&at(7));
        for name in [
            "stage_01_entered",
            "stage_07_entered",
            "medusa_defeated",
            "stage_07_section_0",
        ] {
            assert!(names.contains(&name.to_owned()), "{name}");
        }
        assert!(!names.contains(&"stage_08_entered".to_owned()));
        assert!(NamedProgress::reached(&at(0)) == vec!["stage_00_section_0".to_owned()]);
    }

    #[test]
    fn a_boss_is_engaged_on_its_first_hit_and_defeated_at_zero() {
        let mut fight = at(3);
        assert!(!NamedProgress::reached(&fight).contains(&"phantom_bat_engaged".to_owned()));
        fight.decoded.boss_health = 0x30;
        assert!(NamedProgress::reached(&fight).contains(&"phantom_bat_engaged".to_owned()));
        fight.decoded.boss_health = 0;
        assert!(NamedProgress::reached(&fight).contains(&"phantom_bat_defeated".to_owned()));
        let mut dead = fight;
        dead.decoded.mode = 6;
        assert!(NamedProgress::reached(&dead).is_empty());
    }

    #[test]
    fn dracula_at_zero_is_engaged_and_leaving_play_is_the_ending() {
        let mut fight = at(18);
        fight.decoded.boss_health = 0;
        let names = NamedProgress::reached(&fight);
        assert!(names.contains(&"dracula_engaged".to_owned()));
        assert!(!names.contains(&"dracula_defeated".to_owned()));
        fight.decoded.mode = 0x08;
        assert!(NamedProgress::reached(&fight).contains(&"ending".to_owned()));
    }

    #[test]
    fn a_restored_checkpoint_keeps_sections_and_refuses_unknown_route_names() {
        let mut progress = NamedProgress::default();
        let stamp = Some(FirstSeen {
            execution: 4,
            route_action_end_frame: 9,
        });
        progress
            .restore(vec![
                ("stage_01_entered".to_owned(), stamp),
                ("stage_02_section_1".to_owned(), stamp),
            ])
            .expect("restore");
        assert_eq!(progress.first_seen["stage_02_section_1"], stamp);
        assert!(progress.restore(vec![("kraid".to_owned(), stamp)]).is_err());
    }
}
