// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, Tap},
    joust::target::{
        ButtonChord, GRID_SHIFT, JoustInput, JoustMechanicalState, JoustObservations, JoustSnapshot,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str =
    "joust_waves_cleared_then_defeated_tier_stable_wave_grid_32_place_lives_score_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type JoustArchive = Archive<ButtonChord, JoustArchiveKey, JoustMilestones, JoustSnapshot>;

pub const WAVE_MARKS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct JoustTier {
    pub cleared: u8,
    pub defeated: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct JoustArchiveKey {
    pub tier: JoustTier,
    pub wave: u8,
    pub column: u8,
    pub row: u8,
    pub lives: u8,
    pub score: u32,
}

impl ArchiveKey for JoustArchiveKey {
    type Place = (u8, u8, u8);
    type Progress = JoustTier;
    type Identity = ();

    fn place(self) -> Self::Place {
        (self.wave, self.column, self.row)
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
pub fn tier(state: JoustMechanicalState) -> JoustTier {
    JoustTier {
        cleared: state.waves_cleared(),
        defeated: state.defeated,
    }
}

#[must_use]
pub fn archive_key(state: JoustMechanicalState) -> Option<JoustArchiveKey> {
    if state.terminal() || !state.playing() {
        return None;
    }
    Some(JoustArchiveKey {
        tier: tier(state),
        wave: state.wave,
        column: state.x >> GRID_SHIFT,
        row: state.y >> GRID_SHIFT,
        lives: state.lives,
        score: state.score,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustMilestones {
    pub wave: u8,
    pub score: u32,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustMilestoneTimes {
    pub first_waves: [Option<u64>; WAVE_MARKS],
    pub first_victory: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustMilestoneInputs {
    pub first_waves: [Option<JoustInput>; WAVE_MARKS],
    pub first_victory: Option<JoustInput>,
    pub first_death: Option<JoustInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct JoustProgressWatermark {
    pub cleared: u8,
    pub defeated: u8,
}

pub type JoustArchiveProgressPoint = ProgressPoint<JoustMilestones, JoustProgressWatermark>;
pub type JoustArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, JoustArchiveKey, JoustMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoustArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: JoustMilestones,
    pub progress_watermark: JoustProgressWatermark,
    pub first_reached: JoustMilestoneTimes,
    pub first_inputs: JoustMilestoneInputs,
    pub champion_input: JoustInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<JoustArchiveEntryReport>,
    pub progress_curve: Vec<JoustArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: JoustMechanicalState) -> JoustMilestones {
    JoustMilestones {
        wave: state.wave,
        score: state.score,
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &JoustObservations) -> JoustMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut JoustMilestones, from: JoustMilestones) {
    into.wave = into.wave.max(from.wave);
    into.score = into.score.max(from.score);
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: JoustMilestones) -> (bool, u8, u32, bool) {
    (value.victory, value.wave, value.score, !value.death)
}

#[must_use]
pub fn progress_point(state: JoustMechanicalState) -> JoustProgressWatermark {
    JoustProgressWatermark {
        cleared: state.waves_cleared(),
        defeated: state.defeated,
    }
}

pub fn merge_progress_watermark(
    watermark: &mut JoustProgressWatermark,
    observations: &[JoustObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_point(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

const FLAP: u8 = 0x01;
const HELD: [u8; 6] = [0x00, 0x01, 0x40, 0x41, 0x80, 0x81];

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &HELD,
    tap: Some(Tap {
        buttons: FLAP,
        odds: 3,
        hold_frames: (1, 2),
    }),
    short_hold: (1, 10),
    long_hold: (12, 40),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rand::RomuDuoJrRand;

    fn state(x: u8, y: u8) -> JoustMechanicalState {
        JoustMechanicalState {
            wave: 3,
            lives: 4,
            x,
            y,
            in_game: true,
            ..JoustMechanicalState::default()
        }
    }

    #[test]
    fn the_place_is_the_wave_and_a_32_pixel_grid_cell_of_the_rider() {
        let here = archive_key(state(0x50, 0xac)).unwrap();
        let near = archive_key(state(0x5f, 0xbf)).unwrap();
        let right = archive_key(state(0x60, 0xac)).unwrap();
        let above = archive_key(state(0x50, 0x9f)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), above.place());
        let mut later = state(0x50, 0xac);
        later.wave = 4;
        assert_ne!(here.place(), archive_key(later).unwrap().place());
    }

    #[test]
    fn the_tier_is_waves_cleared_then_enemies_defeated_in_the_wave() {
        let plain = archive_key(state(0x50, 0xac)).unwrap();
        let mut hit = state(0x50, 0xac);
        hit.defeated = 2;
        assert!(archive_key(hit).unwrap().progress() > plain.progress());
        let mut next = state(0x50, 0xac);
        next.wave = 4;
        assert!(archive_key(next).unwrap().progress() > archive_key(hit).unwrap().progress());
    }

    #[test]
    fn lives_then_score_are_preferred() {
        let full = archive_key(state(0x50, 0xac)).unwrap();
        let mut hurt = state(0x50, 0xac);
        hurt.lives = 3;
        hurt.score = 9_000;
        let hurt = archive_key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x50, 0xac);
        rich.score = 500;
        assert_eq!(
            archive_key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
    }

    #[test]
    fn game_over_and_menus_have_no_key_and_game_over_is_a_death_milestone() {
        let mut over = state(0x50, 0xac);
        over.game_over = true;
        assert_eq!(archive_key(over), None);
        assert!(milestones(over).death);
        let mut menu = state(0x50, 0xac);
        menu.in_game = false;
        assert_eq!(archive_key(menu), None);
    }

    #[test]
    fn milestones_keep_the_best_wave_and_score() {
        let mut aggregate = JoustMilestones::default();
        let mut first = state(0x50, 0xac);
        first.wave = 5;
        first.score = 3_000;
        merge_milestones(&mut aggregate, milestones(first));
        let mut second = state(0x50, 0xac);
        second.wave = 4;
        second.score = 4_500;
        merge_milestones(&mut aggregate, milestones(second));
        assert_eq!((aggregate.wave, aggregate.score), (5, 4_500));
        assert!(milestone_key(aggregate) > milestone_key(milestones(second)));
    }

    #[test]
    fn vocabulary_draws_steering_masks_and_short_flap_taps() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut taps = 0;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert!(HELD.contains(&chord.buttons));
            if chord.buttons & FLAP != 0 && chord.hold_frames <= 2 {
                taps += 1;
            }
        }
        assert!(taps > 100, "flap taps: {taps}");
    }
}
