// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::ChordVocabulary,
    famidash::target::{
        A_BUTTON, ButtonChord, FamidashInput, FamidashMechanicalState, FamidashObservations,
        FamidashSnapshot,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "famidash_level_band_256_cell_32x16_mode_motion_v2";
pub const REPLACEMENT_IDENTIFIER: &str = "furthest_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_tap_1_4_or_hold_6_40_v1";

const CELL_WIDTH: i32 = 32;
const CELL_HEIGHT: i32 = 16;
const PROGRESS_BAND: i32 = 256;

pub type FamidashArchive =
    Archive<ButtonChord, FamidashArchiveKey, FamidashMilestones, FamidashSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FamidashArchiveKey {
    pub level: u8,
    pub x: i32,
    pub y: i32,
    pub gamemode: u8,
    pub gravity: bool,
    pub mini: bool,
    pub dual: bool,
    pub motion: u8,
}

impl ArchiveKey for FamidashArchiveKey {
    type Place = (u8, i32, i32, u8, bool, bool, bool);
    type Progress = (u8, i32);
    type Identity = u8;

    fn place(self) -> Self::Place {
        (
            self.level,
            self.x.div_euclid(CELL_WIDTH),
            self.y.div_euclid(CELL_HEIGHT),
            self.gamemode,
            self.gravity,
            self.mini,
            self.dual,
        )
    }

    fn progress(self) -> Self::Progress {
        (self.level, self.x.div_euclid(PROGRESS_BAND))
    }

    fn identity(self) -> Self::Identity {
        self.motion
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        self.x.cmp(&other.x)
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

pub type FamidashChampionKey = (u8, i32);

impl FamidashArchiveKey {
    #[must_use]
    pub fn champion_key(self) -> FamidashChampionKey {
        (self.level, self.x)
    }
}

#[must_use]
pub fn archive_key(state: FamidashMechanicalState) -> Option<FamidashArchiveKey> {
    let gameplay = state.gameplay?;
    Some(FamidashArchiveKey {
        level: state.level,
        x: gameplay.x,
        y: gameplay.y,
        gamemode: gameplay.gamemode,
        gravity: gameplay.gravity,
        mini: gameplay.mini,
        dual: gameplay.dual,
        motion: gameplay.vertical_motion(),
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashMilestones {
    pub cleared: u8,
    pub percent: u8,
    pub complete: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashMilestoneTimes {
    pub first_complete: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashMilestoneInputs {
    pub first_complete: Option<FamidashInput>,
    pub first_death: Option<FamidashInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FamidashProgressWatermark {
    pub cleared: u8,
    pub percent: u8,
    pub x: i32,
}

pub type FamidashArchiveProgressPoint =
    ProgressPoint<FamidashMilestones, FamidashProgressWatermark>;
pub type FamidashArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, FamidashArchiveKey, FamidashMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FamidashArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: FamidashMilestones,
    pub progress_watermark: FamidashProgressWatermark,
    pub first_reached: FamidashMilestoneTimes,
    pub first_inputs: FamidashMilestoneInputs,
    pub champion_input: FamidashInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<FamidashArchiveEntryReport>,
    pub progress_curve: Vec<FamidashArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: FamidashMechanicalState) -> FamidashMilestones {
    FamidashMilestones {
        cleared: state.cleared(),
        percent: level_percent(state),
        complete: state.finished(),
        death: state.dead,
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &FamidashObservations) -> FamidashMilestones {
    milestones(observation.decoded)
}

fn level_percent(state: FamidashMechanicalState) -> u8 {
    if state.complete() { 0 } else { state.percent() }
}

pub fn merge_milestones(into: &mut FamidashMilestones, from: FamidashMilestones) {
    if (from.cleared, from.percent) > (into.cleared, into.percent) {
        into.cleared = from.cleared;
        into.percent = from.percent;
    }
    into.complete |= from.complete;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: FamidashMilestones) -> (bool, u8, u8, bool) {
    (value.complete, value.cleared, value.percent, !value.death)
}

pub fn merge_progress_watermark(
    watermark: &mut FamidashProgressWatermark,
    observations: &[FamidashObservations],
) {
    for observation in observations {
        let state = observation.decoded;
        *watermark = (*watermark).max(FamidashProgressWatermark {
            cleared: state.cleared(),
            percent: level_percent(state),
            x: state.gameplay.map_or(0, |gameplay| gameplay.x),
        });
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &[0x00, A_BUTTON],
    tap: None,
    short_hold: (1, 4),
    long_hold: (6, 40),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::famidash::target::{FamidashGameplayState, GAME_STATE_COMPLETE, LAST_LEVEL};
    use crate::search::rand::RomuDuoJrRand;

    fn state(x: i32, y: i32, gamemode: u8) -> FamidashMechanicalState {
        FamidashMechanicalState {
            game_state: 2,
            gameplay: Some(FamidashGameplayState {
                x,
                y,
                scroll_x: x - 80,
                gamemode,
                ..FamidashGameplayState::default()
            }),
            ..FamidashMechanicalState::default()
        }
    }

    #[test]
    fn places_are_32_by_16_pixel_cells_per_level_and_mode() {
        let key = archive_key(state(1_000, 897, 0)).unwrap();
        assert_eq!(key.place(), (0, 31, 56, 0, false, false, false));
        let mut later = state(1_000, 897, 0);
        later.level = 1;
        assert_ne!(archive_key(later).unwrap().place(), key.place());
        assert_ne!(
            archive_key(state(1_000, 897, 1)).unwrap().place(),
            key.place()
        );
        assert_eq!(
            archive_key(state(1_023, 911, 0)).unwrap().place(),
            key.place()
        );
    }

    #[test]
    fn progress_bands_are_256_pixels_of_distance() {
        assert_eq!(archive_key(state(255, 897, 0)).unwrap().progress(), (0, 0));
        assert_eq!(archive_key(state(256, 897, 0)).unwrap().progress(), (0, 1));
        assert_eq!(
            archive_key(state(-160, 897, 0)).unwrap().progress(),
            (0, -1)
        );
    }

    #[test]
    fn rising_falling_and_resting_states_are_distinct_identities() {
        let mut rising = state(500, 850, 0);
        let mut falling = rising;
        rising.gameplay.as_mut().unwrap().velocity_y = -400;
        falling.gameplay.as_mut().unwrap().velocity_y = 400;
        let [rest, up, down] =
            [state(500, 850, 0), rising, falling].map(|state| archive_key(state).unwrap());
        assert_eq!(rest.place(), up.place());
        assert_eq!(up.place(), down.place());
        assert_eq!([rest.identity(), up.identity(), down.identity()], [0, 1, 2]);
    }

    #[test]
    fn the_furthest_state_in_a_cell_is_preferred() {
        let near = archive_key(state(1_000, 897, 0)).unwrap();
        let far = archive_key(state(1_020, 897, 0)).unwrap();
        assert_eq!(near.place(), far.place());
        assert_eq!(far.preference_cmp(0, near), Ordering::Greater);
    }

    #[test]
    fn dead_and_complete_states_have_no_key() {
        let mut dead = state(300, 897, 0);
        dead.dead = true;
        dead.gameplay = None;
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
        let mut complete = state(14_000, 897, 0);
        complete.game_state = GAME_STATE_COMPLETE;
        complete.gameplay = None;
        assert_eq!(archive_key(complete), None);
        assert!(!milestones(complete).complete);
        assert_eq!(
            (milestones(complete).cleared, milestones(complete).percent),
            (1, 0)
        );
        complete.level = LAST_LEVEL;
        assert!(milestones(complete).complete);
        assert_eq!(milestones(complete).cleared, LAST_LEVEL + 1);
    }

    #[test]
    fn a_cleared_level_outranks_any_percent_of_the_level_before() {
        let mut into = milestones(state(14_000, 897, 0));
        let mut next = state(500, 897, 0);
        next.level = 1;
        merge_milestones(&mut into, milestones(next));
        assert_eq!((into.cleared, into.percent), (1, 3));
        merge_milestones(&mut into, milestones(state(14_000, 897, 0)));
        assert_eq!((into.cleared, into.percent), (1, 3));
    }

    #[test]
    fn watermark_orders_by_level_then_percent_then_distance() {
        let mut watermark = FamidashProgressWatermark::default();
        let observation = |state| FamidashObservations {
            frame_count: 1,
            decoded: state,
            terminal: false,
        };
        merge_progress_watermark(&mut watermark, &[observation(state(1_500, 897, 0))]);
        merge_progress_watermark(&mut watermark, &[observation(state(900, 897, 0))]);
        assert_eq!(
            watermark,
            FamidashProgressWatermark {
                cleared: 0,
                percent: 9,
                x: 1_500
            }
        );
        let mut next = state(300, 897, 0);
        next.level = 1;
        merge_progress_watermark(&mut watermark, &[observation(next)]);
        assert_eq!((watermark.cleared, watermark.x), (1, 300));
    }

    #[test]
    fn vocabulary_presses_only_a_with_taps_and_holds() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let (mut taps, mut holds) = (0, 0);
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & !A_BUTTON, 0);
            match chord.bounded_hold_frames() {
                1..=4 => taps += 1,
                6..=40 => holds += 1,
                other => panic!("hold {other} is outside both strata"),
            }
        }
        assert!(taps > 300 && holds > 300);
    }
}
