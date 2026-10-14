// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
    twin_dragons::target::{
        ButtonChord, LEVEL_COUNT, TwinDragonsInput, TwinDragonsMechanicalState,
        TwinDragonsObservations, TwinDragonsSnapshot,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "twin_dragons_level_spatial_32_health_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type TwinDragonsArchive =
    Archive<ButtonChord, TwinDragonsArchiveKey, TwinDragonsMilestones, TwinDragonsSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsArchiveKey {
    pub level: u8,
    pub x: i16,
    pub y: i16,
    pub health: u8,
}

impl Ord for TwinDragonsArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.level, self.x, self.health, self.y).cmp(&(
            other.level,
            other.x,
            other.health,
            other.y,
        ))
    }
}

impl PartialOrd for TwinDragonsArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ArchiveKey for TwinDragonsArchiveKey {
    type Place = (u8, i16, i16);
    type Progress = ();
    type Identity = (u8, i16, i16);

    fn place(self) -> Self::Place {
        (self.level, self.x.div_euclid(2), self.y.div_euclid(2))
    }

    fn progress(self) -> Self::Progress {}

    fn identity(self) -> Self::Identity {
        (self.level, self.x, self.y)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        self.health.cmp(&other.health)
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TwinDragonsChampionKey {
    level: u8,
    x: i16,
    health: u8,
}

impl TwinDragonsArchiveKey {
    pub fn champion_key(self) -> TwinDragonsChampionKey {
        TwinDragonsChampionKey {
            level: self.level,
            x: self.x,
            health: self.health,
        }
    }
}

#[must_use]
pub fn archive_key(state: TwinDragonsMechanicalState) -> Option<TwinDragonsArchiveKey> {
    let gameplay = state.gameplay?;
    Some(TwinDragonsArchiveKey {
        level: state.level,
        x: gameplay.x.div_euclid(16),
        y: gameplay.y.div_euclid(16),
        health: gameplay.health,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsMilestones {
    pub levels_cleared: u8,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsMilestoneTimes {
    pub first_clears: [Option<u64>; LEVEL_COUNT as usize],
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsMilestoneInputs {
    pub first_clears: [Option<TwinDragonsInput>; LEVEL_COUNT as usize],
    pub first_death: Option<TwinDragonsInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TwinDragonsProgressWatermark {
    pub levels_cleared: u8,
    pub x: i16,
}

pub type TwinDragonsArchiveProgressPoint =
    ProgressPoint<TwinDragonsMilestones, TwinDragonsProgressWatermark>;
pub type TwinDragonsArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, TwinDragonsArchiveKey, TwinDragonsMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TwinDragonsArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: TwinDragonsMilestones,
    pub progress_watermark: TwinDragonsProgressWatermark,
    pub first_reached: TwinDragonsMilestoneTimes,
    pub first_inputs: TwinDragonsMilestoneInputs,
    pub champion_input: TwinDragonsInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<TwinDragonsArchiveEntryReport>,
    pub progress_curve: Vec<TwinDragonsArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: TwinDragonsMechanicalState) -> TwinDragonsMilestones {
    TwinDragonsMilestones {
        levels_cleared: state.levels_cleared(),
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &TwinDragonsObservations) -> TwinDragonsMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut TwinDragonsMilestones, from: TwinDragonsMilestones) {
    into.levels_cleared = into.levels_cleared.max(from.levels_cleared);
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: TwinDragonsMilestones) -> (u8, bool, bool) {
    (value.levels_cleared, value.victory, !value.death)
}

#[must_use]
pub fn progress_point(state: TwinDragonsMechanicalState) -> Option<TwinDragonsProgressWatermark> {
    state.gameplay.map(|gameplay| TwinDragonsProgressWatermark {
        levels_cleared: state.levels_cleared(),
        x: gameplay.x,
    })
}

pub fn merge_progress_watermark(
    watermark: &mut TwinDragonsProgressWatermark,
    observations: &[TwinDragonsObservations],
) {
    for observation in observations {
        let state = observation.decoded;
        let point = progress_point(state).unwrap_or(TwinDragonsProgressWatermark {
            levels_cleared: state.levels_cleared(),
            x: 0,
        });
        *watermark = (*watermark).max(point);
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
    use crate::search::rand::RomuDuoJrRand;
    use crate::twin_dragons::target::TwinDragonsGameplayState;

    fn state(level: u8, x: i16, health: u8) -> TwinDragonsMechanicalState {
        TwinDragonsMechanicalState {
            game_state: 1,
            level,
            next_level: level + 1,
            loaded: true,
            gameplay: Some(TwinDragonsGameplayState {
                x,
                y: 175,
                health,
                ..TwinDragonsGameplayState::default()
            }),
            ..TwinDragonsMechanicalState::default()
        }
    }

    #[test]
    fn places_are_32_pixel_cells_within_a_level() {
        for x in 0_i16..=2304 {
            let key = archive_key(state(1, x, 3)).unwrap();
            assert_eq!(key.place(), (1, x.div_euclid(32), 175_i16.div_euclid(32)));
            assert_eq!(key.identity().1, x.div_euclid(16));
        }
        assert_ne!(
            archive_key(state(0, 500, 3)).unwrap().place(),
            archive_key(state(1, 500, 3)).unwrap().place()
        );
    }

    #[test]
    fn health_is_the_same_place_preference() {
        let hurt = archive_key(state(0, 300, 1)).unwrap();
        let healthy = archive_key(state(0, 300, 3)).unwrap();
        assert_eq!(hurt.place(), healthy.place());
        assert_eq!(healthy.preference_cmp(0, hurt), Ordering::Greater);
    }

    #[test]
    fn later_levels_order_above_further_positions() {
        let far = archive_key(state(0, 2200, 3)).unwrap();
        let next = archive_key(state(1, 40, 1)).unwrap();
        assert!(next > far);
    }

    #[test]
    fn dead_and_goal_states_have_no_key() {
        let mut dead = state(0, 300, 3);
        dead.gameplay = None;
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
        let mut goal = state(2, 1500, 3);
        goal.goal = true;
        goal.gameplay = None;
        assert_eq!(archive_key(goal), None);
        assert!(milestones(goal).victory);
        assert_eq!(milestones(goal).levels_cleared, LEVEL_COUNT);
    }

    #[test]
    fn watermark_prefers_cleared_levels_over_distance() {
        let mut watermark = TwinDragonsProgressWatermark::default();
        let observation = |state| TwinDragonsObservations {
            frame_count: 1,
            decoded: state,
            terminal: false,
        };
        merge_progress_watermark(&mut watermark, &[observation(state(0, 2000, 3))]);
        merge_progress_watermark(&mut watermark, &[observation(state(1, 100, 3))]);
        merge_progress_watermark(&mut watermark, &[observation(state(0, 2200, 3))]);
        assert_eq!(
            watermark,
            TwinDragonsProgressWatermark {
                levels_cleared: 1,
                x: 100
            }
        );
    }

    #[test]
    fn vocabulary_excludes_start_and_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
        }
    }
}
