// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
            SelectorPolicy, entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
    thwaite::target::{
        ButtonChord, ThwaiteInput, ThwaiteMechanicalState, ThwaiteObservations, ThwaiteSnapshot,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

pub const MAX_THWAITE_ACTIONS: usize = 8_192;
pub const KEY_POLICY_IDENTIFIER: &str = "thwaite_town_defence_wave_phase_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_aim_or_sweep_v1";

pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        ThwaiteArchiveKey::groups().saturating_sub(2),
    )
}

pub type ThwaiteArchive =
    Archive<ButtonChord, ThwaiteArchiveKey, ThwaiteMilestones, ThwaiteSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ThwaiteArchiveGroup {
    level_index: u8,
    buildings_standing: u8,
    enemy_missiles_left: u8,
    enemy_missiles_in_flight: u8,
    crosshair_x: u8,
    crosshair_y: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteArchiveKey {
    pub perfect_levels: u16,
    pub levels_cleared: u16,
    pub wave_progress: u8,
    pub buildings_standing: u8,
    pub score: u16,
    pub level_index: u8,
    pub enemy_missiles_left: u8,
    pub enemy_missiles_in_flight: u8,
    pub silo_missiles: u8,
    pub crosshair_x: u8,
    pub crosshair_y: u8,
}

impl Ord for ThwaiteArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.progress_key()
            .cmp(&other.progress_key())
            .then_with(|| self.identity_key().cmp(&other.identity_key()))
    }
}

impl PartialOrd for ThwaiteArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ArchiveKey for ThwaiteArchiveKey {
    type Group = ThwaiteArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn group(self, depth: usize) -> Self::Group {
        let town = ThwaiteArchiveGroup {
            level_index: self.level_index,
            buildings_standing: self.buildings_standing,
            enemy_missiles_left: self.enemy_missiles_left,
            enemy_missiles_in_flight: self.enemy_missiles_in_flight,
            crosshair_x: self.crosshair_x,
            crosshair_y: self.crosshair_y,
        };
        match depth {
            0 => town,
            1 => ThwaiteArchiveGroup {
                crosshair_x: self.crosshair_x.div_euclid(2),
                crosshair_y: self.crosshair_y.div_euclid(2),
                ..town
            },
            2 => ThwaiteArchiveGroup {
                crosshair_x: 0,
                crosshair_y: 0,
                ..town
            },
            3 => ThwaiteArchiveGroup {
                level_index: self.level_index,
                buildings_standing: self.buildings_standing,
                ..ThwaiteArchiveGroup::default()
            },
            _ => ThwaiteArchiveGroup {
                level_index: self.level_index,
                ..ThwaiteArchiveGroup::default()
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ThwaiteChampionKey {
    progress: (u16, u16, u8, u8, u16),
    preference: (u16, u16, u8, u8, u16, u8),
}

impl ThwaiteArchiveKey {
    pub(super) fn champion_key(self) -> ThwaiteChampionKey {
        ThwaiteChampionKey {
            progress: self.progress_key(),
            preference: self.preference(),
        }
    }

    fn progress_key(self) -> (u16, u16, u8, u8, u16) {
        (
            self.perfect_levels,
            self.levels_cleared,
            self.wave_progress,
            self.buildings_standing,
            self.score,
        )
    }

    fn identity_key(self) -> (u8, u8, u8, u8, u8, u8) {
        (
            self.level_index,
            self.enemy_missiles_left,
            self.enemy_missiles_in_flight,
            self.silo_missiles,
            self.crosshair_x,
            self.crosshair_y,
        )
    }

    fn preference(self) -> (u16, u16, u8, u8, u16, u8) {
        (
            self.perfect_levels,
            self.levels_cleared,
            self.buildings_standing,
            self.wave_progress,
            self.score,
            self.silo_missiles,
        )
    }
}

#[must_use]
pub fn archive_key(
    state: ThwaiteMechanicalState,
    evidence: crate::thwaite::target::ThwaiteDefenceEvidence,
) -> Option<ThwaiteArchiveKey> {
    let town = state.town?;
    Some(ThwaiteArchiveKey {
        perfect_levels: evidence.perfect_levels,
        levels_cleared: evidence.levels_cleared,
        wave_progress: u8::MAX.saturating_sub(town.enemy_missiles_left),
        buildings_standing: town.buildings_standing,
        score: state.score,
        level_index: state.level_index(),
        enemy_missiles_left: town.enemy_missiles_left,
        enemy_missiles_in_flight: town.enemy_missiles_in_flight,
        silo_missiles: town.silo_missiles[0].saturating_add(town.silo_missiles[1]),
        crosshair_x: town.crosshair_x.div_euclid(16),
        crosshair_y: town.crosshair_y.div_euclid(16),
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteMilestones {
    pub levels_cleared: u16,
    pub perfect_levels: u16,
    pub buildings_lost: u16,
    pub score: u16,
    pub survived_all_days: bool,
    pub game_over: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteMilestoneTimes {
    pub first_level_cleared: Option<u64>,
    pub first_perfect_level: Option<u64>,
    pub first_game_over: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteMilestoneInputs {
    pub first_level_cleared: Option<ThwaiteInput>,
    pub first_perfect_level: Option<ThwaiteInput>,
    pub first_game_over: Option<ThwaiteInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ThwaiteProgressWatermark {
    pub perfect_levels: u16,
    pub levels_cleared: u16,
    pub score: u16,
}

pub type ThwaiteArchiveProgressPoint = ProgressPoint<ThwaiteMilestones, ThwaiteProgressWatermark>;
pub type ThwaiteArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, ThwaiteArchiveKey, ThwaiteMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: ThwaiteMilestones,
    pub progress_watermark: ThwaiteProgressWatermark,
    pub first_reached: ThwaiteMilestoneTimes,
    pub first_inputs: ThwaiteMilestoneInputs,
    pub champion_input: ThwaiteInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<ThwaiteArchiveEntryReport>,
    pub progress_curve: Vec<ThwaiteArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones_from_observation(observation: &ThwaiteObservations) -> ThwaiteMilestones {
    ThwaiteMilestones {
        levels_cleared: observation.evidence.levels_cleared,
        perfect_levels: observation.evidence.perfect_levels,
        buildings_lost: observation.evidence.buildings_lost,
        score: observation.decoded.score,
        survived_all_days: observation.decoded.survived_all_days(),
        game_over: observation.decoded.game_over(),
    }
}

pub fn merge_milestones(into: &mut ThwaiteMilestones, from: ThwaiteMilestones) {
    into.levels_cleared = into.levels_cleared.max(from.levels_cleared);
    into.perfect_levels = into.perfect_levels.max(from.perfect_levels);
    into.buildings_lost = into.buildings_lost.max(from.buildings_lost);
    into.score = into.score.max(from.score);
    into.survived_all_days |= from.survived_all_days;
    into.game_over |= from.game_over;
}

#[must_use]
pub fn milestone_key(value: ThwaiteMilestones) -> (u16, u16, u16, bool) {
    (
        value.perfect_levels,
        value.levels_cleared,
        value.score,
        value.survived_all_days,
    )
}

pub fn merge_progress_watermark(
    watermark: &mut ThwaiteProgressWatermark,
    observations: &[ThwaiteObservations],
) {
    for observation in observations {
        watermark.perfect_levels = watermark
            .perfect_levels
            .max(observation.evidence.perfect_levels);
        watermark.levels_cleared = watermark
            .levels_cleared
            .max(observation.evidence.levels_cleared);
        watermark.score = watermark.score.max(observation.decoded.score);
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x90, 0x50, 0xa0, 0x60];
const AB: [u8; 4] = [0, 0x02, 0x01, 0x03];

pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    let direction = DIRECTIONS[rand
        .below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty Thwaite direction vocabulary")?)];
    let buttons = direction
        | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty Thwaite A/B vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(5).ok_or("invalid duration odds")?) < 3 {
        u8::try_from(1 + rand.below(NonZeroUsize::new(8).ok_or("invalid aim duration")?))?
    } else {
        u8::try_from(9 + rand.below(NonZeroUsize::new(40).ok_or("invalid sweep duration")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thwaite::target::{
        NUM_BUILDINGS, STATE_ACTIVE, ThwaiteDefenceEvidence, ThwaiteTownState,
    };

    fn state(standing: u8, score: u16, crosshair_x: u8) -> ThwaiteMechanicalState {
        ThwaiteMechanicalState {
            game_state: STATE_ACTIVE,
            num_players: 1,
            score,
            town: Some(ThwaiteTownState {
                buildings: [1; NUM_BUILDINGS],
                buildings_standing: standing,
                silos_standing: 2,
                silo_missiles: [15, 15],
                enemy_missiles_left: 10,
                enemy_missiles_in_flight: 2,
                buildings_destroyed_this_level: 0,
                crosshair_x,
                crosshair_y: 128,
            }),
            ..ThwaiteMechanicalState::default()
        }
    }

    fn evidence(perfect: u16, cleared: u16) -> ThwaiteDefenceEvidence {
        ThwaiteDefenceEvidence {
            buildings_lost: 0,
            levels_cleared: cleared,
            perfect_levels: perfect,
        }
    }

    #[test]
    fn crosshair_groups_coarsen_with_depth() {
        let key = archive_key(state(12, 0, 200), evidence(0, 0)).expect("live key");
        assert_eq!(key.group(0).crosshair_x, 200 / 16);
        assert_eq!(key.group(1).crosshair_x, 200 / 16 / 2);
        assert_eq!(key.group(2).crosshair_x, 0);
        assert_eq!(key.group(3).enemy_missiles_left, 0);
        assert_eq!(key.group(4).buildings_standing, 0);
    }

    #[test]
    fn town_identity_is_separate_from_same_town_preference() {
        let weak = archive_key(state(12, 0, 64), evidence(0, 0)).expect("live key");
        let strong = archive_key(state(12, 400, 64), evidence(0, 0)).expect("live key");
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(ThwaiteArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn defended_hours_outrank_a_richer_ruined_town() {
        let ruined = archive_key(state(4, 900, 64), evidence(0, 3)).expect("live key");
        let defended = archive_key(state(12, 100, 64), evidence(1, 1)).expect("live key");
        assert!(defended > ruined);
    }

    #[test]
    fn terminal_state_has_no_town_payload() {
        let mut state = state(0, 100, 64);
        state.game_state = crate::thwaite::target::STATE_GAMEOVER;
        state.town = None;
        assert_eq!(archive_key(state, evidence(0, 1)), None);
    }

    #[test]
    fn vocabulary_covers_both_silos_without_menu_buttons() {
        let mut rand = RomuDuoJrRand::with_seed(11);
        let mut saw_a = false;
        let mut saw_b = false;
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(chord.hold_frames >= 1 && chord.hold_frames <= 48);
            saw_a |= chord.buttons & 0x01 != 0;
            saw_b |= chord.buttons & 0x02 != 0;
        }
        assert!(saw_a && saw_b);
    }
}
