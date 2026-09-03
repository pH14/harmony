// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova-owned archive keys, state preferences, and report shapes.

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

/// Largest bounded input horizon accepted by a Nova campaign.
pub const MAX_NOVA_ACTIONS: usize = 8_192;
/// Recorded archive-key and per-location preference policy.
pub const KEY_POLICY_IDENTIFIER: &str = "nova_spatial_16_preference_v1";
/// Recorded same-slot replacement policy.
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
/// Recorded controller hold distribution.
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

/// The parent selector named by a stream, resolved under Nova's group depths.
pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        NovaArchiveKey::groups().saturating_sub(2),
    )
}

/// Nova's archive instantiation.
pub type NovaArchive = Archive<ButtonChord, NovaArchiveKey, NovaMilestones, NovaSnapshot>;

/// Opaque pooled identity returned to the generic selector.
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

/// Quality-diversity key for one Nova endpoint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaArchiveKey {
    /// Durable completed-level count.
    pub cleared: u8,
    /// Durable collectible count.
    pub collectibles: u8,
    /// Unlocked-level count.
    pub available: u8,
    /// Whether an ability is carried.
    pub has_ability: bool,
    /// Current health.
    pub health: u8,
    /// Current puzzle-chip count.
    pub chips: u8,
    /// Selected campaign level.
    pub started_level: u8,
    /// Internal map number.
    pub level: u8,
    /// Player horizontal 16-pixel bucket.
    pub x: u16,
    /// Player vertical 16-pixel bucket.
    pub y: u16,
}

impl ArchiveKey for NovaArchiveKey {
    type Group = NovaArchiveGroup;

    fn groups() -> usize {
        5
    }

    /// Depth 0 is one 16-pixel location, depth 1 one 32-pixel selection cell,
    /// depth 2 a durable-progress 128-pixel region, depth 3 its level, and
    /// depth 4 durable progress alone. Resource fields never multiply slots;
    /// they decide which one representative remains at a location.
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

/// Build the opaque archive key from a decoded Nova state.
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

/// Strongest durable and mechanical rungs observed by a campaign.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestones {
    /// Greatest completed-level count.
    pub cleared: u8,
    /// Greatest unlocked-level count.
    pub available: u8,
    /// Greatest durable collectible count.
    pub collectibles: u8,
    /// Whether any input acquired an ability.
    pub acquired_ability: bool,
}

/// First deterministic execution reaching each durable rung.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneTimes {
    /// First execution that cleared a level.
    pub first_clear: Option<u64>,
    /// First execution that acquired a collectible.
    pub first_collectible: Option<u64>,
    /// First execution that acquired an ability.
    pub first_ability: Option<u64>,
}

/// First clean-reset input reaching each durable rung.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaMilestoneInputs {
    /// First input that cleared a level.
    pub first_clear: Option<NovaInput>,
    /// First input that acquired a collectible.
    pub first_collectible: Option<NovaInput>,
    /// First input that acquired an ability.
    pub first_ability: Option<NovaInput>,
}

/// Strongest lexicographic mechanical position seen at any emulated frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaProgressWatermark {
    /// Durable completed-level count.
    pub cleared: u8,
    /// Durable collectible count.
    pub collectibles: u8,
    /// Unlocked-level count.
    pub available: u8,
    /// Selected campaign level.
    pub started_level: u8,
    /// Internal map number.
    pub level: u8,
    /// Whole-pixel X position.
    pub x: u16,
    /// Whole-pixel Y position.
    pub y: u16,
}

/// Nova progress curve point.
pub type NovaArchiveProgressPoint = ProgressPoint<NovaMilestones, NovaProgressWatermark>;
/// Nova archive entry report.
pub type NovaArchiveEntryReport = ArchiveEntryReport<ButtonChord, NovaArchiveKey, NovaMilestones>;

/// Complete deterministic report for one Nova campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaArchiveReport {
    /// Campaign seed.
    pub seed: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Strongest durable milestones.
    pub milestones: NovaMilestones,
    /// Strongest per-frame mechanical progress.
    pub progress_watermark: NovaProgressWatermark,
    /// First execution reaching each durable rung.
    pub first_reached: NovaMilestoneTimes,
    /// First input reaching each durable rung.
    pub first_inputs: NovaMilestoneInputs,
    /// Best input under Nova's opaque progress/preference order.
    pub champion_input: NovaInput,
    /// Retained per-location representatives.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<NovaArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<NovaArchiveProgressPoint>,
    /// Candidates admitted.
    pub retained: u64,
    /// Candidates rejected or superseded.
    pub rejected: u64,
    /// Terminal deaths observed.
    pub deaths: u64,
    /// Generic selector accounting.
    #[serde(default)]
    pub selector: SelectorAccounting,
}

/// Decode milestones from one state.
#[must_use]
pub fn milestones(state: NovaMechanicalState) -> NovaMilestones {
    NovaMilestones {
        cleared: state.cleared_count(),
        available: state.available_count(),
        collectibles: state.collectible_count(),
        acquired_ability: state.ability != 0,
    }
}

/// Merge strongest milestone fields.
pub fn merge_milestones(into: &mut NovaMilestones, from: NovaMilestones) {
    into.cleared = into.cleared.max(from.cleared);
    into.available = into.available.max(from.available);
    into.collectibles = into.collectibles.max(from.collectibles);
    into.acquired_ability |= from.acquired_ability;
}

/// Stable champion order owned by the Nova adapter.
#[must_use]
pub fn milestone_key(value: NovaMilestones) -> (u8, u8, u8, bool) {
    (
        value.cleared,
        value.collectibles,
        value.available,
        value.acquired_ability,
    )
}

/// Fold every action-interior observation into the progress watermark.
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

/// Held-frame clock used by same-slot route replacement.
pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

/// Longest hold [`sample_chord`] can draw; the suffix time bound is a
/// multiple of it.
pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];

/// Draw one game-neutral controller chord from Nova's recorded vocabulary.
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
