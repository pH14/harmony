// SPDX-License-Identifier: AGPL-3.0-or-later

//! Named, reporting-only discoveries. No ordering, route, or search reward.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::target::MetroidObservations;

/// Raw boss identity, kept separate from the archive's legacy progress count.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BossDefeats {
    /// Kraid's defeat flag, $687B bit 0.
    pub kraid: bool,
    /// Ridley's defeat flag, $687C bit 1 (not bit 0).
    pub ridley: bool,
}

/// Reporting transitions observed anywhere inside one controller action.
/// Latching preserves brief states without emitting extra search observations.
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

/// First observation in admission order. Frame is a route coordinate, not work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    /// Admitted execution, starting at one; zero denotes the supplied origin.
    pub execution: u64,
    /// End of the controller action in frames since gameplay genesis. Cartridge
    /// RAM is sampled after an action; this is a reported route coordinate,
    /// not an exact within-action pickup timestamp.
    pub route_action_end_frame: u64,
}

// Metroid_Defines.asm, SamusGear and InArea, nmikstas/metroid-disassembly
// 4270d57f9468daebdeea485686e31e26218a780c. Array order is presentation only.
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

/// Name an observed area without ranking it. Unknown bytes remain unknown.
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

/// Bounded union over live observations, separate from archive policy evidence.
#[derive(Clone, Debug, Serialize)]
pub struct NamedProgress {
    /// Schema and decoder meaning; independent of search policy versions.
    pub format: &'static str,
    /// All named milestones are present; null means not observed in this scope.
    pub first_seen: BTreeMap<&'static str, Option<FirstSeen>>,
    /// Capacity, not missile pickups: defeated bosses also grant capacity.
    pub max_missile_capacity: u8,
    /// Energy tank count reported by the game, separate from missile capacity.
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
    /// Record discoveries and return only newly observed names for witness
    /// export. These names are never passed to selection or archive admission.
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
        // Metroid_Defines.asm $98 and Bank03.asm LA003-LA018: the fatal
        // 32nd hit enters state 3. State 8 initializes a living Mother Brain.
        // States 6/7 are the armed/exploded time bomb. No map coordinate or
        // action advice is involved; other areas' reused RAM is ignored.
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
