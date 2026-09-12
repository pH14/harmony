// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    metroid::target::{
        ButtonChord, MetroidInput, MetroidMechanicalState, MetroidObservations, MetroidSnapshot,
        preference_tuple,
    },
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
            entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

pub const MAX_METROID_ACTIONS: usize = 8_192;
pub const KEY_POLICY_IDENTIFIER: &str =
    "metroid_items_tanks_area_map_spatial_16_posture_door_preference_missiles_first_ridley_bit1_v8";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";

const AREAS: u16 = 8;

pub type MetroidArchive =
    Archive<ButtonChord, MetroidArchiveKey, MetroidMilestones, MetroidSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveGroup {
    items: u8,
    tanks: u8,
    area: u8,
    map_x: u8,
    map_y: u8,
    x: u8,
    y: u8,
    posture: u8,
    door: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveKey {
    pub items: u8,
    pub tanks: u8,
    pub area: u8,
    pub map_x: u8,
    pub map_y: u8,
    pub x: u8,
    pub y: u8,
    pub posture: u8,
    pub door: u8,
    pub health: u16,
    pub missiles: u8,
}

impl ArchiveKey for MetroidArchiveKey {
    type Group = MetroidArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn progress_cmp(left: Self::Group, right: Self::Group) -> Ordering {
        (left.items, left.tanks).cmp(&(right.items, right.tanks))
    }

    fn group(self, depth: usize) -> Self::Group {
        let location = MetroidArchiveGroup {
            items: self.items,
            tanks: self.tanks,
            area: self.area,
            map_x: self.map_x,
            map_y: self.map_y,
            x: self.x,
            y: self.y,
            posture: self.posture,
            door: self.door,
        };
        match depth {
            0 => location,
            1 => MetroidArchiveGroup {
                x: self.x / 2,
                y: self.y / 2,
                ..location
            },
            2 => MetroidArchiveGroup {
                x: self.x / 8,
                y: self.y / 8,
                posture: 0,
                door: 0,
                ..location
            },
            3 => MetroidArchiveGroup {
                items: self.items,
                tanks: self.tanks,
                area: self.area,
                map_x: self.map_x,
                map_y: self.map_y,
                ..MetroidArchiveGroup::default()
            },
            _ => MetroidArchiveGroup {
                items: self.items,
                tanks: self.tanks,
                ..MetroidArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn preference_cmp(self, other: Self) -> Ordering {
        self.preference().cmp(&other.preference())
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

impl MetroidArchiveKey {
    fn preference(self) -> (u8, u8, u8, u16) {
        (self.items, self.tanks, self.missiles, self.health)
    }
}

const POSITION_BUCKET: u8 = 16;

#[must_use]
pub fn archive_key(state: MetroidMechanicalState) -> MetroidArchiveKey {
    let (items, tanks, health, missiles) = preference_tuple(state);
    MetroidArchiveKey {
        items,
        tanks,
        area: state.area,
        map_x: state.map_x,
        map_y: state.map_y,
        x: state.x / POSITION_BUCKET,
        y: state.y / POSITION_BUCKET,
        posture: state.posture(),
        door: state.door,
        health,
        missiles,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestones {
    pub items: u8,
    pub tanks: u8,
    pub areas: u8,
    pub gained: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestoneTimes {
    pub first_new_area: Option<u64>,
    pub first_gain: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestoneInputs {
    pub first_new_area: Option<MetroidInput>,
    pub first_gain: Option<MetroidInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidProgressWatermark {
    pub items: u8,
    pub missile_capacity: u8,
}

pub type MetroidArchiveProgressPoint = ProgressPoint<MetroidMilestones, MetroidProgressWatermark>;
pub type MetroidArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, MetroidArchiveKey, MetroidMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: MetroidMilestones,
    pub progress_watermark: MetroidProgressWatermark,
    pub first_reached: MetroidMilestoneTimes,
    pub first_inputs: MetroidMilestoneInputs,
    pub champion_input: MetroidInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<MetroidArchiveEntryReport>,
    pub progress_curve: Vec<MetroidArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(
    state: MetroidMechanicalState,
    genesis_items: u8,
    genesis_tanks: u8,
) -> MetroidMilestones {
    MetroidMilestones {
        items: state.items(),
        tanks: state.collectibles(),
        areas: 1_u8 << (u16::from(state.area) % AREAS),
        gained: state.items() > genesis_items || state.collectibles() > genesis_tanks,
    }
}

pub fn merge_milestones(into: &mut MetroidMilestones, from: MetroidMilestones) {
    into.items = into.items.max(from.items);
    into.tanks = into.tanks.max(from.tanks);
    into.areas |= from.areas;
    into.gained |= from.gained;
}

#[must_use]
pub fn milestone_key(value: MetroidMilestones) -> (bool, u8, u8, u32) {
    (
        value.gained,
        value.items,
        value.tanks,
        value.areas.count_ones(),
    )
}

#[must_use]
pub fn progress_watermark(state: MetroidMechanicalState) -> MetroidProgressWatermark {
    MetroidProgressWatermark {
        items: state.items(),
        missile_capacity: state.missile_capacity,
    }
}

pub fn merge_progress_watermark(
    watermark: &mut MetroidProgressWatermark,
    observations: &[MetroidObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_watermark(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const LONGEST_HOLD_FRAMES: u8 = 120;

pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];
const SELECT: u8 = 0x04;
const SELECT_ODDS: usize = 12;

pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    if rand.below(NonZeroUsize::new(SELECT_ODDS).ok_or("invalid select odds")?) == 0 {
        let hold = u8::try_from(2 + rand.below(NonZeroUsize::new(6).ok_or("invalid tap")?))?;
        return Ok(ButtonChord::new(SELECT, hold));
    }
    let direction = DIRECTIONS
        [rand.below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty direction vocabulary")?)];
    let buttons =
        direction | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty A/B vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(2).ok_or("invalid duration odds")?) == 0 {
        u8::try_from(2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short duration")?))?
    } else {
        u8::try_from(48 + rand.below(NonZeroUsize::new(73).ok_or("invalid long duration")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(x: u8, health: u16, equipment: u8) -> MetroidMechanicalState {
        MetroidMechanicalState {
            area: 0,
            map_x: 3,
            map_y: 14,
            x,
            y: 0xb1,
            health,
            equipment,
            ..MetroidMechanicalState::default()
        }
    }

    #[test]
    fn one_location_uses_resources_only_for_preference() {
        let weak = archive_key(state(100, 40, 0));
        let strong = archive_key(state(100, 300, 0));
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(weak.group(1), strong.group(1));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(MetroidArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn an_item_outranks_every_resource_in_the_preference() {
        let stocked = archive_key(state(100, 300, 0));
        let equipped = archive_key(state(100, 10, 0b1));
        assert_eq!(equipped.preference_cmp(stocked), Ordering::Greater);
    }

    #[test]
    fn map_cells_are_separate_groups_below_the_items_held() {
        let here = archive_key(state(100, 300, 0));
        let mut moved = state(100, 300, 0);
        moved.map_x = 4;
        let there = archive_key(moved);
        assert_ne!(here.group(3), there.group(3));
        assert_eq!(here.group(4), there.group(4));
    }

    #[test]
    fn progress_orders_items_then_tanks_ahead_of_position() {
        let mut far = state(0, 300, 0);
        far.map_x = 9;
        let far = archive_key(far);
        let equipped = archive_key(state(0, 300, 0b1));
        assert!(equipped.group(4) > far.group(4));
        assert!(equipped.group(3) > far.group(3));
    }

    #[test]
    fn vocabulary_draws_select_taps_and_never_start_or_conflicting_directions() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut selects = 0;
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x08, 0);
            if chord.buttons & SELECT != 0 {
                assert_eq!(chord.buttons, SELECT);
                assert!(chord.hold_frames <= 8);
                selects += 1;
            }
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
        assert!(selects > 0, "the vocabulary must reach missiles");
    }

    #[test]
    fn milestones_are_relative_to_genesis_holdings() {
        assert!(!milestones(state(0, 300, 0b1), 1, 0).gained);
        assert!(milestones(state(0, 300, 0b11), 1, 0).gained);
    }
}
