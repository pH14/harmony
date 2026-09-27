// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    mm2::target::{
        BOSS_DAMAGE_BUCKET, BOSS_PHASE_DEFEATED, ButtonChord, ENEMY_DAMAGE_BUCKET, MENU_CLOSED,
        Mm2Input, Mm2MechanicalState, Mm2Observations, Mm2Scene, Mm2Snapshot,
        WILY5_REFIGHTS_COMPLETE, WILY5_STAGE, preference_tuple,
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
pub const KEY_POLICY_IDENTIFIER: &str =
    "mm2_whole_game_inventory_encounters_tiers_spatial_32_resources_preference_v21";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type Mm2Archive = Archive<ButtonChord, Mm2ArchiveKey, Mm2Milestones, Mm2Snapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2Progress {
    pub ending: bool,
    pub bosses: u8,
    pub castle_clears: u8,
    pub refights: u8,
    pub machine_shell: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveKey {
    pub bosses: u8,
    pub castle_clears: u8,
    pub capabilities: u8,
    pub refighting_mask: u8,
    pub refight_boss: u8,
    pub boobeam_targets: u16,
    pub machine_shell: bool,
    pub ending: bool,
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
    type Place = (
        u8,
        u8,
        u8,
        u8,
        u8,
        u8,
        u8,
        u8,
        u8,
        bool,
        (u8, u8, u8, u16, bool),
    );
    type Progress = Mm2Progress;
    type Identity = (u8, u8, u8, u8);

    fn place(self) -> Self::Place {
        (
            self.stage,
            self.screen,
            self.room,
            self.boss_damage,
            self.enemy_damage,
            self.x / 2,
            self.y / 2,
            self.posture,
            self.platforms,
            self.menu != MENU_CLOSED,
            (
                self.capabilities,
                self.refighting_mask,
                self.refight_boss,
                self.boobeam_targets,
                self.machine_shell,
            ),
        )
    }

    fn progress(self) -> Self::Progress {
        Mm2Progress {
            ending: self.ending,
            bosses: self.bosses,
            castle_clears: self.castle_clears,
            refights: self.refighting_mask.count_ones() as u8,
            machine_shell: self.machine_shell,
        }
    }

    fn identity(self) -> Self::Identity {
        (self.x, self.y, self.weapon, self.menu)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
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
        castle_clears: state.castle_clears,
        capabilities: state.weapons_obtained,
        refighting_mask: state.refighting_mask,
        refight_boss: state.refight_boss,
        boobeam_targets: state.boobeam_targets,
        machine_shell: state.wily_machine_shell_broken,
        ending: state.scene == Mm2Scene::Ending,
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
    pub robot_masters: u8,
    pub castle_clears: u8,
    pub ending: bool,
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
    pub whole_game: Mm2Progress,
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
    let boss_defeated = state.boss_phase >= BOSS_PHASE_DEFEATED;
    let defeated_boss = state.weapons_obtained & !genesis_weapons != 0
        || (boss_defeated
            && (state.stage != WILY5_STAGE || state.refighting_mask == WILY5_REFIGHTS_COMPLETE));
    Mm2Milestones {
        robot_masters: state.weapons_obtained,
        castle_clears: state.castle_clears,
        ending: state.scene == Mm2Scene::Ending,
        max_screen: state.screen,
        reached_boss: defeated_boss || state.boss_fight_underway() || boss_defeated,
        defeated_boss,
    }
}

pub fn merge_milestones(into: &mut Mm2Milestones, from: Mm2Milestones) {
    into.robot_masters |= from.robot_masters;
    into.castle_clears = into.castle_clears.max(from.castle_clears);
    into.ending |= from.ending;
    into.max_screen = into.max_screen.max(from.max_screen);
    into.reached_boss |= from.reached_boss;
    into.defeated_boss |= from.defeated_boss;
}

#[must_use]
pub fn milestone_key(value: Mm2Milestones) -> (bool, u32, u8, bool, bool, u8) {
    (
        value.ending,
        value.robot_masters.count_ones(),
        value.castle_clears,
        value.defeated_boss,
        value.reached_boss,
        value.max_screen,
    )
}

#[must_use]
pub fn progress_watermark(state: Mm2MechanicalState) -> Mm2ProgressWatermark {
    Mm2ProgressWatermark {
        whole_game: archive_key(state).progress(),
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
            player_state: 3,
            weapons_obtained: weapons,
            ..Mm2MechanicalState::default()
        }
    }

    #[test]
    fn whole_game_encounter_masks_preserve_distinct_states_and_order_progress() {
        let mut first = state(100, 28, 255);
        first.stage = 12;
        first.castle_clears = 4;
        first.refighting_mask = 1;
        let mut other = first;
        other.refighting_mask = 2;
        assert_eq!(archive_key(first).progress(), archive_key(other).progress());
        assert_ne!(archive_key(first).place(), archive_key(other).place());
        other.refighting_mask = 3;
        assert!(archive_key(other).progress() > archive_key(first).progress());
        first.refighting_mask = 255;
        other = first;
        other.wily_machine_shell_broken = true;
        assert!(archive_key(other).progress() > archive_key(first).progress());
        first.stage = 11;
        first.boobeam_targets = 3;
        other = first;
        other.boobeam_targets = 2;
        assert_ne!(archive_key(first).place(), archive_key(other).place());
    }

    #[test]
    fn whole_game_inventory_sets_need_distinct_places() {
        let metal = archive_key(state(100, 28, 0x40));
        let air = archive_key(state(100, 28, 0x02));
        assert_eq!(metal.progress(), air.progress());
        assert_ne!(metal.place(), air.place());
    }

    #[test]
    fn whole_game_castle_clear_must_advance_the_tier() {
        let mut before = state(100, 28, 255);
        before.stage = 12;
        before.castle_clears = 4;
        before.refighting_mask = 255;
        before.wily_machine_shell_broken = true;
        let after = Mm2MechanicalState {
            stage: 13,
            castle_clears: 5,
            refighting_mask: 0,
            wily_machine_shell_broken: false,
            ..before
        };
        assert!(archive_key(after).progress() > archive_key(before).progress());
    }

    #[test]
    fn wily5_milestones_require_all_refights_for_stage_clear() {
        let mut intermediate = state(100, 28, 0);
        intermediate.stage = 12;
        intermediate.boss_phase = BOSS_PHASE_DEFEATED;
        intermediate.refighting_mask = 0x7f;
        let value = milestones(intermediate, 0);
        assert!(value.reached_boss);
        assert!(!value.defeated_boss);

        intermediate.refighting_mask = WILY5_REFIGHTS_COMPLETE;
        let complete = milestones(intermediate, 0);
        assert!(complete.reached_boss);
        assert!(complete.defeated_boss);
    }

    #[test]
    fn stale_boss_health_after_continue_does_not_report_an_encounter() {
        let mut restarted = state(128, 28, 255);
        restarted.stage = 11;
        restarted.screen = 22;
        restarted.room = 22;
        restarted.boss_health = 28;
        assert!(!milestones(restarted, 255).reached_boss);
        restarted.boss_phase = 2;
        assert!(milestones(restarted, 255).reached_boss);
        restarted.boss_phase = BOSS_PHASE_DEFEATED;
        restarted.boss_health = 0;
        assert!(milestones(restarted, 255).reached_boss);
    }

    #[test]
    fn a_wily_boss_that_grants_no_weapon_still_reports_as_defeated() {
        let genesis = 0x40;
        let mut cleared = state(100, 28, genesis);
        cleared.boss_phase = BOSS_PHASE_DEFEATED;
        assert!(milestones(cleared, genesis).defeated_boss);

        let mut alive = state(100, 28, genesis);
        alive.boss_phase = BOSS_PHASE_DEFEATED - 1;
        assert!(!milestones(alive, genesis).defeated_boss);

        assert!(milestones(state(100, 28, genesis | 0x01), genesis).defeated_boss);
    }

    #[test]
    fn one_location_uses_resources_only_for_preference() {
        let weak = archive_key(state(100, 4, 0));
        let strong = archive_key(state(100, 20, 0));
        assert_eq!(weak.place(), strong.place());
        assert_eq!(weak.identity(), strong.identity());
        assert_eq!(weak.progress(), strong.progress());
        assert_eq!(strong.preference_cmp(0, weak), Ordering::Greater);
        assert_eq!(Mm2ArchiveKey::capacity(), 1);
    }

    #[test]
    fn screens_are_separate_places_under_one_tier() {
        let first = archive_key(state(100, 28, 0));
        let mut next_state = state(100, 28, 0);
        next_state.screen = 3;
        let next = archive_key(next_state);
        assert_ne!(first.place(), next.place());
        assert_eq!(first.progress(), next.progress());
    }

    #[test]
    fn the_tier_is_the_bosses_cleared_and_boss_damage_is_a_place() {
        let first = Mm2ArchiveKey {
            stage: 1,
            screen: 10,
            room: 4,
            bosses: 2,
            boss_damage: 1,
            ..Default::default()
        };
        let elsewhere = Mm2ArchiveKey {
            stage: 12,
            screen: 99,
            room: 77,
            ..first
        };
        assert_eq!(first.progress(), elsewhere.progress());
        let hurt = Mm2ArchiveKey {
            boss_damage: 2,
            ..first
        };
        assert_eq!(hurt.progress(), first.progress());
        assert_ne!(hurt.place(), first.place());
        assert!(Mm2ArchiveKey { bosses: 3, ..first }.progress() > first.progress());
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
