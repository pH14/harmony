// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    nova::target::{
        ButtonChord, NovaInput, NovaMechanicalState, NovaObservations, NovaSnapshot,
        preference_tuple,
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

pub const MAX_NOVA_ACTIONS: usize = 8_192;
pub const KEY_POLICY_IDENTIFIER: &str = "nova_spatial_16_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        NovaArchiveKey::groups().saturating_sub(2),
    )
}

pub type NovaArchive = Archive<ButtonChord, NovaArchiveKey, NovaMilestones, NovaSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaArchiveGroup {
    cleared: u8,
    collectibles: u8,
    available: u8,
    started_level: u8,
    level: u8,
    x: u16,
    y: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaArchiveKey {
    pub cleared: u8,
    pub collectibles: u8,
    pub available: u8,
    pub has_ability: bool,
    pub health: u8,
    pub chips: u8,
    pub started_level: u8,
    pub level: u8,
    pub x: u16,
    pub y: u16,
}

impl ArchiveKey for NovaArchiveKey {
    type Group = NovaArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn group(self, depth: usize) -> Self::Group {
        let location = NovaArchiveGroup {
            started_level: self.started_level,
            level: self.level,
            x: self.x,
            y: self.y,
            ..NovaArchiveGroup::default()
        };
        match depth {
            0 => location,
            1 => NovaArchiveGroup {
                x: self.x / 2,
                y: self.y / 2,
                ..location
            },
            2 => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                x: self.x / 8,
                y: self.y / 8,
                ..location
            },
            3 => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                started_level: self.started_level,
                level: self.level,
                ..NovaArchiveGroup::default()
            },
            _ => NovaArchiveGroup {
                cleared: self.cleared,
                collectibles: self.collectibles,
                available: self.available,
                ..NovaArchiveGroup::default()
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

impl NovaArchiveKey {
    fn preference(self) -> (u8, u8, u8, bool, u8, u8) {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.has_ability,
            self.health,
            self.chips,
        )
    }
}

#[must_use]
pub fn archive_key(state: NovaMechanicalState) -> NovaArchiveKey {
    let (cleared, collectibles, available, has_ability, health, chips) = preference_tuple(state);
    NovaArchiveKey {
        cleared,
        collectibles,
        available,
        has_ability,
        health,
        chips,
        started_level: state.started_level,
        level: state.level,
        x: state.x / 16,
        y: state.y / 16,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestones {
    pub cleared: u8,
    pub available: u8,
    pub collectibles: u8,
    pub acquired_ability: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneTimes {
    pub first_clear: Option<u64>,
    pub first_collectible: Option<u64>,
    pub first_ability: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneInputs {
    pub first_clear: Option<NovaInput>,
    pub first_collectible: Option<NovaInput>,
    pub first_ability: Option<NovaInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaProgressWatermark {
    pub cleared: u8,
    pub collectibles: u8,
    pub available: u8,
    pub started_level: u8,
    pub level: u8,
    pub x: u16,
    pub y: u16,
}

pub type NovaArchiveProgressPoint = ProgressPoint<NovaMilestones, NovaProgressWatermark>;
pub type NovaArchiveEntryReport = ArchiveEntryReport<ButtonChord, NovaArchiveKey, NovaMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: NovaMilestones,
    pub progress_watermark: NovaProgressWatermark,
    pub first_reached: NovaMilestoneTimes,
    pub first_inputs: NovaMilestoneInputs,
    pub champion_input: NovaInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<NovaArchiveEntryReport>,
    pub progress_curve: Vec<NovaArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: NovaMechanicalState) -> NovaMilestones {
    NovaMilestones {
        cleared: state.cleared_count(),
        available: state.available_count(),
        collectibles: state.collectible_count(),
        acquired_ability: state.ability != 0,
    }
}

pub fn merge_milestones(into: &mut NovaMilestones, from: NovaMilestones) {
    into.cleared = into.cleared.max(from.cleared);
    into.available = into.available.max(from.available);
    into.collectibles = into.collectibles.max(from.collectibles);
    into.acquired_ability |= from.acquired_ability;
}

#[must_use]
pub fn milestone_key(value: NovaMilestones) -> (u8, u8, u8, bool) {
    (
        value.cleared,
        value.collectibles,
        value.available,
        value.acquired_ability,
    )
}

pub fn merge_progress_watermark(
    watermark: &mut NovaProgressWatermark,
    observations: &[NovaObservations],
) {
    for observation in observations {
        let state = observation.decoded;
        *watermark = (*watermark).max(NovaProgressWatermark {
            cleared: state.cleared_count(),
            collectibles: state.collectible_count(),
            available: state.available_count(),
            started_level: state.started_level,
            level: state.level,
            x: state.x,
            y: state.y,
        });
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];

pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    let direction = DIRECTIONS
        [rand.below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty Nova direction vocabulary")?)];
    let buttons =
        direction | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty Nova A/B vocabulary")?)];
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

    fn state(x: u16, health: u8, cleared: u8) -> NovaMechanicalState {
        let mut value = NovaMechanicalState {
            x,
            y: 64,
            health,
            ..NovaMechanicalState::default()
        };
        value.levels_cleared[0] = cleared;
        value
    }

    #[test]
    fn one_location_uses_opaque_resources_only_for_preference() {
        let weak = archive_key(state(100, 2, 0));
        let strong = archive_key(state(100, 4, 1));
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(weak.group(1), strong.group(1));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(NovaArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn vocabulary_never_draws_start_or_select_or_conflicting_verticals() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x0c, 0);
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
    }
}
