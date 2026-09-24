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
pub const KEY_POLICY_IDENTIFIER: &str = "metroid_items_progress_area_map_cell_boss_damage_columns_place_spatial_16_posture_door_identity_tanks_missiles_health_two_preferences_tier_items_boss_engaged_mother_brain_defeat_item_door_transition_keeps_cell";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";

const AREAS: u16 = 8;
const BOSS_DAMAGE_BUCKET: u16 = 4;

pub type MetroidArchive =
    Archive<ButtonChord, MetroidArchiveKey, MetroidMilestones, MetroidSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MetroidArchiveKey {
    pub items: u8,
    pub tanks: u8,
    pub boss_damage: u8,
    pub boss_health: u16,
    pub boss_health_seen: u16,
    pub area: u8,
    pub map_x: u8,
    pub map_y: u8,
    pub x: u8,
    pub y: u8,
    pub posture: u8,
    pub door: u8,
    pub columns: u8,
    pub health: u16,
    pub missiles: u8,
}

impl ArchiveKey for MetroidArchiveKey {
    type Place = (u8, u8, u8, u8, u8);
    type Progress = (u8, bool);
    type Identity = (u8, u8, u8, u8);

    fn place(self) -> Self::Place {
        (
            self.area,
            self.map_x,
            self.map_y,
            self.boss_damage,
            self.columns,
        )
    }

    fn progress(self) -> Self::Progress {
        (self.items, self.boss_damage > 0)
    }

    fn identity(self) -> Self::Identity {
        (self.x, self.y, self.posture, self.door)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        2
    }

    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        match preference {
            0 => self.preference().cmp(&other.preference()),
            _ => self.health_preference().cmp(&other.health_preference()),
        }
    }

    type Lineage = MetroidLineage;

    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
        let same_cell = parent.is_some_and(|(key, _)| key.cell() == self.cell());
        if self.boss_health == 0 {
            return Self {
                boss_damage: parent
                    .filter(|(key, _)| same_cell && key.items == self.items)
                    .map_or(0, |(key, _)| key.boss_damage),
                ..self
            };
        }
        let inherited = parent.map_or(0, |(_, lineage)| lineage.boss_health_highest);
        let highest = inherited.max(self.boss_health_seen).max(self.boss_health);
        Self {
            boss_damage: u8::try_from(
                highest.saturating_sub(self.boss_health) / BOSS_DAMAGE_BUCKET,
            )
            .unwrap_or(u8::MAX),
            ..self
        }
    }

    fn record(lineage: &mut Self::Lineage, key: Self) {
        if key.boss_health == 0 && lineage.cell != key.cell() {
            lineage.boss_health_highest = 0;
        }
        lineage.cell = key.cell();
        lineage.boss_health_highest = lineage
            .boss_health_highest
            .max(key.boss_health_seen)
            .max(key.boss_health);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetroidLineage {
    boss_health_highest: u16,
    cell: (u8, u8, u8),
}

impl MetroidArchiveKey {
    fn cell(self) -> (u8, u8, u8) {
        (self.area, self.map_x, self.map_y)
    }

    #[must_use]
    pub fn with_boss_health_seen(self, boss_health_seen: u16) -> Self {
        Self {
            boss_health_seen: boss_health_seen.max(self.boss_health),
            ..self
        }
    }

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
        items: items.saturating_add(u8::from(state.mother_brain_defeated)),
        tanks,
        boss_damage: 0,
        boss_health: state.boss_health,
        boss_health_seen: state.boss_health,
        area: state.area,
        map_x: state.map_x,
        map_y: state.map_y,
        x: state.x / POSITION_BUCKET,
        y: state.y / POSITION_BUCKET,
        posture: state.posture(),
        door: state.door,
        columns: state.zebetite_hits_left,
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
    use crate::search::archive::ArchiveCandidate;

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
        assert_eq!(kraid.progress(), ridley.progress());
        assert_ne!(kraid.place(), ridley.place());
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
        assert_eq!(weak.place(), strong.place());
        assert_eq!(weak.identity(), strong.identity());
        assert_eq!(strong.preference_cmp(0, weak), Ordering::Greater);
        assert_eq!(strong.preference_cmp(1, weak), Ordering::Greater);
        assert_eq!(MetroidArchiveKey::capacity(), 1);
    }

    #[test]
    fn the_two_preferences_disagree_on_missiles_against_health() {
        let mut stocked_state = state(100, 20, 0);
        stocked_state.missiles = 10;
        let mut healthy_state = state(100, 200, 0);
        healthy_state.missiles = 5;
        let stocked = archive_key(stocked_state);
        let healthy = archive_key(healthy_state);
        assert_eq!(MetroidArchiveKey::preferences(), 2);
        assert_eq!(stocked.preference_cmp(0, healthy), Ordering::Greater);
        assert_eq!(stocked.preference_cmp(1, healthy), Ordering::Less);
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
    fn column_states_are_separate_places_with_one_holder_identity() {
        let clear = archive_key(state(100, 300, 0));
        let mut live = state(100, 300, 0);
        live.zebetite_hits_left = 13;
        let live = archive_key(live);
        assert_ne!(clear, live);
        assert_ne!(clear.place(), live.place());
        assert_eq!(clear.identity(), live.identity());
    }

    #[test]
    fn map_cells_are_separate_places_under_one_items_tier() {
        let here = archive_key(state(100, 300, 0));
        let mut moved = state(100, 300, 0);
        moved.map_x = 4;
        let there = archive_key(moved);
        assert_ne!(here.place(), there.place());
        assert_eq!(here.progress(), there.progress());
    }

    #[test]
    fn progress_is_the_items_held_and_tanks_stay_in_the_preference() {
        let mut far = state(0, 300, 0);
        far.map_x = 9;
        let far = archive_key(far);
        let equipped = archive_key(state(0, 300, 0b1));
        assert!(equipped.progress() > far.progress());
        let mut tanked = state(0, 300, 0);
        tanked.energy_tanks = 3;
        let tanked = archive_key(tanked);
        assert_eq!(tanked.progress(), far.progress());
        assert_eq!(tanked.preference_cmp(0, far), Ordering::Greater);
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
    fn damaging_a_boss_moves_the_state_to_the_engaged_tier_and_a_place_per_damage_level() {
        let lineage = MetroidLineage {
            boss_health_highest: 64,
            cell: (0, 0, 0),
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
        let harder = archive_key(MetroidMechanicalState {
            equipment: 0b1,
            boss_health: 4,
            ..MetroidMechanicalState::default()
        })
        .complete(Some((MetroidArchiveKey::default(), &lineage)));
        assert_eq!(arriving.progress(), (1, false));
        assert_eq!(hurt.progress(), (1, true));
        assert_eq!(harder.progress(), hurt.progress());
        assert_ne!(hurt.place(), arriving.place());
        assert_ne!(harder.place(), hurt.place());
        assert_eq!(hurt.identity(), arriving.identity());
    }

    #[test]
    fn mother_brain_defeat_adds_an_item_and_clears_the_boss_damage() {
        let parent = MetroidArchiveKey {
            items: 1,
            boss_damage: 31,
            area: 0x13,
            ..MetroidArchiveKey::default()
        };
        let lineage = MetroidLineage::default();
        let state = MetroidMechanicalState {
            area: 0x13,
            equipment: 0b1,
            ..MetroidMechanicalState::default()
        };
        let before = archive_key(state).complete(Some((parent, &lineage)));
        let after = archive_key(MetroidMechanicalState {
            mother_brain_defeated: true,
            ..state
        })
        .complete(Some((parent, &lineage)));
        assert_eq!(before.progress(), (1, true));
        assert_eq!(after.progress(), (2, false));
        assert_eq!(after.boss_damage, 0);
    }

    #[test]
    fn an_absent_boss_reading_carries_the_parent_damage() {
        let lineage = MetroidLineage {
            boss_health_highest: 96,
            cell: (0, 0, 0),
        };
        let parent = MetroidArchiveKey {
            boss_damage: 6,
            boss_health: 72,
            ..MetroidArchiveKey::default()
        };
        let flash = MetroidArchiveKey {
            boss_health: 0,
            ..MetroidArchiveKey::default()
        }
        .complete(Some((parent, &lineage)));
        assert_eq!(flash.boss_damage, 6);
        assert_eq!(
            MetroidArchiveKey {
                boss_health: 0,
                ..MetroidArchiveKey::default()
            }
            .complete(None)
            .boss_damage,
            0
        );
        let back = MetroidArchiveKey {
            boss_health: 96,
            ..MetroidArchiveKey::default()
        }
        .complete(Some((flash, &lineage)));
        assert_eq!(back.boss_damage, 0);
    }

    #[test]
    fn the_highest_carries_into_the_next_cell_while_a_reading_is_present() {
        let mut lineage = MetroidLineage::default();
        let shaft = archive_key(MetroidMechanicalState {
            map_x: 5,
            map_y: 11,
            boss_health: 288,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, shaft);
        let her_screen = archive_key(MetroidMechanicalState {
            map_x: 3,
            map_y: 11,
            boss_health: 256,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        })
        .complete(Some((shaft, &lineage)));
        assert_eq!(her_screen.boss_damage, 8);
        MetroidArchiveKey::record(&mut lineage, her_screen);
        assert_eq!(lineage.boss_health_highest, 288);
        let outside = archive_key(MetroidMechanicalState {
            map_x: 9,
            map_y: 29,
            boss_health: 0,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, outside);
        assert_eq!(lineage.boss_health_highest, 0);
    }

    #[test]
    fn a_lineage_remembers_the_highest_boss_health_it_saw() {
        let mut lineage = MetroidLineage::default();
        let arriving = archive_key(MetroidMechanicalState {
            boss_health: 64,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, arriving);
        assert_eq!(lineage.boss_health_highest, 64);
        let hurt = archive_key(MetroidMechanicalState {
            boss_health: 10,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        });
        MetroidArchiveKey::record(&mut lineage, hurt);
        assert_eq!(lineage.boss_health_highest, 64);
        assert_eq!(
            u16::from(hurt.complete(Some((arriving, &lineage))).boss_damage),
            (64 - 10) / BOSS_DAMAGE_BUCKET
        );
    }

    #[test]
    fn the_highest_reading_within_one_execution_sets_the_damage() {
        let mut lineage = MetroidLineage::default();
        let first_retained = archive_key(MetroidMechanicalState {
            boss_health: 48,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        })
        .with_boss_health_seen(96)
        .complete(Some((MetroidArchiveKey::default(), &lineage)));
        assert_eq!(first_retained.boss_damage, 12);
        MetroidArchiveKey::record(&mut lineage, first_retained);
        assert_eq!(lineage.boss_health_highest, 96);
        let later = archive_key(MetroidMechanicalState {
            boss_health: 40,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        })
        .with_boss_health_seen(48)
        .complete(Some((first_retained, &lineage)));
        assert_eq!(later.boss_damage, 14);
        assert_eq!(
            archive_key(MetroidMechanicalState {
                boss_health: 64,
                zebetites_destroyed: 0,
                zebetite_hits_left: 0,
                ..MetroidMechanicalState::default()
            })
            .with_boss_health_seen(10)
            .boss_health_seen,
            64
        );
    }

    #[test]
    fn leaving_the_map_cell_clears_the_reading() {
        let mut lineage = MetroidLineage::default();
        let room = MetroidMechanicalState {
            area: 18,
            map_x: 8,
            map_y: 29,
            boss_health: 96,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        };
        let arriving = archive_key(room).complete(None);
        MetroidArchiveKey::record(&mut lineage, arriving);
        let hurt = archive_key(MetroidMechanicalState {
            boss_health: 48,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..room
        })
        .complete(Some((arriving, &lineage)));
        assert_eq!(hurt.boss_damage, 12);
        MetroidArchiveKey::record(&mut lineage, hurt);
        let outside = archive_key(MetroidMechanicalState {
            map_x: 9,
            boss_health: 0,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..room
        })
        .complete(Some((hurt, &lineage)));
        assert_eq!(outside.boss_damage, 0);
        MetroidArchiveKey::record(&mut lineage, outside);
        assert_eq!(lineage.boss_health_highest, 0);
        let back = archive_key(MetroidMechanicalState {
            boss_health: 96,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..room
        })
        .complete(Some((outside, &lineage)));
        assert_eq!(back.boss_damage, 0);
        MetroidArchiveKey::record(&mut lineage, back);
        let hurt_again = archive_key(MetroidMechanicalState {
            boss_health: 80,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..room
        })
        .complete(Some((back, &lineage)));
        assert_eq!(hurt_again.boss_damage, 4);
        let stayed = archive_key(MetroidMechanicalState {
            boss_health: 0,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..room
        })
        .complete(Some((hurt, &lineage)));
        assert_eq!(stayed.boss_damage, 12);
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

    fn tourian(
        map_x: u8,
        map_y: u8,
        health: u16,
        missiles: u8,
        boss_health: u16,
    ) -> MetroidMechanicalState {
        MetroidMechanicalState {
            area: 0x14,
            map_x,
            map_y,
            x: 0x40,
            y: 0xb0,
            health,
            missiles,
            missile_capacity: 175,
            equipment: 0b1_1111,
            energy_tanks: 4,
            boss_health,
            zebetites_destroyed: 0,
            zebetite_hits_left: 0,
            ..MetroidMechanicalState::default()
        }
    }

    fn held(
        archive: &mut MetroidArchive,
        parent: Option<usize>,
        execution: u64,
        suffix: u8,
        state: MetroidMechanicalState,
    ) -> Option<usize> {
        archive
            .insert(
                parent,
                execution,
                ArchiveCandidate {
                    suffix: vec![ButtonChord::new(suffix, 4)],
                    key: archive_key(state),
                    milestones: MetroidMilestones::default(),
                },
                MetroidSnapshot::for_census_tests(state),
            )
            .expect("insert")
    }

    #[test]
    fn a_stocked_energy_arrival_is_held_beside_the_missile_holder() {
        for reversed in [false, true] {
            let mut archive = MetroidArchive::new(chord_time);
            let root = held(&mut archive, None, 0, 0x00, tourian(10, 8, 300, 20, 0)).expect("root");
            let mut states = [tourian(10, 11, 534, 39, 0), tourian(10, 11, 834, 9, 0)];
            if reversed {
                states.reverse();
            }
            let first = held(&mut archive, Some(root), 1, 0x10, states[0]).expect("first");
            let second = held(&mut archive, Some(root), 2, 0x20, states[1]).expect("second");
            assert!(archive.active[first], "reversed {reversed}");
            assert!(archive.active[second], "reversed {reversed}");
        }
    }

    #[test]
    fn a_fired_missile_that_hit_a_column_is_held_beside_the_unfired_state() {
        let mut archive = MetroidArchive::new(chord_time);
        let unfired =
            held(&mut archive, None, 0, 0x00, tourian(4, 11, 266, 7, 32)).expect("unfired");
        let fired = held(
            &mut archive,
            Some(unfired),
            1,
            0x20,
            tourian(4, 11, 266, 6, 28),
        )
        .expect("fired");
        assert_eq!(archive.entry_key(fired).expect("fired key").boss_damage, 1);
        assert_eq!(
            archive.entry_key(unfired).expect("unfired key").boss_damage,
            0
        );
        assert!(archive.active[fired]);
        assert!(archive.active[unfired]);
    }

    #[test]
    fn a_missile_that_hit_a_zebetite_is_held_beside_the_unfired_state() {
        let mut archive = MetroidArchive::new(chord_time);
        let mut before = tourian(4, 11, 266, 7, 0);
        before.zebetite_hits_left = 8;
        let unfired = held(&mut archive, None, 0, 0x00, before).expect("unfired");
        let mut after = tourian(4, 11, 266, 6, 0);
        after.zebetite_hits_left = 7;
        let fired = held(&mut archive, Some(unfired), 1, 0x20, after).expect("fired");
        assert!(archive.active[fired]);
        assert!(archive.active[unfired]);
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
        assert_eq!(five_tanks.progress(), one_tank.progress());
        assert_eq!(five_tanks.place(), one_tank.place());
    }

    #[test]
    fn the_key_round_trips_through_json_and_postcard() {
        let key = MetroidArchiveKey {
            items: 3,
            tanks: 2,
            boss_damage: 7,
            boss_health: 60,
            boss_health_seen: 96,
            area: 0x11,
            map_x: 8,
            map_y: 29,
            x: 12,
            y: 9,
            posture: 1,
            door: 2,
            columns: 4,
            health: 299,
            missiles: 25,
        };
        let json: MetroidArchiveKey =
            serde_json::from_str(&serde_json::to_string(&key).unwrap()).unwrap();
        assert_eq!(json, key);
        let postcard: MetroidArchiveKey =
            postcard::from_bytes(&postcard::to_allocvec(&key).unwrap()).unwrap();
        assert_eq!(postcard, key);
    }
}
