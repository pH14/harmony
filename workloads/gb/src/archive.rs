// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use searcher::search::{
    archive::{
        Archive, ArchiveEntryReport, ArchiveKey, Input, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};

use crate::target::{
    ACTION_KINDS, BlueAction, BlueObservation, BlueSnapshot, BlueState, MAX_ACTION_FRAME_BUDGET,
    action_cost,
};

pub use searcher::search::archive::MAX_ARCHIVE_ENTRIES;

pub const MAX_BLUE_ACTIONS: usize = 4_096;
pub const KEY_POLICY_IDENTIFIER: &str =
    "blue_badges_route_events_map_cell4_preference_party_hp_then_levels_v2";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "uniform_macro_kind_and_slot_v1";

pub const CELL_STEPS: u8 = 2;
pub const ALPHABET_SLOTS: usize = 256;

pub type BlueInput = Input<BlueAction>;
pub type BlueArchive = Archive<BlueAction, BlueArchiveKey, BlueMilestones, BlueSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BlueArchiveGroup {
    badges: u8,
    route: u8,
    events: u16,
    map: u8,
    cell_x: u8,
    cell_y: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BlueArchiveKey {
    pub badges: u8,
    pub route: u8,
    pub events: u16,
    pub map: u8,
    pub cell_x: u8,
    pub cell_y: u8,
    pub party_hp: u32,
    pub party_levels: u32,
}

impl ArchiveKey for BlueArchiveKey {
    type Group = BlueArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn progress_cmp(left: Self::Group, right: Self::Group) -> Ordering {
        (
            left.badges.count_ones(),
            left.route.count_ones(),
            left.events,
        )
            .cmp(&(
                right.badges.count_ones(),
                right.route.count_ones(),
                right.events,
            ))
    }

    fn group(self, depth: usize) -> Self::Group {
        let place = BlueArchiveGroup {
            badges: self.badges,
            route: self.route,
            events: self.events,
            map: self.map,
            cell_x: self.cell_x,
            cell_y: self.cell_y,
        };
        match depth {
            0 => place,
            1 => BlueArchiveGroup {
                cell_x: 0,
                cell_y: 0,
                ..place
            },
            2 => BlueArchiveGroup {
                map: 0,
                cell_x: 0,
                cell_y: 0,
                ..place
            },
            3 => BlueArchiveGroup {
                badges: self.badges,
                route: self.route,
                ..BlueArchiveGroup::default()
            },
            _ => BlueArchiveGroup {
                badges: self.badges,
                ..BlueArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn preference_cmp(self, other: Self) -> Ordering {
        (self.party_hp, self.party_levels).cmp(&(other.party_hp, other.party_levels))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn archive_key(state: BlueState) -> BlueArchiveKey {
    BlueArchiveKey {
        badges: state.badges,
        route: state.route_flags,
        events: state.events_set,
        map: state.map,
        cell_x: state.x / CELL_STEPS,
        cell_y: state.y / CELL_STEPS,
        party_hp: state.party_hp(),
        party_levels: state.party_levels(),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueMilestones {
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BlueProgressWatermark {
    pub flags: u8,
    pub party_levels: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueMilestoneTimes {
    pub first_route_flag: Option<u64>,
    pub first_badge: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueMilestoneInputs {
    pub first_route_flag: Option<BlueInput>,
    pub first_badge: Option<BlueInput>,
}

pub type BlueArchiveProgressPoint = ProgressPoint<BlueMilestones, BlueProgressWatermark>;
pub type BlueArchiveEntryReport = ArchiveEntryReport<BlueAction, BlueArchiveKey, BlueMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: BlueMilestones,
    pub progress_watermark: BlueProgressWatermark,
    pub first_reached: BlueMilestoneTimes,
    pub first_inputs: BlueMilestoneInputs,
    pub champion_input: BlueInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<BlueArchiveEntryReport>,
    pub progress_curve: Vec<BlueArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub whiteouts: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: BlueState) -> BlueMilestones {
    BlueMilestones {
        flags: state.milestone_flags(),
    }
}

pub fn merge_milestones(into: &mut BlueMilestones, from: BlueMilestones) {
    into.flags |= from.flags;
}

#[must_use]
pub fn milestone_key(value: BlueMilestones) -> (u32, u8) {
    (value.flags.count_ones(), value.flags)
}

#[must_use]
pub fn progress_watermark(state: BlueState) -> BlueProgressWatermark {
    BlueProgressWatermark {
        flags: state.milestone_flags(),
        party_levels: state.party_levels(),
    }
}

pub fn merge_progress_watermark(
    watermark: &mut BlueProgressWatermark,
    observations: &[BlueObservation],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_watermark(observation.state));
    }
}

pub fn sample_action(rand: &mut RomuDuoJrRand) -> Result<BlueAction, Box<dyn Error>> {
    let kinds = NonZeroUsize::new(ACTION_KINDS.len()).ok_or("empty macro vocabulary")?;
    let slots = NonZeroUsize::new(ALPHABET_SLOTS).ok_or("empty alphabet slot range")?;
    Ok(BlueAction::new(
        ACTION_KINDS[rand.below(kinds)],
        u8::try_from(rand.below(slots))?,
    ))
}

#[must_use]
pub fn action_time(action: &BlueAction) -> u64 {
    action_cost(action)
}

#[must_use]
pub fn longest_action_frames() -> u64 {
    MAX_ACTION_FRAME_BUDGET
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(badges: u8, route: u8, map: u8, x: u8, y: u8, hp: u32, levels: u32) -> BlueArchiveKey {
        keyed(badges, route, 0, map, x, y, hp, levels)
    }

    #[allow(clippy::too_many_arguments)]
    fn keyed(
        badges: u8,
        route: u8,
        events: u16,
        map: u8,
        x: u8,
        y: u8,
        hp: u32,
        levels: u32,
    ) -> BlueArchiveKey {
        BlueArchiveKey {
            badges,
            route,
            events,
            map,
            cell_x: x / CELL_STEPS,
            cell_y: y / CELL_STEPS,
            party_hp: hp,
            party_levels: levels,
        }
    }

    #[test]
    fn a_badge_outranks_every_route_flag_and_a_place_never_ranks() {
        let badge = key(1, 0, 0, 0, 0, 0, 0);
        let route = key(0, 0x7f, 9, 9, 9, 0, 0);
        assert_eq!(
            BlueArchiveKey::progress_cmp(badge.group(0), route.group(0)),
            Ordering::Greater
        );
        let far = key(0, 0x7f, 200, 40, 40, 0, 0);
        assert_eq!(
            BlueArchiveKey::progress_cmp(route.group(0), far.group(0)),
            Ordering::Equal
        );
    }

    #[test]
    fn two_steps_share_a_cell_and_the_coarser_groups_drop_the_place() {
        let left = key(0, 1, 12, 10, 28, 20, 6);
        let right = key(0, 1, 12, 11, 29, 20, 6);
        assert_eq!(left.group(0), right.group(0));
        assert_ne!(left.group(0), key(0, 1, 12, 12, 28, 20, 6).group(0));
        assert_eq!(left.group(1), key(0, 1, 12, 30, 2, 20, 6).group(1));
        assert_eq!(left.group(2), key(0, 1, 40, 30, 2, 20, 6).group(2));
        assert_eq!(left.group(3), keyed(0, 1, 9, 40, 30, 2, 20, 6).group(3));
        assert_eq!(left.group(4), key(0, 0x40, 40, 30, 2, 20, 6).group(4));
    }

    #[test]
    fn a_set_event_flag_is_a_new_place_and_outranks_the_same_place_without_it() {
        let before = keyed(0, 0, 1, 40, 7, 4, 0, 0);
        let after = keyed(0, 0, 4, 40, 7, 4, 0, 0);
        assert_ne!(before.group(0), after.group(0));
        assert_eq!(
            BlueArchiveKey::progress_cmp(after.group(0), before.group(0)),
            Ordering::Greater
        );
        assert_eq!(
            BlueArchiveKey::progress_cmp(keyed(0, 1, 0, 40, 7, 4, 0, 0).group(0), after.group(0)),
            Ordering::Greater
        );
    }

    #[test]
    fn the_slot_prefers_more_health_then_more_levels() {
        let healthy = key(0, 1, 12, 10, 28, 30, 6);
        let hurt = key(0, 1, 12, 10, 28, 10, 9);
        assert_eq!(healthy.preference_cmp(hurt), Ordering::Greater);
        let taller = key(0, 1, 12, 10, 28, 30, 9);
        assert_eq!(taller.preference_cmp(healthy), Ordering::Greater);
    }

    #[test]
    fn a_drawn_action_names_a_macro_and_an_alphabet_slot() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        for _ in 0..256 {
            let action = sample_action(&mut rand).unwrap();
            assert!(ACTION_KINDS.contains(&action.kind));
            assert!(action_time(&action) <= longest_action_frames());
        }
    }
}
