// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    donkey_kong::target::{
        ButtonChord, DonkeyKongInput, DonkeyKongMechanicalState, DonkeyKongObservations,
        DonkeyKongSnapshot, GRID_SHIFT, STAGE_RIVETS, TRACKED_STAGES,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "donkey_kong_stage_rivet_height_tier_stable_stage_grid_16_status_place_bonus_lives_score_preference_v2";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

const HEIGHT_SHIFT: u8 = 3;

fn height_tier(state: DonkeyKongMechanicalState) -> u8 {
    if state.stage == STAGE_RIVETS {
        0
    } else {
        state.height() >> HEIGHT_SHIFT
    }
}

pub type DonkeyKongArchive =
    Archive<ButtonChord, DonkeyKongArchiveKey, DonkeyKongMilestones, DonkeyKongSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DonkeyKongTier {
    pub stages: u8,
    pub rivets: u8,
    pub height: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DonkeyKongArchiveKey {
    pub tier: DonkeyKongTier,
    pub stage: u8,
    pub column: u8,
    pub row: u8,
    pub status: u8,
    pub bonus: u8,
    pub lives: u8,
    pub score: u32,
}

impl ArchiveKey for DonkeyKongArchiveKey {
    type Place = (u8, u8, u8, u8, u8);
    type Progress = DonkeyKongTier;
    type Identity = ();

    fn place(self) -> Self::Place {
        (
            self.stage,
            self.tier.stages,
            self.column,
            self.row,
            self.status,
        )
    }

    fn progress(self) -> Self::Progress {
        self.tier
    }

    fn identity(self) -> Self::Identity {}

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        (self.bonus, self.lives, self.score).cmp(&(other.bonus, other.lives, other.score))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn tier(state: DonkeyKongMechanicalState) -> DonkeyKongTier {
    DonkeyKongTier {
        stages: state.stages_cleared(),
        rivets: state.rivets,
        height: height_tier(state),
    }
}

#[must_use]
pub fn archive_key(state: DonkeyKongMechanicalState) -> Option<DonkeyKongArchiveKey> {
    if state.terminal() || state.in_transition() {
        return None;
    }
    Some(DonkeyKongArchiveKey {
        tier: tier(state),
        stage: state.stage,
        column: state.x >> GRID_SHIFT,
        row: state.y >> GRID_SHIFT,
        status: state.status,
        bonus: state.bonus,
        lives: state.lives,
        score: state.score,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongMilestones {
    pub cleared: u8,
    pub rivets: u8,
    pub height: u8,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongMilestoneTimes {
    pub first_cleared: [Option<u64>; TRACKED_STAGES],
    pub first_victory: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongMilestoneInputs {
    pub first_cleared: [Option<DonkeyKongInput>; TRACKED_STAGES],
    pub first_victory: Option<DonkeyKongInput>,
    pub first_death: Option<DonkeyKongInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DonkeyKongProgressWatermark {
    pub cleared: u8,
    pub rivets: u8,
    pub height: u8,
}

pub type DonkeyKongArchiveProgressPoint =
    ProgressPoint<DonkeyKongMilestones, DonkeyKongProgressWatermark>;
pub type DonkeyKongArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, DonkeyKongArchiveKey, DonkeyKongMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DonkeyKongArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: DonkeyKongMilestones,
    pub progress_watermark: DonkeyKongProgressWatermark,
    pub first_reached: DonkeyKongMilestoneTimes,
    pub first_inputs: DonkeyKongMilestoneInputs,
    pub champion_input: DonkeyKongInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<DonkeyKongArchiveEntryReport>,
    pub progress_curve: Vec<DonkeyKongArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: DonkeyKongMechanicalState) -> DonkeyKongMilestones {
    DonkeyKongMilestones {
        cleared: state.stages_cleared(),
        rivets: state.rivets,
        height: height_tier(state),
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &DonkeyKongObservations) -> DonkeyKongMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut DonkeyKongMilestones, from: DonkeyKongMilestones) {
    let deeper = from.cleared > into.cleared;
    let level = from.cleared == into.cleared;
    if deeper {
        into.cleared = from.cleared;
        into.rivets = from.rivets;
        into.height = from.height;
    } else if level {
        into.rivets = into.rivets.max(from.rivets);
        into.height = into.height.max(from.height);
    }
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: DonkeyKongMilestones) -> (bool, u8, u8, u8, bool) {
    (
        value.victory,
        value.cleared,
        value.rivets,
        value.height,
        !value.death,
    )
}

#[must_use]
pub fn progress_point(state: DonkeyKongMechanicalState) -> DonkeyKongProgressWatermark {
    DonkeyKongProgressWatermark {
        cleared: state.stages_cleared(),
        rivets: state.rivets,
        height: height_tier(state),
    }
}

pub fn merge_progress_watermark(
    watermark: &mut DonkeyKongProgressWatermark,
    observations: &[DonkeyKongObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_point(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &NES_PRESSABLE_BUTTON_MASKS,
    tap: None,
    short_hold: SHORT_HOLD_FRAMES,
    long_hold: LONG_HOLD_FRAMES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::donkey_kong::target::{
        GOAL_ROUNDS, STAGE_BARRELS, STAGE_ELEVATORS, START_LIVES, START_X, START_Y, STATUS_DEAD,
    };
    use crate::search::rand::RomuDuoJrRand;

    fn state(x: u8, y: u8) -> DonkeyKongMechanicalState {
        DonkeyKongMechanicalState {
            stage: STAGE_BARRELS,
            lives: START_LIVES,
            bonus: 0x49,
            status: 1,
            x,
            y,
            ..DonkeyKongMechanicalState::default()
        }
    }

    #[test]
    fn the_place_is_the_stage_a_16_pixel_cell_of_mario_and_his_posture() {
        let here = archive_key(state(START_X, START_Y)).unwrap();
        let near = archive_key(state(START_X + 2, START_Y + 2)).unwrap();
        let right = archive_key(state(START_X + 16, START_Y)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        let mut climbing = state(START_X, START_Y);
        climbing.status = 2;
        assert_ne!(here.place(), archive_key(climbing).unwrap().place());
        let mut elsewhere = state(START_X, START_Y);
        elsewhere.stage = STAGE_ELEVATORS;
        assert_ne!(here.place(), archive_key(elsewhere).unwrap().place());
    }

    #[test]
    fn the_tier_orders_stages_then_rivets_then_height_except_on_the_rivet_stage() {
        let low = archive_key(state(START_X, START_Y)).unwrap();
        let high = archive_key(state(START_X, START_Y - 0x40)).unwrap();
        assert!(high.progress() > low.progress());
        let mut next = state(START_X, START_Y);
        next.stage = STAGE_ELEVATORS;
        assert!(archive_key(next).unwrap().progress() > high.progress());
        let mut rivet = state(START_X, START_Y);
        rivet.stage = STAGE_RIVETS;
        let mut one = rivet;
        one.rivets = 1;
        let mut tall = rivet;
        tall.y = START_Y - 0x60;
        assert!(archive_key(one).unwrap().progress() > archive_key(tall).unwrap().progress());
        assert_eq!(
            archive_key(tall).unwrap().progress(),
            archive_key(rivet).unwrap().progress()
        );
    }

    #[test]
    fn more_bonus_then_lives_then_score_is_preferred() {
        let slow = state(START_X, START_Y);
        let mut fast = slow;
        fast.bonus = 0x50;
        let (slow_key, fast_key) = (archive_key(slow).unwrap(), archive_key(fast).unwrap());
        assert_eq!(slow_key.place(), fast_key.place());
        assert_eq!(fast_key.preference_cmp(0, slow_key), Ordering::Greater);
        let mut rich = slow;
        rich.score = 800;
        assert_eq!(
            archive_key(rich).unwrap().preference_cmp(0, slow_key),
            Ordering::Greater
        );
    }

    #[test]
    fn death_and_intro_frames_have_no_key_but_death_is_a_milestone() {
        let mut dead = state(START_X, START_Y);
        dead.status = STATUS_DEAD;
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
        let mut intro = state(START_X, START_Y);
        intro.y = 0;
        intro.status = 0;
        assert_eq!(archive_key(intro), None);
    }

    #[test]
    fn milestones_keep_the_deepest_stage_with_its_rivets_and_height() {
        let mut aggregate = DonkeyKongMilestones::default();
        let mut first = state(START_X, START_Y - 0x20);
        first.stage = STAGE_ELEVATORS;
        merge_milestones(&mut aggregate, milestones(first));
        let mut second = state(START_X, START_Y - 0x60);
        second.stage = STAGE_BARRELS;
        merge_milestones(&mut aggregate, milestones(second));
        assert_eq!(aggregate.cleared, 1);
        assert_eq!(aggregate.height, 0x20 >> HEIGHT_SHIFT);
        let mut last = state(START_X, START_Y);
        last.round = GOAL_ROUNDS;
        merge_milestones(&mut aggregate, milestones(last));
        assert!(aggregate.victory);
        assert_eq!(aggregate.cleared, TRACKED_STAGES as u8);
    }

    #[test]
    fn vocabulary_draws_pressable_chords_without_start_or_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&chord.buttons));
        }
    }
}
