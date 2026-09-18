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
pub const KEY_POLICY_IDENTIFIER: &str = "metroid_items_tanks_boss_damage_map_spatial_16_posture_door_area_last_preference_missiles_only_ridley_bit1_v12";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";

const AREAS: u16 = 8;
const BOSS_DAMAGE_BUCKET: u8 = 8;

pub type MetroidArchive =
    Archive<ButtonChord, MetroidArchiveKey, MetroidMilestones, MetroidSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveGroup {
    items: u8,
    tanks: u8,
    boss_damage: u8,
    map_x: u8,
    map_y: u8,
    x: u8,
    y: u8,
    posture: u8,
    door: u8,
    area: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveKey {
    pub items: u8,
    pub tanks: u8,
    pub boss_damage: u8,
    pub boss_health: u8,
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
        (left.items, left.boss_damage).cmp(&(right.items, right.boss_damage))
    }

    fn group(self, depth: usize) -> Self::Group {
        let location = MetroidArchiveGroup {
            items: self.items,
            tanks: self.tanks,
            boss_damage: self.boss_damage,
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
                boss_damage: self.boss_damage,
                area: self.area,
                map_x: self.map_x,
                map_y: self.map_y,
                ..MetroidArchiveGroup::default()
            },
            _ => MetroidArchiveGroup {
                items: self.items,
                tanks: self.tanks,
                boss_damage: self.boss_damage,
                ..MetroidArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        match preference {
            0 => self.preference().cmp(&other.preference()),
            _ => self.health_preference().cmp(&other.health_preference()),
        }
    }

    type Lineage = MetroidLineage;

    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
        let highest = parent
            .map_or(0, |(_, lineage)| lineage.boss_health_highest)
            .max(self.boss_health);
        Self {
            boss_damage: highest.saturating_sub(self.boss_health) / BOSS_DAMAGE_BUCKET,
            ..self
        }
    }

    fn record(lineage: &mut Self::Lineage, key: Self) {
        lineage.boss_health_highest = lineage.boss_health_highest.max(key.boss_health);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetroidLineage {
    boss_health_highest: u8,
}

impl MetroidArchiveKey {
    fn preference(self) -> (u8, u8, u8, u16) {
        (self.items, self.tanks, self.missiles, self.health)
    }

    fn health_preference(self) -> (u8, u8, u16, u8) {
        (self.items, self.tanks, self.health, self.missiles)
    }
}

const POSITION_BUCKET: u8 = 16;

#[must_use]
pub fn archive_key(state: MetroidMechanicalState) -> MetroidArchiveKey {
    let (items, tanks, health, missiles) = preference_tuple(state);
    MetroidArchiveKey {
        items,
        tanks,
        boss_damage: 0,
        boss_health: state.boss_health,
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

    #[test]
    fn the_area_byte_does_not_rank_one_boss_area_over_the_other() {
        let kraid = archive_key(MetroidMechanicalState {
            area: 0x12,
            map_x: 1,
            map_y: 9,
            ..MetroidMechanicalState::default()
        });
        let ridley = archive_key(MetroidMechanicalState {
            area: 0x14,
            map_x: 0,
            map_y: 0,
            ..MetroidMechanicalState::default()
        });
        assert_eq!(
            MetroidArchiveKey::progress_cmp(kraid.group(0), ridley.group(0)),
            Ordering::Equal
        );
        assert!(kraid.group(0) > ridley.group(0));
        assert!(KEY_POLICY_IDENTIFIER.contains("area_last"));
    }

    #[test]
    fn a_boss_kill_does_not_relabel_every_map_cell_as_holding_more_tanks() {
        let mut collected = state(100, 300, 0);
        collected.missile_capacity = 30;
        let mut killed = state(100, 300, 0);
        killed.missile_capacity = 30 + 75;
        killed.bosses = 1;

        assert_eq!(collected.collectibles(), 6);
        assert_eq!(killed.collectibles(), 6);
        assert_eq!(killed.items(), collected.items() + 1);
        assert_eq!(archive_key(killed).tanks, archive_key(collected).tanks);
    }

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
        assert_eq!(strong.preference_cmp(0, weak), Ordering::Greater);
        assert_eq!(MetroidArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn the_only_preference_ranks_missiles_before_health() {
        let mut stocked_state = state(100, 20, 0);
        stocked_state.missiles = 10;
        let mut healthy_state = state(100, 200, 0);
        healthy_state.missiles = 5;
        let stocked = archive_key(stocked_state);
        let healthy = archive_key(healthy_state);
        assert_eq!(MetroidArchiveKey::preferences(), 1);
        assert_eq!(stocked.preference_cmp(0, healthy), Ordering::Greater);
    }

    #[test]
    fn an_item_outranks_every_resource_under_the_preference() {
        let stocked = archive_key(state(100, 300, 0));
        let equipped = archive_key(state(10, 10, 0b1));
        assert_eq!(equipped.preference_cmp(0, stocked), Ordering::Greater);
    }

    #[test]
    fn an_item_outranks_every_resource_in_the_preference() {
        let stocked = archive_key(state(100, 300, 0));
        let equipped = archive_key(state(100, 10, 0b1));
        assert_eq!(equipped.preference_cmp(0, stocked), Ordering::Greater);
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

    #[test]
    fn the_progress_relation_is_a_total_preorder() {
        let states = [
            MetroidMechanicalState::default(),
            MetroidMechanicalState {
                area: 0x12,
                map_x: 3,
                map_y: 4,
                ..MetroidMechanicalState::default()
            },
            MetroidMechanicalState {
                equipment: 0b11,
                energy_tanks: 2,
                ..MetroidMechanicalState::default()
            },
            MetroidMechanicalState {
                equipment: 0b1,
                energy_tanks: 5,
                area: 0x14,
                ..MetroidMechanicalState::default()
            },
        ];
        let groups = (0..MetroidArchiveKey::groups())
            .flat_map(|depth| {
                states
                    .into_iter()
                    .map(move |state| archive_key(state).group(depth))
            })
            .collect::<Vec<_>>();
        crate::search::archive::check_total_preorder::<MetroidArchiveKey>(&groups)
            .expect("Metroid progress relation");
    }

    #[test]
    fn damaging_a_boss_advances_progress_within_one_item_count() {
        let lineage = MetroidLineage {
            boss_health_highest: 64,
        };
        let arriving = archive_key(MetroidMechanicalState {
            equipment: 0b1,
            boss_health: 64,
            ..MetroidMechanicalState::default()
        })
        .complete(Some((MetroidArchiveKey::default(), &lineage)));
        let hurt = archive_key(MetroidMechanicalState {
            equipment: 0b1,
            boss_health: 24,
            ..MetroidMechanicalState::default()
        })
        .complete(Some((MetroidArchiveKey::default(), &lineage)));
        assert_eq!(arriving.boss_damage, 0);
        assert_eq!(hurt.boss_damage, 5);
        assert_eq!(
            MetroidArchiveKey::progress_cmp(hurt.group(1), arriving.group(1)),
            Ordering::Greater
        );
        assert_ne!(hurt.group(0), arriving.group(0));
    }

    #[test]
    fn a_lineage_remembers_the_highest_boss_health_it_saw() {
        let mut lineage = MetroidLineage::default();
        let arriving = archive_key(MetroidMechanicalState {
            boss_health: 64,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, arriving);
        assert_eq!(lineage.boss_health_highest, 64);
        let hurt = archive_key(MetroidMechanicalState {
            boss_health: 10,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, hurt);
        assert_eq!(lineage.boss_health_highest, 64);
        assert_eq!(
            hurt.complete(Some((arriving, &lineage))).boss_damage,
            (64 - 10) / BOSS_DAMAGE_BUCKET
        );
    }

    #[test]
    fn no_boss_in_the_room_is_no_damage() {
        let key = archive_key(MetroidMechanicalState::default()).complete(Some((
            MetroidArchiveKey::default(),
            &MetroidLineage::default(),
        )));
        assert_eq!(key.boss_health, 0);
        assert_eq!(key.boss_damage, 0);
    }

    #[test]
    fn tanks_are_capacity_and_do_not_advance_progress() {
        let one_tank = archive_key(MetroidMechanicalState {
            equipment: 0b1,
            energy_tanks: 1,
            ..MetroidMechanicalState::default()
        });
        let five_tanks = archive_key(MetroidMechanicalState {
            equipment: 0b1,
            energy_tanks: 5,
            ..MetroidMechanicalState::default()
        });
        assert_eq!(
            MetroidArchiveKey::progress_cmp(five_tanks.group(1), one_tank.group(1)),
            Ordering::Equal
        );
    }
}
