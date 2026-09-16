// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::target::BlueObservation;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    pub execution: u64,
    pub action_end_frame: u64,
}

pub const MILESTONE_NAMES: [&str; 8] = [
    "got_starter",
    "got_parcel",
    "delivered_parcel",
    "got_pokedex",
    "entered_viridian_forest",
    "entered_pewter",
    "entered_pewter_gym",
    "boulder_badge",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NamedProgress {
    pub format: &'static str,
    pub first_seen: BTreeMap<&'static str, Option<FirstSeen>>,
    pub max_party_levels: u32,
    pub maps_seen: u32,
}

impl Default for NamedProgress {
    fn default() -> Self {
        Self {
            format: "blue-named-progress-v1",
            first_seen: MILESTONE_NAMES.iter().map(|name| (*name, None)).collect(),
            max_party_levels: 0,
            maps_seen: 0,
        }
    }
}

impl NamedProgress {
    pub fn observe(
        &mut self,
        observation: &BlueObservation,
        execution: u64,
        action_end_frame: u64,
    ) -> Vec<&'static str> {
        if observation.dead {
            return Vec::new();
        }
        let state = observation.state;
        let stamp = FirstSeen {
            execution,
            action_end_frame,
        };
        let mut discoveries = Vec::new();
        let flags = state.milestone_flags();
        for (bit, name) in MILESTONE_NAMES.iter().enumerate() {
            if flags & (1 << bit) == 0 {
                continue;
            }
            if let Some(first) = self.first_seen.get_mut(name)
                && first.is_none()
            {
                *first = Some(stamp);
                discoveries.push(*name);
            }
        }
        self.max_party_levels = self.max_party_levels.max(state.party_levels());
        discoveries
    }

    #[must_use]
    pub fn reached(&self) -> u32 {
        u32::try_from(
            self.first_seen
                .values()
                .filter(|first| first.is_some())
                .count(),
        )
        .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{BlueState, MILESTONE_BITS, decode_state};

    fn observation(flags: u8) -> BlueObservation {
        let state = BlueState {
            route_flags: flags & 0x7f,
            badges: u8::from(flags & 0x80 != 0),
            ..BlueState::default()
        };
        BlueObservation {
            frame_count: 10,
            state,
            alphabet_size: 1,
            dead: false,
        }
    }

    #[test]
    fn every_milestone_bit_has_its_own_name_and_the_first_sighting_stands() {
        assert_eq!(MILESTONE_NAMES.len(), MILESTONE_BITS);
        let mut progress = NamedProgress::default();
        assert_eq!(progress.observe(&observation(0x01), 4, 40), ["got_starter"]);
        assert!(progress.observe(&observation(0x01), 9, 90).is_empty());
        assert_eq!(progress.first_seen["got_starter"].unwrap().execution, 4);
        assert_eq!(progress.reached(), 1);
        assert_eq!(
            progress.observe(&observation(0x83), 7, 70),
            ["got_parcel", "boulder_badge"]
        );
        assert_eq!(progress.reached(), 3);
    }

    #[test]
    fn a_whiteout_observation_adds_no_discovery() {
        let mut progress = NamedProgress::default();
        let mut dead = observation(0xff);
        dead.dead = true;
        assert!(progress.observe(&dead, 1, 10).is_empty());
        assert_eq!(progress.reached(), 0);
    }

    #[test]
    fn the_named_order_matches_the_bits_the_target_decodes() {
        let wram = vec![0_u8; crate::target::WRAM_SIZE];
        assert_eq!(decode_state(&wram, 0).milestone_flags(), 0);
    }
}
