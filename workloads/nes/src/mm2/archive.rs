// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    mm2::target::{
        BOSS_DAMAGE_BUCKET, ButtonChord, ENEMY_DAMAGE_BUCKET, MENU_CLOSED, Mm2Input,
        Mm2MechanicalState, Mm2Observations, Mm2Snapshot, preference_tuple,
    },
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
            SelectorPolicy, entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

pub const MAX_MM2_ACTIONS: usize = 8_192;
pub const KEY_POLICY_IDENTIFIER: &str =
    "mm2_location_boss_enemy_spatial_16_posture_weapon_menu_energy_platforms_preference_v18";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        Mm2ArchiveKey::groups().saturating_sub(2),
    )
}

pub type Mm2Archive = Archive<ButtonChord, Mm2ArchiveKey, Mm2Milestones, Mm2Snapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveGroup {
    bosses: u8,
    stage: u8,
    screen: u8,
    room: u8,
    boss_damage: u8,
    enemy_damage: u8,
    x: u8,
    y: u8,
    posture: u8,
    weapon: u8,
    platforms: u8,
    menu: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveKey {
    pub bosses: u8,
    pub stage: u8,
    pub screen: u8,
    pub room: u8,
    pub boss_damage: u8,
    pub enemy_damage: u8,
    pub x: u8,
    pub y: u8,
    pub posture: u8,
    pub weapon: u8,
    pub platforms: u8,
    pub menu: u8,
    pub health: u8,
    pub energy: u16,
}

impl ArchiveKey for Mm2ArchiveKey {
    type Group = Mm2ArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn group(self, depth: usize) -> Self::Group {
        let location = Mm2ArchiveGroup {
            stage: self.stage,
            screen: self.screen,
            room: self.room,
            boss_damage: self.boss_damage,
            enemy_damage: self.enemy_damage,
            x: self.x,
            y: self.y,
            posture: self.posture,
            weapon: self.weapon,
            platforms: self.platforms,
            menu: self.menu,
            ..Mm2ArchiveGroup::default()
        };
        match depth {
            0 => location,
            1 => Mm2ArchiveGroup {
                x: self.x / 2,
                y: self.y / 2,
                weapon: 0,
                menu: u8::from(self.menu != MENU_CLOSED),
                ..location
            },
            2 => Mm2ArchiveGroup {
                bosses: self.bosses,
                x: self.x / 8,
                y: self.y / 8,
                posture: 0,
                weapon: 0,
                platforms: 0,
                menu: 0,
                ..location
            },
            3 => Mm2ArchiveGroup {
                bosses: self.bosses,
                stage: self.stage,
                screen: self.screen,
                ..Mm2ArchiveGroup::default()
            },
            _ => Mm2ArchiveGroup {
                bosses: self.bosses,
                stage: self.stage,
                ..Mm2ArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn progress_cmp(left: Self::Group, right: Self::Group) -> Ordering {
        (left.bosses, left.boss_damage).cmp(&(right.bosses, right.boss_damage))
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

impl Mm2ArchiveKey {
    fn preference(self) -> (u8, u8, u16) {
        (self.bosses, self.health, self.energy)
    }
}

#[must_use]
pub fn archive_key(state: Mm2MechanicalState) -> Mm2ArchiveKey {
    let (bosses, health, energy) = preference_tuple(state);
    Mm2ArchiveKey {
        bosses,
        health,
        energy,
        stage: state.stage,
        screen: state.screen,
        room: state.room,
        boss_damage: state.boss_damage() / BOSS_DAMAGE_BUCKET,
        enemy_damage: state.enemy_damage / ENEMY_DAMAGE_BUCKET,
        x: state.x / 16,
        y: state.y / 16,
        posture: state.posture(),
        weapon: state.weapon,
        platforms: state.platforms,
        menu: state.menu,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Milestones {
    pub max_screen: u8,
    pub reached_boss: bool,
    pub defeated_boss: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2MilestoneTimes {
    pub first_new_screen: Option<u64>,
    pub first_boss: Option<u64>,
    pub first_clear: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2MilestoneInputs {
    pub first_new_screen: Option<Mm2Input>,
    pub first_boss: Option<Mm2Input>,
    pub first_clear: Option<Mm2Input>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ProgressWatermark {
    pub bosses: u8,
    pub stage: u8,
    pub screen: u8,
    pub room: u8,
    pub boss_damage: u8,
    pub enemy_damage: u8,
    pub x: u8,
    pub y: u8,
}

pub type Mm2ArchiveProgressPoint = ProgressPoint<Mm2Milestones, Mm2ProgressWatermark>;
pub type Mm2ArchiveEntryReport = ArchiveEntryReport<ButtonChord, Mm2ArchiveKey, Mm2Milestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2ArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: Mm2Milestones,
    pub progress_watermark: Mm2ProgressWatermark,
    pub first_reached: Mm2MilestoneTimes,
    pub first_inputs: Mm2MilestoneInputs,
    pub champion_input: Mm2Input,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<Mm2ArchiveEntryReport>,
    pub progress_curve: Vec<Mm2ArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: Mm2MechanicalState, genesis_weapons: u8) -> Mm2Milestones {
    Mm2Milestones {
        max_screen: state.screen,
        reached_boss: state.boss_health != 0,
        defeated_boss: state.weapons_obtained & !genesis_weapons != 0,
    }
}

pub fn merge_milestones(into: &mut Mm2Milestones, from: Mm2Milestones) {
    into.max_screen = into.max_screen.max(from.max_screen);
    into.reached_boss |= from.reached_boss;
    into.defeated_boss |= from.defeated_boss;
}

#[must_use]
pub fn milestone_key(value: Mm2Milestones) -> (bool, bool, u8) {
    (value.defeated_boss, value.reached_boss, value.max_screen)
}

#[must_use]
pub fn progress_watermark(state: Mm2MechanicalState) -> Mm2ProgressWatermark {
    Mm2ProgressWatermark {
        bosses: state.bosses_beaten(),
        stage: state.stage,
        screen: state.screen,
        room: state.room,
        boss_damage: state.boss_damage(),
        enemy_damage: state.enemy_damage,
        x: state.x,
        y: state.y,
    }
}

pub fn merge_progress_watermark(
    watermark: &mut Mm2ProgressWatermark,
    observations: &[Mm2Observations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_watermark(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];
const START: u8 = 0x08;
const START_ODDS: usize = 12;

pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    if rand.below(NonZeroUsize::new(START_ODDS).ok_or("invalid start odds")?) == 0 {
        let hold = u8::try_from(2 + rand.below(NonZeroUsize::new(6).ok_or("invalid tap")?))?;
        return Ok(ButtonChord::new(START, hold));
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

    fn state(x: u8, health: u8, weapons: u8) -> Mm2MechanicalState {
        Mm2MechanicalState {
            stage: 6,
            screen: 2,
            x,
            y: 0xb4,
            health,
            lives: 2,
            weapons_obtained: weapons,
            ..Mm2MechanicalState::default()
        }
    }

    #[test]
    fn one_location_uses_resources_only_for_preference() {
        let weak = archive_key(state(100, 4, 0));
        let strong = archive_key(state(100, 20, 0x40));
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(weak.group(1), strong.group(1));
        assert_ne!(weak.group(2), strong.group(2));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(Mm2ArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn screens_are_separate_groups_below_the_stage() {
        let first = archive_key(state(100, 28, 0));
        let mut next_state = state(100, 28, 0);
        next_state.screen = 3;
        let next = archive_key(next_state);
        assert_ne!(first.group(3), next.group(3));
        assert_eq!(first.group(4), next.group(4));
    }

    #[test]
    fn progress_ignores_stage_and_location_labels() {
        let first = Mm2ArchiveGroup {
            stage: 1,
            screen: 10,
            room: 4,
            bosses: 2,
            boss_damage: 1,
            ..Default::default()
        };
        let elsewhere = Mm2ArchiveGroup {
            stage: 12,
            screen: 99,
            room: 77,
            ..first
        };
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(first, elsewhere),
            Ordering::Equal
        );
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(
                Mm2ArchiveGroup {
                    boss_damage: 2,
                    ..first
                },
                elsewhere
            ),
            Ordering::Greater
        );
    }

    #[test]
    fn milestones_are_relative_to_genesis_weapons() {
        let value = milestones(state(0, 28, 0x40), 0x40);
        assert!(!value.defeated_boss);
        let value = milestones(state(0, 28, 0x44), 0x40);
        assert!(value.defeated_boss);
    }

    #[test]
    fn wandering_does_not_change_the_same_endpoint_key() {
        let first = archive_key(state(0x40, 28, 0));
        let mut elsewhere = first;
        elsewhere.room = 99;
        assert_eq!(first.complete(Some((elsewhere, &()))), first.complete(None));
    }

    #[test]
    fn vocabulary_draws_bare_start_taps_and_never_select_or_conflicting_directions() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x04, 0);
            if chord.buttons & START != 0 {
                assert_eq!(chord.buttons, START);
                assert!(chord.hold_frames <= 8);
                starts += 1;
            }
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
