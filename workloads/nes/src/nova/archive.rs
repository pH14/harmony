// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    nova::target::{
        ButtonChord, KEY_COLORS, NovaInput, NovaMechanicalState, NovaObservations, NovaSnapshot,
        preference_tuple,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "nova_in_order_cleared_tiers_level_fight_puzzle_arrow_state_spatial_32_place_ability_identity_key_color_preferences_v13";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type NovaArchive = Archive<ButtonChord, NovaArchiveKey, NovaMilestones, NovaSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaArchiveKey {
    pub cleared: u8,
    pub collectibles: u8,
    pub available: u8,
    pub health: u8,
    pub chips: u8,
    pub started_level: u8,
    pub level: u8,
    pub fight: u8,
    pub keys: [u8; KEY_COLORS],
    pub ability: u8,
    pub sun_key: bool,
    pub carrying_block: bool,
    pub toggle: bool,
    pub arrow_blocks: u16,
    pub x: u16,
    pub y: u16,
}

impl ArchiveKey for NovaArchiveKey {
    type Place = (u8, u8, u8, u8, u8, (bool, bool, bool, u8, u16), u16, u16);
    type Progress = u8;
    type Identity = (u16, u16, u8);

    fn place(self) -> Self::Place {
        (
            self.collectibles,
            self.available,
            self.started_level,
            self.level,
            self.fight,
            (
                self.sun_key,
                self.carrying_block,
                self.toggle,
                self.chips,
                self.arrow_blocks,
            ),
            self.x / 2,
            self.y / 2,
        )
    }

    fn progress(self) -> Self::Progress {
        self.cleared
    }

    fn identity(self) -> Self::Identity {
        (self.x, self.y, self.ability)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1 + KEY_COLORS
    }

    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        match preference.checked_sub(1) {
            Some(color) => (self.keys.get(color), self.preference())
                .cmp(&(other.keys.get(color), other.preference())),
            None => self.preference().cmp(&other.preference()),
        }
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

impl NovaArchiveKey {
    fn preference(self) -> (u8, u8, u8, u8, u8) {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.health,
            self.chips,
        )
    }
}

#[must_use]
pub fn archive_key(state: NovaMechanicalState) -> NovaArchiveKey {
    let (cleared, collectibles, available, _, health, chips) = preference_tuple(state);
    NovaArchiveKey {
        cleared,
        collectibles,
        available,
        health,
        chips,
        started_level: state.started_level,
        level: state.level,
        fight: state.fight,
        keys: state.keys,
        ability: state.ability,
        sun_key: state.sun_key,
        carrying_block: state.carrying_block,
        toggle: state.toggle,
        arrow_blocks: state.arrow_blocks,
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
    pub fight: u8,
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
        cleared: state.cleared_in_order(),
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
            cleared: state.cleared_in_order(),
            collectibles: state.collectible_count(),
            available: state.available_count(),
            started_level: state.started_level,
            level: state.level,
            fight: state.fight,
            x: state.x,
            y: state.y,
        });
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
        let strong = archive_key(state(100, 4, 0));
        assert_eq!(weak.place(), strong.place());
        assert_eq!(weak.identity(), strong.identity());
        assert_eq!(strong.preference_cmp(0, weak), Ordering::Greater);
        let cleared = archive_key(state(100, 4, 1));
        assert_eq!((strong.progress(), cleared.progress()), (0, 1));
        assert_eq!(cleared.preference_cmp(0, strong), Ordering::Greater);
        assert_eq!(NovaArchiveKey::capacity(), 1);
    }

    #[test]
    fn each_fight_count_is_its_own_place() {
        let before = archive_key(state(100, 4, 0));
        let mut hit = state(100, 4, 0);
        hit.fight = 1;
        let after = archive_key(hit);
        assert_ne!(after.place(), before.place());
        assert_eq!(after.identity(), before.identity());
    }

    #[test]
    fn puzzle_state_is_its_own_place() {
        let base = archive_key(state(100, 4, 0));
        let mut sun = state(100, 4, 0);
        sun.sun_key = true;
        let mut carrying = state(100, 4, 0);
        carrying.carrying_block = true;
        let mut toggled = state(100, 4, 0);
        toggled.toggle = true;
        let mut chipped = state(100, 4, 0);
        chipped.chips = 1;
        let mut spent_arrow = state(100, 4, 0);
        spent_arrow.arrow_blocks = 1;
        let places = [
            base,
            archive_key(sun),
            archive_key(carrying),
            archive_key(toggled),
            archive_key(chipped),
            archive_key(spent_arrow),
        ]
        .map(|key| key.place());
        for (index, place) in places.iter().enumerate() {
            assert!(!places[index + 1..].contains(place));
        }
    }

    #[test]
    fn each_held_ability_keeps_its_own_holder_at_a_place() {
        let none = archive_key(state(100, 4, 0));
        let mut nice = state(100, 4, 0);
        nice.ability = 6;
        let mut burger = state(100, 3, 0);
        burger.ability = 11;
        let keys = [none, archive_key(nice), archive_key(burger)];
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(key.place(), none.place());
            assert_eq!(key.progress(), none.progress());
            assert!(
                keys[index + 1..]
                    .iter()
                    .all(|other| other.identity() != key.identity())
            );
        }
    }

    #[test]
    fn each_key_color_keeps_its_largest_count_at_a_place() {
        let none = archive_key(state(100, 4, 0));
        let mut one = state(100, 4, 0);
        one.keys = [0, 1, 0];
        let one = archive_key(one);
        assert_eq!(none.place(), one.place());
        assert_eq!(none.preference_cmp(0, one), Ordering::Equal);
        assert_eq!(one.preference_cmp(2, none), Ordering::Greater);
        let mut healthy = state(100, 4, 0);
        healthy.keys = [5, 0, 0];
        let mut hurt = state(100, 3, 0);
        hurt.keys = [6, 0, 0];
        let (healthy, hurt) = (archive_key(healthy), archive_key(hurt));
        assert_eq!(healthy.place(), hurt.place());
        assert_eq!(healthy.preference_cmp(0, hurt), Ordering::Greater);
        assert_eq!(hurt.preference_cmp(1, healthy), Ordering::Greater);
        let mut red = state(100, 4, 0);
        red.keys = [2, 1, 0];
        let mut green = state(100, 4, 0);
        green.keys = [1, 3, 0];
        let (red, green) = (archive_key(red), archive_key(green));
        assert_eq!(red.place(), green.place());
        assert_eq!(red.preference_cmp(1, green), Ordering::Greater);
        assert_eq!(green.preference_cmp(2, red), Ordering::Greater);
        assert_eq!(NovaArchiveKey::preferences(), 4);
    }

    #[test]
    fn vocabulary_never_draws_start_or_select_or_conflicting_verticals() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS
                .draw(&mut rand, previous.as_ref())
                .expect("draw chord");
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
    }
}
