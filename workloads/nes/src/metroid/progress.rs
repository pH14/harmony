// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::target::MetroidObservations;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BossDefeats {
    pub kraid: bool,
    pub ridley: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TourianEvents {
    pub mother_brain_defeated: bool,
    pub escape_started: bool,
}

impl TourianEvents {
    pub fn observe(&mut self, state: super::target::MetroidMechanicalState, status: u8) {
        if state.area == 0x13 && state.in_play() && !state.is_dead() {
            self.mother_brain_defeated |= matches!(status, 3..=7 | 9 | 10);
            self.escape_started |= matches!(status, 6 | 7);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    pub execution: u64,
    pub route_action_end_frame: u64,
}

const GEAR: [(u8, &str); 8] = [
    (0x10, "morph_ball"),
    (0x01, "bombs"),
    (0x04, "long_beam"),
    (0x02, "high_jump"),
    (0x08, "screw_attack"),
    (0x20, "varia_suit"),
    (0x40, "wave_beam"),
    (0x80, "ice_beam"),
];

#[must_use]
pub fn area_name(area: u8) -> Option<&'static str> {
    match area {
        0 | 0x10 => Some("brinstar"),
        0x11 => Some("norfair"),
        0x12 => Some("kraid_area"),
        0x13 => Some("tourian"),
        0x14 => Some("ridley_area"),
        _ => None,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct NamedProgress {
    pub format: &'static str,
    pub first_seen: BTreeMap<&'static str, Option<FirstSeen>>,
    pub max_missile_capacity: u8,
    pub max_energy_tanks: u8,
}

impl Default for NamedProgress {
    fn default() -> Self {
        Self {
            format: "metroid-named-progress-v2",
            first_seen: GEAR
                .iter()
                .map(|(_, name)| *name)
                .chain([
                    "brinstar",
                    "norfair",
                    "kraid_area",
                    "ridley_area",
                    "tourian",
                    "mother_brain_defeated",
                    "escape_started",
                    "kraid_defeated",
                    "ridley_defeated",
                    "ending",
                    "missile_capacity",
                    "energy_tank",
                ])
                .map(|name| (name, None))
                .collect(),
            max_missile_capacity: 0,
            max_energy_tanks: 0,
        }
    }
}

impl NamedProgress {
    pub fn observe(
        &mut self,
        observation: &MetroidObservations,
        execution: u64,
        route_action_end_frame: u64,
    ) -> Vec<&'static str> {
        let state = observation.decoded;
        if observation.dead || (!state.in_play() && !state.ending) {
            return Vec::new();
        }
        let stamp = FirstSeen {
            execution,
            route_action_end_frame,
        };
        let mut discoveries = Vec::new();
        let mut note = |name| {
            let first = self
                .first_seen
                .get_mut(name)
                .expect("fixed milestone vocabulary");
            if first.is_none() {
                *first = Some(stamp);
                discoveries.push(name);
            }
        };
        for (bit, name) in GEAR {
            if state.equipment & bit != 0 {
                note(name);
            }
        }
        if let Some(area) = area_name(state.area) {
            note(area);
        }
        if observation.boss_defeats.kraid {
            note("kraid_defeated");
        }
        if observation.boss_defeats.ridley {
            note("ridley_defeated");
        }
        let mut tourian = observation.tourian_events;
        tourian.observe(state, observation.mother_brain_status);
        if tourian.mother_brain_defeated {
            note("mother_brain_defeated");
        }
        if tourian.escape_started {
            note("escape_started");
        }
        if state.ending {
            note("ending");
        }
        if state.missile_capacity > 0 {
            note("missile_capacity");
        }
        if state.energy_tanks > 0 {
            note("energy_tank");
        }
        self.max_missile_capacity = self.max_missile_capacity.max(state.missile_capacity);
        self.max_energy_tanks = self.max_energy_tanks.max(state.energy_tanks);
        discoveries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metroid::target::decode_state;

    fn observation(gear: u8, area: u8, kraid: u8, ridley: u8) -> MetroidObservations {
        let mut wram = [0; 2048];
        let mut cartridge = [0; 8192];
        wram[0x1e] = 3;
        wram[0x107] = 3;
        wram[0x74] = area;
        cartridge[0x878] = gear;
        cartridge[0x87b] = kraid;
        cartridge[0x87c] = ridley;
        MetroidObservations {
            frame_count: 10,
            decoded: decode_state(&wram, &cartridge).unwrap(),
            boss_defeats: BossDefeats {
                kraid: kraid & 1 != 0,
                ridley: ridley & 2 != 0,
            },
            mother_brain_status: 0,
            tourian_events: TourianEvents::default(),
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        }
    }

    #[test]
    fn identities_do_not_collapse_equal_item_counts_or_rank_areas() {
        let mut progress = NamedProgress::default();
        progress.observe(&observation(0x10, 0x12, 0, 0), 7, 120);
        progress.observe(&observation(0x04, 0x14, 0, 0), 9, 90);
        for name in ["morph_ball", "long_beam", "kraid_area", "ridley_area"] {
            assert!(progress.first_seen[name].is_some());
        }
        assert!(progress.first_seen["bombs"].is_none());
        assert!(progress.first_seen["tourian"].is_none());
        assert!(progress.first_seen["kraid_defeated"].is_none());
        assert_eq!(progress.first_seen["morph_ball"].unwrap().execution, 7);
        assert_eq!(area_name(0x18), None);
        assert_eq!(area_name(0), area_name(0x10));
    }

    #[test]
    fn boss_bits_are_distinct_and_statue_bits_are_not_defeats() {
        for (kraid, ridley, count) in [
            (0, 0, 0),
            (1, 0, 1),
            (0, 2, 1),
            (1, 2, 2),
            (0x80, 0x80, 0),
            (0, 1, 0),
        ] {
            let observation = observation(0, 0x10, kraid, ridley);
            assert_eq!(observation.decoded.bosses, count);
            let mut progress = NamedProgress::default();
            progress.observe(&observation, 1, 10);
            assert_eq!(
                progress.first_seen["kraid_defeated"].is_some(),
                kraid & 1 != 0
            );
            assert_eq!(
                progress.first_seen["ridley_defeated"].is_some(),
                ridley & 2 != 0
            );
        }
    }

    #[test]
    fn every_gear_bit_is_named_independently_and_first_seen_is_stable() {
        for (bit, name) in GEAR {
            let mut progress = NamedProgress::default();
            let observation = observation(bit, 0x10, 0, 0);
            progress.observe(&observation, 12, 100);
            progress.observe(&observation, 20, 80);
            for (other, other_name) in GEAR {
                assert_eq!(progress.first_seen[other_name].is_some(), bit == other);
            }
            assert_eq!(progress.first_seen[name].unwrap().execution, 12);
        }
    }

    #[test]
    fn mother_brain_defeat_is_distinct_from_initialization_and_escape() {
        for area in [0x10, 0x13, 0x14] {
            for status in 0..=11 {
                let mut observation = observation(0, area, 0, 0);
                observation.mother_brain_status = status;
                let mut progress = NamedProgress::default();
                progress.observe(&observation, 3, 40);
                assert_eq!(
                    progress.first_seen["mother_brain_defeated"].is_some(),
                    area == 0x13 && matches!(status, 3..=7 | 9 | 10)
                );
                assert_eq!(
                    progress.first_seen["escape_started"].is_some(),
                    area == 0x13 && matches!(status, 6 | 7)
                );
            }
        }
    }

    #[test]
    fn capacity_and_tanks_are_separate_and_deaths_do_not_add_discoveries() {
        let mut progress = NamedProgress::default();
        let mut observation = observation(0x01, 0x13, 0, 0);
        observation.dead = true;
        assert!(progress.observe(&observation, 1, 10).is_empty());
        observation.dead = false;
        observation.decoded.missile_capacity = 85;
        observation.decoded.energy_tanks = 2;
        progress.observe(&observation, 2, 20);
        assert_eq!(progress.max_missile_capacity, 85);
        assert_eq!(progress.max_energy_tanks, 2);
        assert!(progress.first_seen["ridley_defeated"].is_none());
    }
}
