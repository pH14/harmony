// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    ice_climbers::target::{
        ButtonChord, FLOORS, GRID_SHIFT, IceClimbersInput, IceClimbersMechanicalState,
        IceClimbersObservations, IceClimbersSnapshot, MOUNTAINS,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "ice_climbers_mountain_floor_tier_phase_altitude_band_16_column_32_place_altitude_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";
pub const ALTITUDE_BAND_SHIFT: u16 = 4;

pub type IceClimbersArchive =
    Archive<ButtonChord, IceClimbersArchiveKey, IceClimbersMilestones, IceClimbersSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IceClimbersTier {
    pub level: u8,
    pub floor: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IceClimbersArchiveKey {
    pub tier: IceClimbersTier,
    pub phase: u8,
    pub band: u16,
    pub column: u8,
    pub altitude: u16,
}

impl ArchiveKey for IceClimbersArchiveKey {
    type Place = (u8, u8, u16, u8);
    type Progress = IceClimbersTier;
    type Identity = ();

    fn place(self) -> Self::Place {
        (self.tier.level, self.phase, self.band, self.column)
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
        self.altitude.cmp(&other.altitude)
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn tier(state: IceClimbersMechanicalState) -> IceClimbersTier {
    IceClimbersTier {
        level: state.level,
        floor: state.floor(),
    }
}

#[must_use]
pub fn archive_key(state: IceClimbersMechanicalState) -> Option<IceClimbersArchiveKey> {
    if state.terminal() || state.in_transition() {
        return None;
    }
    Some(IceClimbersArchiveKey {
        tier: tier(state),
        phase: state.phase,
        band: state.altitude() >> ALTITUDE_BAND_SHIFT,
        column: state.x >> GRID_SHIFT,
        altitude: state.altitude(),
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersMilestones {
    pub level: u8,
    pub floor: u8,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersMilestoneTimes {
    pub first_mountains: [Option<u64>; MOUNTAINS as usize],
    pub first_floors: [Option<u64>; FLOORS as usize],
    pub first_victory: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersMilestoneInputs {
    pub first_mountains: [Option<IceClimbersInput>; MOUNTAINS as usize],
    pub first_floors: [Option<IceClimbersInput>; FLOORS as usize],
    pub first_victory: Option<IceClimbersInput>,
    pub first_death: Option<IceClimbersInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IceClimbersProgressWatermark {
    pub level: u8,
    pub floor: u8,
}

pub type IceClimbersArchiveProgressPoint =
    ProgressPoint<IceClimbersMilestones, IceClimbersProgressWatermark>;
pub type IceClimbersArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, IceClimbersArchiveKey, IceClimbersMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IceClimbersArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: IceClimbersMilestones,
    pub progress_watermark: IceClimbersProgressWatermark,
    pub first_reached: IceClimbersMilestoneTimes,
    pub first_inputs: IceClimbersMilestoneInputs,
    pub champion_input: IceClimbersInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<IceClimbersArchiveEntryReport>,
    pub progress_curve: Vec<IceClimbersArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: IceClimbersMechanicalState) -> IceClimbersMilestones {
    IceClimbersMilestones {
        level: state.level,
        floor: if state.dead() { 0 } else { state.floor() },
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &IceClimbersObservations) -> IceClimbersMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut IceClimbersMilestones, from: IceClimbersMilestones) {
    if (from.level, from.floor) > (into.level, into.floor) {
        into.level = from.level;
        into.floor = from.floor;
    }
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: IceClimbersMilestones) -> (bool, u8, u8, bool) {
    (value.victory, value.level, value.floor, !value.death)
}

#[must_use]
pub fn progress_point(state: IceClimbersMechanicalState) -> IceClimbersProgressWatermark {
    IceClimbersProgressWatermark {
        level: state.level,
        floor: state.floor(),
    }
}

pub fn merge_progress_watermark(
    watermark: &mut IceClimbersProgressWatermark,
    observations: &[IceClimbersObservations],
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
    use crate::ice_climbers::target::{START_X, START_Y};
    use crate::search::rand::RomuDuoJrRand;

    fn state(x: u8, y: u8) -> IceClimbersMechanicalState {
        IceClimbersMechanicalState {
            level: 0,
            lives: 3,
            phase: 1,
            x,
            y,
            nametable: 3,
            scroll: 0xcc,
        }
    }

    #[test]
    fn the_place_is_the_altitude_band_and_a_32_pixel_column() {
        let here = archive_key(state(START_X, START_Y)).unwrap();
        let near = archive_key(state(START_X + 3, START_Y - 2)).unwrap();
        let right = archive_key(state(START_X + 32, START_Y)).unwrap();
        let above = archive_key(state(START_X, START_Y - 20)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), above.place());
    }

    #[test]
    fn the_tier_is_the_mountain_then_the_floor() {
        let low = archive_key(state(START_X, START_Y)).unwrap();
        let high = archive_key(state(START_X, START_Y - 50)).unwrap();
        assert!(high.progress() > low.progress());
        let mut next = state(START_X, START_Y);
        next.level = 1;
        assert!(archive_key(next).unwrap().progress() > high.progress());
    }

    #[test]
    fn higher_altitude_is_preferred_within_a_place() {
        let low = archive_key(state(START_X, START_Y)).unwrap();
        let high = archive_key(state(START_X, START_Y - 3)).unwrap();
        assert_eq!(low.place(), high.place());
        assert_eq!(high.preference_cmp(0, low), Ordering::Greater);
    }

    #[test]
    fn a_lost_life_has_no_key_and_counts_as_a_death_milestone() {
        let mut dead = state(START_X, START_Y);
        dead.lives = 2;
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
    }

    #[test]
    fn milestones_keep_the_best_mountain_and_floor() {
        let mut aggregate = IceClimbersMilestones::default();
        merge_milestones(&mut aggregate, milestones(state(START_X, START_Y - 100)));
        assert_eq!((aggregate.level, aggregate.floor), (0, 2));
        let mut next = state(START_X, START_Y);
        next.level = 1;
        merge_milestones(&mut aggregate, milestones(next));
        assert_eq!((aggregate.level, aggregate.floor), (1, 0));
        merge_milestones(&mut aggregate, milestones(state(START_X, START_Y - 100)));
        assert_eq!((aggregate.level, aggregate.floor), (1, 0));
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
