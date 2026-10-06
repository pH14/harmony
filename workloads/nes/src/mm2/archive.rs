// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{
        ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES, Tap,
    },
    mm2::target::{
        BOSS_DAMAGE_BUCKET, BOSS_PHASE_DEFEATED, ButtonChord, ENEMY_DAMAGE_BUCKET, MENU_CLOSED,
        Mm2Input, Mm2MechanicalState, Mm2Observations, Mm2Snapshot, Mm2Tier, preference_tuple,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "mm2_route_tiers_location_boss_damage_enemy_encounter_spatial_32_posture_platforms_menu_place_weapon_identity_preference_castle_kill_target_grid_v22";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type Mm2Archive = Archive<ButtonChord, Mm2ArchiveKey, Mm2Milestones, Mm2Snapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveKey {
    pub tier: Mm2Tier,
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
    pub refights: u8,
    pub refight_boss: u8,
    pub boobeam_targets: u64,
}

impl ArchiveKey for Mm2ArchiveKey {
    type Place = (
        (u8, u8, u8, u8, u8, u8, u8, u8, u8, bool),
        (u8, u8, u64),
    );
    type Progress = Mm2Tier;
    type Identity = (u8, u8, u8, u8);

    fn place(self) -> Self::Place {
        (
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
            ),
            (self.refights, self.refight_boss, self.boobeam_targets),
        )
    }

    fn progress(self) -> Self::Progress {
        self.tier
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
    fn preference(self) -> (Mm2Tier, u8, u16) {
        (self.tier, self.health, self.energy)
    }
}

#[must_use]
pub fn archive_key(state: Mm2MechanicalState) -> Mm2ArchiveKey {
    let (tier, health, energy) = preference_tuple(state);
    Mm2ArchiveKey {
        tier,
        health,
        energy,
        refights: state.refights,
        refight_boss: state.refight_boss(),
        boobeam_targets: state.boobeam_targets,
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
    pub tier: Mm2Tier,
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
        reached_boss: state.boss_fight_underway() && state.boss_health != 0,
        defeated_boss: state.weapons_obtained & !genesis_weapons != 0
            || state.boss_phase >= BOSS_PHASE_DEFEATED,
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
        tier: state.tier(),
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

const START: u8 = 0x08;

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &NES_PRESSABLE_BUTTON_MASKS,
    tap: Some(Tap {
        buttons: START,
        odds: 12,
        hold_frames: (2, 7),
    }),
    short_hold: SHORT_HOLD_FRAMES,
    long_hold: LONG_HOLD_FRAMES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rand::RomuDuoJrRand;

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
        let strong = archive_key(state(100, 20, 0x40));
        assert_eq!(weak.place(), strong.place());
        assert_eq!(weak.identity(), strong.identity());
        assert_ne!(weak.progress(), strong.progress());
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
            tier: Mm2Tier {
                robot_masters: 2,
                ..Mm2Tier::default()
            },
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
        let more = Mm2Tier {
            robot_masters: 3,
            ..first.tier
        };
        assert!(Mm2ArchiveKey { tier: more, ..first }.progress() > first.progress());
    }

    #[test]
    fn a_castle_boss_kill_rises_one_tier_and_a_refight_kill_does_not() {
        let mut fight = state(100, 28, u8::MAX);
        fight.stage = 11;
        fight.boss_phase = 2;
        let fighting = archive_key(fight).progress();
        fight.boss_phase = BOSS_PHASE_DEFEATED;
        let defeated = archive_key(fight).progress();
        assert!(defeated.castle_boss_defeated && defeated > fighting);
        fight.stage = 12;
        fight.refights = 0x01;
        fight.current_boss = 1;
        assert!(!archive_key(fight).progress().castle_boss_defeated);
    }

    #[test]
    fn each_castle_clear_refight_and_shell_break_rises_one_tier() {
        let mut wily = state(100, 28, u8::MAX);
        wily.stage = 8;
        let first_castle = archive_key(wily).progress();
        assert!(first_castle > archive_key(state(100, 28, 0x7f)).progress());
        wily.stage = 12;
        let hub = archive_key(wily).progress();
        assert!(hub > first_castle);
        wily.refights = 0x01;
        let refought = archive_key(wily).progress();
        assert!(refought > hub);
        wily.refights = u8::MAX;
        wily.current_boss = 12;
        wily.boss_phase = 5;
        let shell = archive_key(wily).progress();
        assert!(shell.machine_shell && shell > refought);
        wily.boss_phase = BOSS_PHASE_DEFEATED;
        let machine_defeated = archive_key(wily).progress();
        assert!(machine_defeated.castle_boss_defeated && machine_defeated > shell);
        wily.stage = 13;
        wily.refights = 0;
        wily.boss_phase = 0;
        assert!(archive_key(wily).progress() > shell);
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
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS
                .draw(&mut rand, previous.as_ref())
                .expect("draw chord");
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x04, 0);
            if chord.buttons & START != 0 {
                assert!(chord.hold_frames <= 7);
                starts += 1;
            }
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
