// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metroid archive keys, state preferences, and report shapes.

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

/// Largest bounded input horizon accepted by a Metroid campaign.
pub const MAX_METROID_ACTIONS: usize = 8_192;
/// Recorded archive-key and per-location preference policy.
pub const KEY_POLICY_IDENTIFIER: &str =
    "metroid_items_tanks_area_map_spatial_16_posture_door_preference_missiles_first_ridley_bit1_v8";
/// Recorded same-slot replacement policy.
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";

/// Areas the world is divided into.
const AREAS: u16 = 8;

/// The Metroid archive instantiation.
pub type MetroidArchive =
    Archive<ButtonChord, MetroidArchiveKey, MetroidMilestones, MetroidSnapshot>;

/// Opaque pooled identity returned to the generic selector.
///
/// The field order carries the progress order the archive derives from
/// `Ord`: what a route has permanently gained first, then where it stands.
/// One representative per position holds the place, whichever route reached
/// it; a count of cells a route has crossed would rank a route that swept
/// one corridor above a route that went straight to an exit.
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

/// Quality-diversity key for one Metroid endpoint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveKey {
    /// Equipment items obtained.
    pub items: u8,
    /// Missile and energy tanks collected.
    pub tanks: u8,
    /// Area of the world. The areas are connected in both directions and
    /// are revisited throughout, so this names a place and never ranks one.
    pub area: u8,
    /// Map column.
    pub map_x: u8,
    /// Map row.
    pub map_y: u8,
    /// Samus's horizontal 16-pixel bucket within the screen.
    pub x: u8,
    /// Samus's vertical 16-pixel bucket within the screen.
    pub y: u8,
    /// Whether Samus is grounded, airborne, or in another pose. A ledge
    /// reachable only from a jump shares its pixels with the ground below.
    pub posture: u8,
    /// Door transition state; a door is the only route between many rooms
    /// and takes several frames to cross.
    pub door: u8,
    /// Health in tenths.
    pub health: u16,
    /// Missiles carried.
    pub missiles: u8,
}

impl ArchiveKey for MetroidArchiveKey {
    type Group = MetroidArchiveGroup;

    fn groups() -> usize {
        5
    }

    /// Map cells are places in a graph, so only holdings rank one frontier
    /// band ahead of another.
    fn progress_cmp(left: Self::Group, right: Self::Group) -> Ordering {
        (left.items, left.tanks).cmp(&(right.items, right.tanks))
    }

    /// Depth 0 is one 16-pixel location, depth 1 one 32-pixel selection
    /// cell, depth 2 a 128-pixel region of a screen, depth 3 the map cell,
    /// and depth 4 the items and tanks held. Posture and the door state
    /// split locations and selection cells only. Resource fields never
    /// multiply slots; they decide which one representative remains.
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

/// Samus's position per key bucket, in pixels.
const POSITION_BUCKET: u8 = 16;

/// Build the opaque archive key from a decoded state.
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

/// Strongest rungs observed by a campaign.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestones {
    /// Greatest equipment item count reached.
    pub items: u8,
    /// Greatest combined count of missile and energy tanks reached.
    pub tanks: u8,
    /// Areas entered, one bit per area.
    pub areas: u8,
    /// Whether any input gained an item or tank beyond the sealed genesis.
    pub gained: bool,
}

/// First deterministic execution reaching each rung.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestoneTimes {
    /// First execution that entered an area beyond the starting one.
    pub first_new_area: Option<u64>,
    /// First execution that gained an item or tank.
    pub first_gain: Option<u64>,
}

/// First clean-reset input reaching each rung.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidMilestoneInputs {
    /// First input that entered an area beyond the starting one.
    pub first_new_area: Option<MetroidInput>,
    /// First input that gained an item or tank.
    pub first_gain: Option<MetroidInput>,
}

/// Strongest lexicographic mechanical position seen at any emulated frame.
///
/// Only what a route permanently gains belongs here. Position is left out
/// because the world is crossed in both directions, so no coordinate rises
/// along the intended course.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidProgressWatermark {
    /// Equipment items obtained.
    pub items: u8,
    /// Missile capacity, which rises with each missile tank collected.
    pub missile_capacity: u8,
}

/// Progress curve point.
pub type MetroidArchiveProgressPoint = ProgressPoint<MetroidMilestones, MetroidProgressWatermark>;
/// Archive entry report.
pub type MetroidArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, MetroidArchiveKey, MetroidMilestones>;

/// Complete deterministic report for one Metroid campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidArchiveReport {
    /// Campaign seed.
    pub seed: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Strongest milestones.
    pub milestones: MetroidMilestones,
    /// Strongest per-frame mechanical progress.
    pub progress_watermark: MetroidProgressWatermark,
    /// First execution reaching each rung.
    pub first_reached: MetroidMilestoneTimes,
    /// First input reaching each rung.
    pub first_inputs: MetroidMilestoneInputs,
    /// Best input under the adapter's progress order.
    pub champion_input: MetroidInput,
    /// Retained per-location representatives.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<MetroidArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<MetroidArchiveProgressPoint>,
    /// Candidates admitted.
    pub retained: u64,
    /// Candidates rejected or superseded.
    pub rejected: u64,
    /// Terminal deaths observed.
    pub deaths: u64,
    /// Generic selector accounting.
    #[serde(default)]
    pub selector: SelectorAccounting,
}

/// Decode milestones from one state relative to the sealed genesis holdings.
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

/// Merge strongest milestone fields.
pub fn merge_milestones(into: &mut MetroidMilestones, from: MetroidMilestones) {
    into.items = into.items.max(from.items);
    into.tanks = into.tanks.max(from.tanks);
    into.areas |= from.areas;
    into.gained |= from.gained;
}

/// Stable champion order owned by the adapter.
#[must_use]
pub fn milestone_key(value: MetroidMilestones) -> (bool, u8, u8, u32) {
    (
        value.gained,
        value.items,
        value.tanks,
        value.areas.count_ones(),
    )
}

/// The watermark of one decoded state.
#[must_use]
pub fn progress_watermark(state: MetroidMechanicalState) -> MetroidProgressWatermark {
    MetroidProgressWatermark {
        items: state.items(),
        missile_capacity: state.missile_capacity,
    }
}

/// Fold every action-interior observation into the progress watermark.
pub fn merge_progress_watermark(
    watermark: &mut MetroidProgressWatermark,
    observations: &[MetroidObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_watermark(observation.decoded));
    }
}

/// Held-frame clock used by same-slot route replacement.
pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

/// Longest controller hold the vocabulary draws.
pub const LONGEST_HOLD_FRAMES: u8 = 120;

/// Recorded controller vocabulary.
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];
const SELECT: u8 = 0x04;
/// One chord in this many is a Select tap. Select swaps between the beam and
/// missiles, and missiles are the only way through a red door or past
/// several bosses, so the vocabulary has to reach it.
const SELECT_ODDS: usize = 12;

/// Draw one game-neutral controller chord: any direction set with any A/B
/// set, or a short Select tap. Start is excluded because it only pauses.
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
