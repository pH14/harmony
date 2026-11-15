// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::ChordVocabulary,
    pac_man::target::{
        ButtonChord, DOWN, GRID_SHIFT, LEFT, MAZES, PacManInput, PacManMechanicalState,
        PacManObservations, PacManSnapshot, RIGHT, UP,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str =
    "pac_man_maze_dots_tier_grid_8_power_place_lives_score_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type PacManArchive = Archive<ButtonChord, PacManArchiveKey, PacManMilestones, PacManSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PacManTier {
    pub mazes: u8,
    pub dots: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PacManArchiveKey {
    pub tier: PacManTier,
    pub column: u8,
    pub row: u8,
    pub powered: bool,
    pub lives: u8,
    pub score: u32,
}

impl ArchiveKey for PacManArchiveKey {
    type Place = (u8, u8, bool);
    type Progress = PacManTier;
    type Identity = ();

    fn place(self) -> Self::Place {
        (self.column, self.row, self.powered)
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
        (self.lives, self.score).cmp(&(other.lives, other.score))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn tier(state: PacManMechanicalState) -> PacManTier {
    PacManTier {
        mazes: state.mazes_cleared(),
        dots: state.dots_eaten(),
    }
}

#[must_use]
pub fn archive_key(state: PacManMechanicalState) -> Option<PacManArchiveKey> {
    if !state.playing() {
        return None;
    }
    Some(PacManArchiveKey {
        tier: tier(state),
        column: state.x >> GRID_SHIFT,
        row: state.y >> GRID_SHIFT,
        powered: state.empowered(),
        lives: state.lives,
        score: state.score,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManMilestones {
    pub mazes: u8,
    pub dots: u8,
    pub score: u32,
    pub game_over: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManMilestoneTimes {
    pub first_mazes: [Option<u64>; MAZES as usize],
    pub first_game_over: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManMilestoneInputs {
    pub first_mazes: [Option<PacManInput>; MAZES as usize],
    pub first_game_over: Option<PacManInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PacManProgressWatermark {
    pub mazes: u8,
    pub dots: u8,
}

pub type PacManArchiveProgressPoint = ProgressPoint<PacManMilestones, PacManProgressWatermark>;
pub type PacManArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, PacManArchiveKey, PacManMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacManArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: PacManMilestones,
    pub progress_watermark: PacManProgressWatermark,
    pub first_reached: PacManMilestoneTimes,
    pub first_inputs: PacManMilestoneInputs,
    pub champion_input: PacManInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<PacManArchiveEntryReport>,
    pub progress_curve: Vec<PacManArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: PacManMechanicalState) -> PacManMilestones {
    PacManMilestones {
        mazes: state.mazes_cleared(),
        dots: state.dots_eaten(),
        score: state.score,
        game_over: state.game_over(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &PacManObservations) -> PacManMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut PacManMilestones, from: PacManMilestones) {
    if (from.mazes, from.dots) > (into.mazes, into.dots) {
        into.mazes = from.mazes;
        into.dots = from.dots;
    }
    into.score = into.score.max(from.score);
    into.game_over |= from.game_over;
}

#[must_use]
pub fn milestone_key(value: PacManMilestones) -> (u8, u8, u32, bool) {
    (value.mazes, value.dots, value.score, !value.game_over)
}

#[must_use]
pub fn progress_point(state: PacManMechanicalState) -> PacManProgressWatermark {
    PacManProgressWatermark {
        mazes: state.mazes_cleared(),
        dots: state.dots_eaten(),
    }
}

pub fn merge_progress_watermark(
    watermark: &mut PacManProgressWatermark,
    observations: &[PacManObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_point(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

const HELD: [u8; 5] = [0x00, UP, DOWN, LEFT, RIGHT];

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &HELD,
    tap: None,
    short_hold: (2, 16),
    long_hold: (24, 64),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pac_man::target::{DOTS, MODE_DEATH, MODE_PLAY, START_LIVES};
    use crate::search::rand::RomuDuoJrRand;

    fn state(x: u8, y: u8) -> PacManMechanicalState {
        PacManMechanicalState {
            mode: MODE_PLAY,
            lives: START_LIVES,
            dots: DOTS,
            x,
            y,
            ..PacManMechanicalState::default()
        }
    }

    #[test]
    fn the_place_is_the_eight_pixel_cell_and_the_power_state() {
        let here = archive_key(state(0x78, 0x8d)).unwrap();
        let near = archive_key(state(0x7f, 0x8f)).unwrap();
        let right = archive_key(state(0x80, 0x8d)).unwrap();
        let below = archive_key(state(0x78, 0x90)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), below.place());
        let mut powered = state(0x78, 0x8d);
        powered.power = 1;
        assert_ne!(here.place(), archive_key(powered).unwrap().place());
    }

    #[test]
    fn the_tier_is_mazes_cleared_then_dots_eaten() {
        let plain = archive_key(state(0x78, 0x8d)).unwrap();
        let mut eaten = state(0x78, 0x8d);
        eaten.dots = DOTS - 1;
        assert!(archive_key(eaten).unwrap().progress() > plain.progress());
        let mut next = state(0x78, 0x8d);
        next.level = 1;
        assert!(archive_key(next).unwrap().progress() > archive_key(eaten).unwrap().progress());
    }

    #[test]
    fn lives_then_score_are_preferred() {
        let full = archive_key(state(0x78, 0x8d)).unwrap();
        let mut hurt = state(0x78, 0x8d);
        hurt.lives = START_LIVES - 1;
        hurt.score = 5000;
        let hurt = archive_key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x78, 0x8d);
        rich.score = 200;
        assert_eq!(
            archive_key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
    }

    #[test]
    fn death_ready_and_clear_frames_have_no_key() {
        let mut dead = state(0x78, 0x8d);
        dead.mode = MODE_DEATH;
        assert_eq!(archive_key(dead), None);
        let mut ready = state(0x78, 0x8d);
        ready.mode = 0x02;
        assert_eq!(archive_key(ready), None);
        let mut cleared = state(0x78, 0x8d);
        cleared.dots = 0;
        assert_eq!(archive_key(cleared), None);
        assert_eq!(milestones(cleared).mazes, 1);
        let mut over = state(0x78, 0x8d);
        over.mode = 0x0a;
        assert!(milestones(over).game_over);
    }

    #[test]
    fn milestones_keep_the_furthest_maze_and_dot_count() {
        let mut aggregate = PacManMilestones::default();
        let mut first = state(0x78, 0x8d);
        first.level = 1;
        first.dots = 100;
        merge_milestones(&mut aggregate, milestones(first));
        let mut second = state(0x78, 0x8d);
        second.dots = 10;
        merge_milestones(&mut aggregate, milestones(second));
        assert_eq!((aggregate.mazes, aggregate.dots), (1, 92));
    }

    #[test]
    fn vocabulary_draws_single_directions_and_neutral() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert!(HELD.contains(&chord.buttons));
            assert!(chord.hold_frames >= 2);
        }
    }
}
